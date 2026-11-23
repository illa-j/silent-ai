//! Build a compact, safe text representation of a user-selected project
//! folder for the LLM context.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::ProjectConfig;
use crate::error::{Result, SilentError};

/// Built-in directories that almost never contain useful source material.
/// Stops pathological trees from delaying startup. `max_files` is usually much
/// smaller; this is only a traversal guard.
const MAX_SCAN_ENTRIES: usize = 5_000;

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".idea",
    ".vscode",
    ".gradle",
    ".cache",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    ".expo",
    ".terraform",
    ".serverless",
    "target",
    "node_modules",
    "dist",
    "build",
    "out",
    "coverage",
    "vendor",
    "Pods",
    "DerivedData",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    "obj",
    "bin",
];

/// Files that are useful even though they have no text extension.
const MANIFEST_NAMES: &[&str] = &[
    "cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "composer.json",
    "gemfile",
    "cmakelists.txt",
    "makefile",
    "dockerfile",
    "procfile",
    "justfile",
    "taskfile.yml",
];

const TEXT_EXTENSIONS: &[&str] = &[
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "py",
    "java",
    "kt",
    "kts",
    "scala",
    "go",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hh",
    "cs",
    "fs",
    "fsx",
    "vb",
    "php",
    "rb",
    "swift",
    "m",
    "mm",
    "dart",
    "lua",
    "pl",
    "r",
    "jl",
    "ex",
    "exs",
    "erl",
    "hrl",
    "clj",
    "cljs",
    "hs",
    "ml",
    "mli",
    "nim",
    "zig",
    "sol",
    "vue",
    "svelte",
    "astro",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "styl",
    "sql",
    "graphql",
    "gql",
    "proto",
    "thrift",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "toml",
    "yaml",
    "yml",
    "json",
    "jsonc",
    "json5",
    "xml",
    "ini",
    "cfg",
    "conf",
    "properties",
    "md",
    "txt",
    "rst",
    "adoc",
    "tex",
];

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "java", "kt", "kts", "scala", "go", "c",
    "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "fs", "fsx", "vb", "php", "rb", "swift", "m", "mm",
    "dart", "lua", "pl", "r", "jl", "ex", "exs", "erl", "hrl", "clj", "cljs", "hs", "ml", "mli",
    "nim", "zig", "sol", "vue", "svelte", "astro",
];

const IGNORED_FILE_NAMES: &[&str] = &[
    "cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "poetry.lock",
    "pipfile.lock",
    "composer.lock",
    "gemfile.lock",
    "go.sum",
    "mix.lock",
    "pubspec.lock",
    "flake.lock",
    "uv.lock",
    "config.local.toml",
    "config.local.json",
    ".env",
    ".env.local",
    ".env.development",
    ".env.production",
    ".npmrc",
    ".pypirc",
    ".netrc",
    ".git-credentials",
];

#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// Canonical folder path.
    pub root: PathBuf,
    /// Folder name, used in the prompt.
    pub name: String,
    /// Number of files whose contents are included.
    pub files: usize,
    /// Characters in the generated context.
    pub chars: usize,
    /// True when a file, count, or character limit cut the context.
    pub truncated: bool,
    /// Prompt-ready project block.
    pub text: String,
}

struct Entry {
    rel: String,
    path: PathBuf,
    priority: u8,
    depth: usize,
}

