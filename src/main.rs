use clap::Parser;
use regex::Regex;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
//  CLI
// ---------------------------------------------------------------------------
#[derive(Parser, Debug)]
#[command(name = "detree3", version, about)]
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
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:06x}", hasher.finish() & 0xFFFF_FFFF)
}

fn smart_truncate(base: &str, max_length: usize) -> String {
    if base.len() <= max_length {
        return base.to_string();
    }
    if max_length <= 3 {
        return base.chars().take(max_length).collect();
    }
    let cut = &base[..max_length - 3];
    if let Some(last_space) = cut.rfind(' ') {
        if last_space > max_length / 2 {
            return format!("{}...", &base[..last_space]);
        }
    }
    format!("{}...", cut)
}

fn clean_suffix(base: &str) -> String {
    if base.len() > 5 {
        let prefix = &base[..base.len() - 5];
        let suffix = &base[base.len() - 5..];
        let cleaned_suffix = suffix.replace([' ', '.', '_'], "");
        format!("{}{}", prefix, cleaned_suffix)
    } else {
        base.replace([' ', '.'], "")
    }
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

    format!("{}{}", base, ext)
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
            format!("{}{}", sanitize_name(cleaners, base, no_digits), ext)
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
fn main() {
    let cli = Cli::parse();
    let cleaners = Cleaners::new();

    // Validation
    if !cli.input_file.exists() {
        eprintln!("Error: '{}' not found.", cli.input_file.display());
        std::process::exit(1);
    }
    if !cli.input_file.is_file() {
        eprintln!("Error: '{}' is not a file.", cli.input_file.display());
        std::process::exit(1);
    }
    let meta = fs::metadata(&cli.input_file).unwrap();
    if meta.len() == 0 {
        eprintln!("Error: '{}' is empty.", cli.input_file.display());
        std::process::exit(1);
    }

    // Read
    let file = File::open(&cli.input_file).unwrap_or_else(|e| {
        eprintln!("Error reading file: {}", e);
        std::process::exit(1);
    });
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map(|l| l.unwrap()).collect();
    let content = lines.join("\n");

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
                serde_json::from_str(&content).unwrap_or_else(|e| {
                    eprintln!("Error: invalid JSON — {}", e);
                    std::process::exit(1);
                })
            } else {
                let obj: serde_json::Value = serde_json::from_str(&content).unwrap_or_else(|e| {
                    eprintln!("Error: invalid JSON — {}", e);
                    std::process::exit(1);
                });
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

    fs::create_dir_all(&cli.output_dir).unwrap();
    build_tree(
        &arena,
        0,
        &cli.output_dir,
        &cleaners,
        cli.remove_digits,
        cli.allow_empty_folders,
    )
    .unwrap_or_else(|e| {
        eprintln!("Error during build: {}", e);
        std::process::exit(1);
    });

    println!("Done.");
}
