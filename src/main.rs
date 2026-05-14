use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
//  CLI (manual parsing — zero dependencies)
// ---------------------------------------------------------------------------
struct Args {
    input_file: PathBuf,
    output_dir: PathBuf,
    format: Format,
    remove_digits: bool,
    allow_empty_folders: bool,
}

#[derive(Clone, Debug)]
enum Format {
    Auto,
    Text,
    Json,
}

fn parse_args() -> Option<Args> {
    let mut args = std::env::args().skip(1).peekable();
    let mut input_file = None;
    let mut output_dir = None;
    let mut format = Format::Auto;
    let mut remove_digits = false;
    let mut allow_empty_folders = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--format" => {
                format = match args.next()?.as_str() {
                    "auto" => Format::Auto,
                    "text" => Format::Text,
                    "json" => Format::Json,
                    _ => return None,
                };
            }
            "--remove-digits" => remove_digits = true,
            "--allow-empty-folders" => allow_empty_folders = true,
            "--help" | "-h" => {
                println!("detree3 <input_file> <output_dir> [OPTIONS]");
                println!("Options:");
                println!("  --format <auto|text|json>   Input format (default: auto)");
                println!("  --remove-digits             Remove leading digits from names");
                println!("  --allow-empty-folders       Create folders for leaf nodes");
                std::process::exit(0);
            }
            _ if arg.starts_with('-') => {
                eprintln!("Unknown flag: {}", arg);
                return None;
            }
            _ if input_file.is_none() => input_file = Some(PathBuf::from(arg)),
            _ if output_dir.is_none() => output_dir = Some(PathBuf::from(arg)),
            _ => {
                eprintln!("Unexpected argument: {}", arg);
                return None;
            }
        }
    }

    Some(Args {
        input_file: input_file?,
        output_dir: output_dir?,
        format,
        remove_digits,
        allow_empty_folders,
    })
}

// ---------------------------------------------------------------------------
//  Sanitization (no regex — pure string ops, faster)
// ---------------------------------------------------------------------------
const BAD_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*', '!', '%', '@', '#', '$', '~', '`', '^', '[', ']', '{', '}'];

fn uid(s: &str) -> String {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:06x}", hasher.finish() & 0xFFFF_FFFF)
}

fn smart_truncate(base: &str, max_length: usize) -> String {
    if base.len() <= max_length { return base.to_string(); }
    if max_length <= 3 { return base.chars().take(max_length).collect(); }
    let cut = &base[..max_length - 3];
    if let Some(last_space) = cut.rfind(' ') {
        if last_space > max_length / 2 { return format!("{}...", &base[..last_space]); }
    }
    format!("{}...", cut)
}

fn clean_suffix(base: &str) -> String {
    if base.len() > 5 {
        let prefix = &base[..base.len() - 5];
        let suffix = base[base.len() - 5..].replace([' ', '.', '_'], "");
        format!("{}{}", prefix, suffix)
    } else {
        base.replace([' ', '.'], "")
    }
}

fn sanitize_name(name: &str, no_digits: bool) -> String {
    let mut s: String = name.chars().filter(|c| !BAD_CHARS.contains(c)).collect();

    if no_digits {
        s = s.trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace()).to_string();
        s = s.replace('.', "");
    } else {
        s = s.trim_start_matches(|c: char| c == '.' || c == '-').to_string();
        s = s.replace('.', "_");
    }

    // Normalize whitespace
    let mut result = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space { result.push(' '); prev_space = true; }
        } else {
            result.push(c);
            prev_space = false;
        }
    }
    s = result.trim().to_string();

    let (base, ext) = split_extension(&s);
    let mut base = smart_truncate(base, 20);
    base = base.trim_end_matches([' ', '.']).to_string();
    base = clean_suffix(&base);
    base = base.replace('.', "");
    format!("{}{}", base, ext)
}

fn split_extension(name: &str) -> (&str, &str) {
    if let Some(pos) = name.rfind('.') {
        if pos > 0 { return (&name[..pos], &name[pos..]); }
    }
    (name, "")
}