/// Scan `root` and build a prompt-ready context. The function never follows
/// symlinks and skips binary files, secrets, lockfiles, hidden files, and common
/// build directories.
pub fn scan_project(root: &Path, config: &ProjectConfig) -> Result<ProjectContext> {
    let root = root
        .canonicalize()
        .map_err(|err| SilentError::io(root, err))?;
    let metadata = fs::metadata(&root).map_err(|err| SilentError::io(&root, err))?;
    if !metadata.is_dir() {
        anyhow::bail!("selected project is not a directory: {}", root.display());
    }

    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project")
        .to_owned();

    let mut entries = Vec::new();
    collect_entries(&root, &root, config, &mut entries, 0);
    entries.sort_by(|left, right| {
        (left.priority, left.depth, &left.rel).cmp(&(right.priority, right.depth, &right.rel))
    });

    let scanned = entries.len();
    let mut selected = Vec::new();
    for entry in entries.into_iter().take(config.max_files) {
        if let Some(text) = read_text_file(&entry.path, config.max_file_bytes) {
            selected.push((entry.rel, text));
        }
    }

    let included = selected.len();
    let mut truncated = scanned > config.max_files;

    let mut header = format!("Project folder: {name}\n");
    header.push_str(&format!(
        "Included files: {included} of {scanned} text files"
    ));
    if truncated {
        header.push_str(" (file limit reached)");
    }
    header.push_str(".\nFile tree:\n");
    for (rel, _) in &selected {
        header.push_str(rel);
        header.push('\n');
    }
    if selected.is_empty() {
        header.push_str("(no readable text files)\n");
    }
    header.push_str("\nFile contents:\n");

    let mut text = String::new();
    let mut budget = config.max_chars;
    let (part, cut) = take_chars(&header, budget);
    text.push_str(part);
    budget = budget.saturating_sub(part.chars().count());
    if cut {
        text.push_str("\n[project context truncated]\n");
        return Ok(ProjectContext {
            root,
            name,
            files: included,
            chars: text.chars().count(),
            truncated: true,
            text,
        });
    }

    for (rel, content) in &selected {
        if budget == 0 {
            truncated = true;
            break;
        }
        let block = format!("--- {rel} ---\n{content}\n\n");
        let (part, cut) = take_chars(&block, budget);
        text.push_str(part);
        budget = budget.saturating_sub(part.chars().count());
        if cut {
            truncated = true;
            break;
        }
    }

    if truncated && budget > 0 {
        let note = "\n[project context truncated]\n";
        let (part, _) = take_chars(note, budget);
        text.push_str(part);
    }

    Ok(ProjectContext {
        root,
        name,
        files: included,
        chars: text.chars().count(),
        truncated,
        text,
    })
}

fn collect_entries(
    root: &Path,
    dir: &Path,
    config: &ProjectConfig,
    out: &mut Vec<Entry>,
    depth: usize,
) {
    if depth > 32 || out.len() >= MAX_SCAN_ENTRIES {
        return;
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read_dir.filter_map(std::result::Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        if out.len() >= MAX_SCAN_ENTRIES {
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();

        if file_type.is_dir() {
            if is_ignored_dir(&name, config) {
                continue;
            }
            collect_entries(root, &path, config, out, depth + 1);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if !config.include_hidden && name.starts_with('.') && !is_allowed_hidden(&name) {
            continue;
        }
        if is_ignored_file(&name) || !is_text_name(&name) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > config.max_file_bytes {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        out.push(Entry {
            path,
            priority: file_priority(&name),
            depth: rel.matches('/').count(),
            rel,
        });
    }
}

fn is_ignored_dir(name: &str, config: &ProjectConfig) -> bool {
    let lower = name.to_ascii_lowercase();
    if !config.include_hidden && lower.starts_with('.') {
        return true;
    }
    IGNORED_DIRS
        .iter()
        .any(|ignored| ignored.eq_ignore_ascii_case(&lower))
        || config
            .exclude_dirs
            .iter()
            .any(|ignored| ignored.eq_ignore_ascii_case(&lower))
}

fn is_allowed_hidden(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".env.example" | ".env.sample"
    )
}

fn is_ignored_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if IGNORED_FILE_NAMES.contains(&lower.as_str()) {
        return true;
    }
    if lower.starts_with(".env.") && !matches!(lower.as_str(), ".env.example" | ".env.sample") {
        return true;
    }
    if lower.starts_with("credentials")
        || lower.starts_with("secrets")
        || lower.starts_with("api_key")
        || lower.starts_with("apikey")
        || lower.starts_with("private_key")
        || lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
    {
        return true;
    }
    if lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".map")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with(".keystore")
        || lower.ends_with(".jks")
    {
        return true;
    }
    false
}

fn is_text_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if MANIFEST_NAMES.contains(&lower.as_str())
        || lower.starts_with("readme")
        || matches!(lower.as_str(), ".env.example" | ".env.sample")
    {
        return true;
    }
    let Some(ext) = Path::new(name).extension().and_then(|value| value.to_str()) else {
        return false;
    };
    TEXT_EXTENSIONS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(ext))
}

