//! Keeps the handlers in `src/containers` separable from the core and from each
//! other, so each can become its own crate: a handler may use only the core's
//! public API and the external crates it owns.

use std::fs;
use std::path::{Path, PathBuf};

/// Core modules a handler must reach through the crate root instead.
const PRIVATE_CORE: &[&str] = &[
    "crc",
    "error",
    "execute",
    "handler",
    "mapped",
    "plan_builder",
    "range",
    "registry",
    "segment",
    "sink",
    "structure",
    "task",
    "thumbnail",
    "tiff",
    "updates",
    "walk",
    "xmp",
    "containers",
];

const HANDLERS: &[&str] = &["jpeg", "png", "bmff", "riff", "svg", "pdf"];

/// External crates a handler owns; no other handler may use them.
const OWNED_DEPS: &[(&str, &str)] = &[
    ("jpeg", "md5"),
    ("jpeg", "memchr"),
    ("svg", "quick_xml"),
    ("pdf", "flate2"),
];
const ALL_DEPS: &[&str] = &["md5", "memchr", "quick_xml", "flate2", "memmap2"];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The handler a file belongs to: `jpeg` for `jpeg.rs`, `pdf` for `pdf/lexer.rs`.
fn handler_of(containers: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(containers).unwrap();
    let first = rel.components().next()?.as_os_str().to_str()?;
    let name = first.strip_suffix(".rs").unwrap_or(first);
    HANDLERS.contains(&name).then(|| name.to_string())
}

fn code(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

fn uses(line: &str, path: &str) -> bool {
    let mut rest = line;
    while let Some(i) = rest.find(path) {
        let before = rest[..i].chars().next_back();
        if !before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return true;
        }
        rest = &rest[i + path.len()..];
    }
    false
}

#[test]
fn handlers_use_only_the_public_core_and_their_own_dependencies() {
    let containers = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/containers");
    let mut files = Vec::new();
    sources(&containers, &mut files);
    let mut problems = Vec::new();

    for file in files {
        let Some(owner) = handler_of(&containers, &file) else {
            continue;
        };
        let text = fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let line = code(line);
            let at = |msg: String| format!("{}:{}: {msg}", file.display(), n + 1);

            for m in PRIVATE_CORE {
                if uses(line, &format!("crate::{m}::")) {
                    problems.push(at(format!(
                        "crate::{m} is not public API; use the crate root"
                    )));
                }
            }
            for h in HANDLERS.iter().filter(|h| **h != owner) {
                if uses(line, &format!("super::{h}")) || uses(line, &format!("containers::{h}")) {
                    problems.push(at(format!("handlers must not depend on `{h}`")));
                }
            }
            for dep in ALL_DEPS {
                let owned = OWNED_DEPS.iter().any(|(h, d)| *h == owner && d == dep);
                if !owned && uses(line, &format!("{dep}::")) {
                    problems.push(at(format!("`{owner}` does not own the `{dep}` dependency")));
                }
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn the_core_reaches_handlers_only_through_register_builtin() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    let containers = src.join("containers");
    let mut problems = Vec::new();

    for file in files.iter().filter(|f| !f.starts_with(&containers)) {
        let text = fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let line = code(line);
            // Doc and comment lines are stripped by `code`; only real paths remain.
            if uses(line, "containers::") && !line.contains("register_builtin") {
                problems.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}
