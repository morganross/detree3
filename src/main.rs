use clap::Parser;
use regex::Regex;
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
//  CLI
// ---------------------------------------------------------------------------
#[derive(Parser, Debug)]
#[command(name = "detree", version, about)]
struct Cli {
    #[arg(help = "Input file (text list or JSON)")]
    input_file: PathBuf,

    #[arg(help = "Output directory")]
    output_dir: PathBuf,

    #[arg(long, value_enum, default_value = "auto")]
    format: Format,

    #[arg(long)]
    remove_digits: bool,

    #[arg(long)]
    allow_empty_folders: bool,
}

#[derive(Clone, Debug, Default, clap::ValueEnum)]
enum Format {
    #[default]
    Auto,
    Text,
    Json,
}

// ---------------------------------------------------------------------------
//  Regexes (compiled once, lazy_static not needed: Regex::new is cheap enough
//  and we only create them once at startup)
// ---------------------------------------------------------------------------
struct Cleaners {
    bad_chars: Regex,
    lead_list: Regex,
    whitespace: Regex,
    dots: Regex,
    lead_digits: Regex,
}

impl Cleaners {
    fn new() -> Self {
        Self {
            bad_chars: Regex::new(r#"[<>:"/\\|?*!%@#$~`^\[\]{}]"#).unwrap(),
            lead_list: Regex::new(r"^[.\-]+\s*").unwrap(),
            whitespace: Regex::new(r"\s+").unwrap(),
            dots: Regex::new(r"\.+").unwrap(),
            lead_digits: Regex::new(r"^[\d\s]+").unwrap(),
        }
    }
}

// ---------------------------------------------------------------------------
//  Helpers
// ---------------------------------------------------------------------------
fn uid(s: &str) -> String {
    // Stable FNV-1a keeps duplicate suffixes deterministic across Rust releases.
    let mut hash = 0x811c_9dc5_u32;
    for byte in s.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

fn smart_truncate(base: &str, max_length: usize) -> String {
    let chars: Vec<char> = base.chars().collect();
    if chars.len() <= max_length {
        return base.to_string();
    }
    if max_length <= 3 {
        return chars.into_iter().take(max_length).collect();
    }
    let cut: String = chars[..max_length - 3].iter().collect();
    if let Some(last_space) = cut.rfind(' ') {
        if last_space > max_length / 2 {
            return format!("{}...", &cut[..last_space]);
        }
    }
    format!("{}...", cut)
}

fn clean_suffix(base: &str) -> String {
    let chars: Vec<char> = base.chars().collect();
    if chars.len() > 5 {
        let split = chars.len() - 5;
        let prefix: String = chars[..split].iter().collect();
        let suffix: String = chars[split..]
            .iter()
            .filter(|ch| !matches!(ch, ' ' | '.' | '_'))
            .collect();
        format!("{prefix}{suffix}")
    } else {
        base.replace([' ', '.'], "")
    }
}

fn sanitize_extension(ext: &str) -> String {
    let Some(body) = ext.strip_prefix('.') else {
        return String::new();
    };
    if body.is_empty()
        || body.len() > 16
        || !body.chars().all(|ch| ch.is_ascii_alphanumeric())
    {
        return String::new();
    }
    format!(".{body}")
}

fn is_windows_reserved(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit()
            && upper.as_bytes()[3] != b'0')
}

fn sanitize_name(cleaners: &Cleaners, name: &str, no_digits: bool) -> String {
    let mut s = cleaners.bad_chars.replace_all(name, "").into_owned();

    if no_digits {
        s = cleaners.lead_digits.replace(&s, "").into_owned();
        s = cleaners.dots.replace_all(&s, "").into_owned();
    } else {
        s = cleaners.lead_list.replace(&s, "").into_owned();
        s = cleaners.dots.replace_all(&s, "_").into_owned();
    }

    s = cleaners.whitespace.replace_all(&s, " ").trim().to_string();

    let max_len = 20usize;
    let (base, ext) = split_extension(&s);
    let mut base = smart_truncate(base, max_len);
    base = base.trim_end_matches([' ', '.']).to_string();
    base = clean_suffix(&base);
    base = base.replace('.', "");
    if base.is_empty() {
        base = "untitled".to_string();
    }
    if is_windows_reserved(&base) {
        base.push('_');
    }

    format!("{}{}", base, sanitize_extension(ext))
}

/// Like std::path::Path::extension but preserves the dot and handles edge cases.
fn split_extension(name: &str) -> (&str, &str) {
    if let Some(pos) = name.rfind('.') {
        // don’t treat hidden files like ".gitignore" as having an extension
        if pos > 0 {
            return (&name[..pos], &name[pos..]);
        }
    }
    (name, "")
}

fn write_file(path: &Path, lines: &[String], title: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = File::create(path)?;
    let escaped = title.replace('"', "\\\"");
    writeln!(f, "---")?;
    writeln!(f, r#"title: "{}""#, escaped)?;
    writeln!(f, "---\n")?;
    for line in lines {
        writeln!(f, "{}", line.replace("**", ""))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
//  JSON data model
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
struct JsonItem {
    name: String,
    #[serde(default)]
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    children: Vec<JsonItem>,
}

// ---------------------------------------------------------------------------
//  Internal tree node (arena-based: children store indices into the arena)
// ---------------------------------------------------------------------------
struct Node {
    content: String,
    full_line: String,
    children: Vec<usize>,
    body_lines: Vec<String>,
    unique_id: String,
    indent_level: usize,
}

type Arena = Vec<Node>;

// ---------------------------------------------------------------------------
//  Parsers
// ---------------------------------------------------------------------------
fn parse_json(data: Vec<JsonItem>, cleaners: &Cleaners, no_digits: bool) -> Arena {
    let mut arena: Arena = Vec::new();
    arena.push(Node {
        content: String::new(),
        full_line: String::new(),
        children: Vec::new(),
        body_lines: Vec::new(),
        unique_id: String::from("root"),
        indent_level: 0,
    });

    fn walk(
        item: &JsonItem,
        parent_idx: usize,
        arena: &mut Arena,
        cleaners: &Cleaners,
        no_digits: bool,
    ) {
        let idx = arena.len();
        arena.push(Node {
            content: sanitize_name(cleaners, &item.name, no_digits),
            full_line: item.name.clone(),
            children: Vec::new(),
            body_lines: item.body.lines().map(String::from).collect(),
            unique_id: uid(&item.name),
            indent_level: 0,
        });
        arena[parent_idx].children.push(idx);

        if item.item_type == "directory" {
            for child in &item.children {
                walk(child, idx, arena, cleaners, no_digits);
            }
        }
    }

    for item in &data {
        walk(item, 0, &mut arena, cleaners, no_digits);
    }
    arena
}

fn parse_text(lines: &[String], cleaners: &Cleaners) -> Arena {
    let mut arena: Arena = Vec::new();
    arena.push(Node {
        content: String::new(),
        full_line: String::new(),
        children: Vec::new(),
        body_lines: Vec::new(),
        unique_id: String::from("root"),
        indent_level: 0,
    });
    let mut stack: Vec<usize> = vec![0];

    for (i, raw) in lines.iter().enumerate() {
        let line_num = i + 1;
        let trimmed = raw.trim_start();

        if line_num <= 3 && trimmed.starts_with("title:") {
            continue;
        }

        let indent = raw.len() - trimmed.len();
        let clean = sanitize_name(cleaners, trimmed, false);
        let content = clean.trim();

        if content.is_empty() {
            continue;
        }

        if raw.contains("**") {
            let parent_idx = *stack.last().unwrap();
            arena[parent_idx].body_lines.push(raw.clone());
            continue;
        }

        let idx = arena.len();
        arena.push(Node {
            content: content.to_string(),
            full_line: raw.clone(),
            children: Vec::new(),
            body_lines: Vec::new(),
            unique_id: uid(&format!("{}_{}_{}", indent, content, line_num)),
            indent_level: indent,
        });

        while stack.len() > 1 && arena[*stack.last().unwrap()].indent_level >= indent {
            stack.pop();
        }

        let parent_idx = *stack.last().unwrap();
        arena[parent_idx].children.push(idx);
        stack.push(idx);
    }

    arena
}

// ---------------------------------------------------------------------------
//  Build
// ---------------------------------------------------------------------------
fn build_tree(
    arena: &Arena,
    node_idx: usize,
    parent_path: &Path,
    cleaners: &Cleaners,
    no_digits: bool,
    allow_empty: bool,
) -> io::Result<()> {
    let node = &arena[node_idx];
    for &child_idx in &node.children {
        let child = &arena[child_idx];
        let full = child.full_line.trim();

        let safe = if full.starts_with('"') && full.ends_with('"') {
            let inner = &full[1..full.len() - 1];
            let (base, ext) = split_extension(inner);
            format!(
                "{}{}",
                sanitize_name(cleaners, base, no_digits),
                sanitize_extension(ext)
            )
        } else {
            sanitize_name(cleaners, &child.content, no_digits)
        };

        let (base, ext) = split_extension(&safe);
        let ext = if ext.is_empty() { ".md" } else { ext };

        let is_dir = !child.children.is_empty() || allow_empty;

        let (target_dir, md_path) = if is_dir {
            let mut dir = parent_path.join(&safe);
            if dir.exists() {
                let uniq = format!("{}_{}", safe, &child.unique_id[..6]);
                dir = parent_path.join(&uniq);
            }
            fs::create_dir_all(&dir)?;
            let md = dir.join("index.md");
            (dir, md)
        } else {
            let mut md = parent_path.join(format!("{}{}", base, ext));
            if md.exists() {
                let uniq = format!("{}_{}{}", base, &child.unique_id[..6], ext);
                md = parent_path.join(&uniq);
            }
            (parent_path.to_path_buf(), md)
        };

        write_file(&md_path, &child.body_lines, full)?;
        build_tree(arena, child_idx, &target_dir, cleaners, no_digits, allow_empty)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
//  Main
// ---------------------------------------------------------------------------
fn run(cli: Cli) -> Result<(), String> {
    let cleaners = Cleaners::new();

    if !cli.input_file.exists() {
        return Err(format!("'{}' not found", cli.input_file.display()));
    }
    if !cli.input_file.is_file() {
        return Err(format!("'{}' is not a file", cli.input_file.display()));
    }
    let content = fs::read_to_string(&cli.input_file)
        .map_err(|error| format!("could not read '{}': {error}", cli.input_file.display()))?;
    if content.is_empty() {
        return Err(format!("'{}' is empty", cli.input_file.display()));
    }
    let lines: Vec<String> = content.lines().map(str::to_owned).collect();

    // Detect format
    let fmt = match cli.format {
        Format::Auto => {
            let first_non_empty = lines.iter().find(|l| !l.trim().is_empty());
            if let Some(line) = first_non_empty {
                if line.trim_start().starts_with('{') || line.trim_start().starts_with('[') {
                    Format::Json
                } else {
                    Format::Text
                }
            } else {
                Format::Text
            }
        }
        other => other,
    };

    // Parse
    let arena = match fmt {
        Format::Json => {
            let json_data: Vec<JsonItem> = if content.trim_start().starts_with('[') {
                serde_json::from_str(&content)
                    .map_err(|error| format!("invalid JSON: {error}"))?
            } else {
                let obj: serde_json::Value = serde_json::from_str(&content)
                    .map_err(|error| format!("invalid JSON: {error}"))?;
                let children = obj.get("children").and_then(|v| v.as_array());
                children
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| serde_json::from_value(v.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            parse_json(json_data, &cleaners, cli.remove_digits)
        }
        Format::Text => parse_text(&lines, &cleaners),
        Format::Auto => unreachable!(),
    };

    fs::create_dir_all(&cli.output_dir).map_err(|error| {
        format!(
            "could not create output directory '{}': {error}",
            cli.output_dir.display()
        )
    })?;
    build_tree(
        &arena,
        0,
        &cli.output_dir,
        &cleaners,
        cli.remove_digits,
        cli.allow_empty_folders,
    )
    .map_err(|error| format!("could not build output: {error}"))?;

    println!("Done.");
    Ok(())
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock must be after the Unix epoch")
                .as_nanos();
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "detree-test-{}-{stamp}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("test directory should be created");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sanitizes_names_portably() {
        let cleaners = Cleaners::new();
        assert_eq!(sanitize_name(&cleaners, "File@With#Chars", false), "FileWithChars");
        assert_eq!(sanitize_name(&cleaners, "123 Report", true), "Report");
        assert_eq!(sanitize_name(&cleaners, "CON", false), "CON_");
        assert_eq!(sanitize_name(&cleaners, "***", false), "untitled");
    }

    #[test]
    fn truncation_is_unicode_safe() {
        let value = "Documentation 🦀 with Unicode characters";
        let truncated = smart_truncate(value, 20);
        assert!(truncated.chars().count() <= 20);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn rejects_unsafe_extensions() {
        assert_eq!(sanitize_extension(".md"), ".md");
        assert_eq!(sanitize_extension(".txt/child"), "");
        assert_eq!(sanitize_extension("../../escape"), "");
    }

    #[test]
    fn parses_nested_text_and_body_content() {
        let cleaners = Cleaners::new();
        let lines = vec![
            "Root".to_string(),
            "  Child".to_string(),
            "  **Body text**".to_string(),
        ];
        let arena = parse_text(&lines, &cleaners);
        assert_eq!(arena[0].children.len(), 1);
        let root = arena[0].children[0];
        assert_eq!(arena[root].children.len(), 1);
        let child = arena[root].children[0];
        assert_eq!(arena[child].body_lines, vec!["  **Body text**"]);
    }

    #[test]
    fn builds_expected_markdown_tree() {
        let cleaners = Cleaners::new();
        let lines = vec![
            "Documentation".to_string(),
            "  Quick Start".to_string(),
            "  **Install and run.**".to_string(),
        ];
        let arena = parse_text(&lines, &cleaners);
        let output = TestDir::new();
        build_tree(&arena, 0, &output.0, &cleaners, false, false)
            .expect("tree should be generated");

        let index = output.0.join("Documentation").join("index.md");
        let child = output.0.join("Documentation").join("Quick Start.md");
        assert!(index.is_file());
        assert!(child.is_file());
        let body = fs::read_to_string(child).expect("child output should be readable");
        assert!(body.contains("title: \"Quick Start\""));
        assert!(body.contains("Install and run."));
        assert!(!body.contains("**"));
    }
}