fn file_priority(name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    if MANIFEST_NAMES.contains(&lower.as_str()) || lower.starts_with("readme") {
        return 0;
    }
    let Some(ext) = Path::new(name).extension().and_then(|value| value.to_str()) else {
        return 2;
    };
    if SOURCE_EXTENSIONS
        .iter()
        .any(|source| source.eq_ignore_ascii_case(ext))
    {
        1
    } else {
        2
    }
}

fn read_text_file(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > max_bytes {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn take_chars(text: &str, max: usize) -> (&str, bool) {
    if max == 0 {
        return ("", !text.is_empty());
    }
    match text.char_indices().nth(max) {
        Some((index, _)) => (&text[..index], true),
        None => (text, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!(
            "silent-ai-project-{tag}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, text).expect("write fixture");
    }

    #[test]
    fn scans_source_and_skips_build_secrets_and_binaries() {
        let root = temp_root("scan");
        write(&root.join("Cargo.toml"), "[package]\nname = \"demo\"\n");
        write(&root.join("README.md"), "Demo project documentation\n");
        write(
            &root.join("src/main.rs"),
            "fn main() { println!(\"hi\"); }\n",
        );
        write(&root.join("src/secret.pem"), "PRIVATE KEY");
        write(&root.join(".env"), "API_KEY=SECRET");
        write(&root.join(".env.example"), "API_KEY=replace-me");
        write(&root.join("target/generated.rs"), "generated code");
        write(
            &root.join("node_modules/pkg/index.js"),
            "module.exports = 1;",
        );
        write(&root.join("src/blob.rs"), "\0\x01\x02");

        let context = scan_project(&root, &ProjectConfig::default()).expect("scan");
        assert_eq!(context.name, root.file_name().unwrap().to_string_lossy());
        assert!(context.files >= 4, "files: {}", context.files);
        assert!(context.text.contains("fn main()"), "{}", context.text);
        assert!(context.text.contains("Demo project documentation"));
        assert!(context.text.contains("Cargo.toml"));
        assert!(context.text.contains(".env.example"));
        assert!(!context.text.contains("SECRET"));
        assert!(!context.text.contains("PRIVATE KEY"));
        assert!(!context.text.contains("generated code"));
        assert!(!context.text.contains("module.exports"));
        assert!(!context.text.contains("blob.rs"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn limits_files_and_characters() {
        let root = temp_root("limits");
        for index in 0..5 {
            write(
                &root.join(format!("src/file{index}.rs")),
                &format!("fn file{index}() {{}}\n{}", "x".repeat(200)),
            );
        }
        let config = ProjectConfig {
            max_files: 2,
            max_file_bytes: 1024,
            max_chars: 260,
            include_hidden: false,
            exclude_dirs: Vec::new(),
        };
        let context = scan_project(&root, &config).expect("scan");
        assert!(context.truncated);
        assert!(
            context.chars <= config.max_chars,
            "chars: {}",
            context.chars
        );
        assert!(context.files <= 2, "files: {}", context.files);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_a_non_directory() {
        let root = temp_root("missing");
        assert!(scan_project(&root, &ProjectConfig::default()).is_err());
    }
}