fn write_file(path: &Path, lines: &[String], title: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let mut f = File::create(path)?;
    let escaped = title.replace('"', "\\\"");
    writeln!(f, "---")?;
    writeln!(f, r#"title: "{}""#, escaped)?;
    writeln!(f, "---\n")?;
    for line in lines { writeln!(f, "{}", line.replace("**", ""))?; }
    Ok(())
}

// ---------------------------------------------------------------------------
//  Arena-based tree
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
//  JSON parser (manual, no serde derive)
// ---------------------------------------------------------------------------
fn parse_json(data: &Value, no_digits: bool) -> Arena {
    let mut arena: Arena = Vec::new();
    arena.push(Node {
        content: String::new(),
        full_line: String::new(),
        children: Vec::new(),
        body_lines: Vec::new(),
        unique_id: String::from("root"),
        indent_level: 0,
    });

    fn walk(item: &Value, parent_idx: usize, arena: &mut Arena, no_digits: bool) {
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("Unnamed");
        let idx = arena.len();
        arena.push(Node {
            content: sanitize_name(name, no_digits),
            full_line: name.to_string(),
            children: Vec::new(),
            body_lines: item.get("body").and_then(|v| v.as_str()).unwrap_or("").lines().map(String::from).collect(),
            unique_id: uid(name),
            indent_level: 0,
        });
        arena[parent_idx].children.push(idx);

        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("file");
        if item_type == "directory" {
            if let Some(children) = item.get("children").and_then(|v| v.as_array()) {
                for child in children { walk(child, idx, arena, no_digits); }
            }
        }
    }

    let items = if let Some(arr) = data.as_array() {
        arr.as_slice()
    } else if let Some(obj) = data.as_object() {
        obj.get("children").and_then(|v| v.as_array()).map(|v| v.as_slice()).unwrap_or(&[])
    } else {
        &[]
    };

    for item in items { walk(item, 0, &mut arena, no_digits); }
    arena
}

// ---------------------------------------------------------------------------
//  Text parser
// ---------------------------------------------------------------------------
fn parse_text(lines: &[String]) -> Arena {
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

        if line_num <= 3 && trimmed.starts_with("title:") { continue; }

        let indent = raw.len() - trimmed.len();
        let clean = sanitize_name(trimmed, false);
        let content = clean.trim();

        if content.is_empty() { continue; }

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
            format!("{}{}", sanitize_name(base, no_digits), ext)
        } else {
            sanitize_name(&child.content, no_digits)
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
            (dir.clone(), dir.join("index.md"))
        } else {
            let mut md = parent_path.join(format!("{}{}", base, ext));
            if md.exists() {
                let uniq = format!("{}_{}{}", base, &child.unique_id[..6], ext);
                md = parent_path.join(&uniq);
            }
            (parent_path.to_path_buf(), md)
        };

        write_file(&md_path, &child.body_lines, full)?;
        build_tree(arena, child_idx, &target_dir, no_digits, allow_empty)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
//  Main
// ---------------------------------------------------------------------------
fn main() {
    let args = parse_args().unwrap_or_else(|| {
        eprintln!("Usage: detree3 <input_file> <output_dir> [OPTIONS]");
        eprintln!("Run with --help for more info.");
        std::process::exit(1);
    });

    // Validation
    if !args.input_file.exists() {
        eprintln!("Error: '{}' not found.", args.input_file.display());
        std::process::exit(1);
    }
    if !args.input_file.is_file() {
        eprintln!("Error: '{}' is not a file.", args.input_file.display());
        std::process::exit(1);
    }
    let meta = fs::metadata(&args.input_file).unwrap();
    if meta.len() == 0 {
        eprintln!("Error: '{}' is empty.", args.input_file.display());
        std::process::exit(1);
    }

    // Read
    let file = File::open(&args.input_file).unwrap_or_else(|e| {
        eprintln!("Error reading file: {}", e);
        std::process::exit(1);
    });
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map(|l| l.unwrap()).collect();
    let content = lines.join("\n");

    // Detect format
    let fmt = match args.format {
        Format::Auto => {
            let first_non_empty = lines.iter().find(|l| !l.trim().is_empty());
            if let Some(line) = first_non_empty {
                let trimmed = line.trim_start();
                if trimmed.starts_with('{') || trimmed.starts_with('[') {
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
            let json_data: Value = serde_json::from_str(&content).unwrap_or_else(|e| {
                eprintln!("Error: invalid JSON — {}", e);
                std::process::exit(1);
            });
            parse_json(&json_data, args.remove_digits)
        }
        Format::Text => parse_text(&lines),
        Format::Auto => unreachable!(),
    };

    fs::create_dir_all(&args.output_dir).unwrap();
    build_tree(&arena, 0, &args.output_dir, args.remove_digits, args.allow_empty_folders)
        .unwrap_or_else(|e| {
            eprintln!("Error during build: {}", e);
            std::process::exit(1);
        });

    println!("Done.");
}
