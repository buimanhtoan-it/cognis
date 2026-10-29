//! `arb_source_tree` — generated source trees.
//!
//! Design: "file trees with mixed languages, empty files, non-UTF-8 bytes,
//! unicode paths, deep nesting, duplicate symbol names, cross-file call
//! graphs". Feeds properties P5–P12.
//!
//! The tree is a *description*, not bytes on disk: [`SourceTree::render`] turns
//! it into `(relative path, exact bytes)` pairs and [`SourceTree::materialize`]
//! writes those bytes under a caller-supplied root (usually a `tempfile::TempDir`
//! owned by the test). Keeping the description separate is what lets a property
//! apply a [`Transition`](crate::Transition) to the tree and re-render, rather
//! than having to diff a directory.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use proptest::prelude::*;
use sha2::{Digest, Sha256};

use crate::{NAME_POOL, UNICODE_SEGMENTS};

/// The languages the walker admits (`LANG_BY_EXT` in
/// `crates/cognis-indexer/src/pipeline.rs`), paired with the extension the
/// generator emits for them. Mixed languages in one tree is the point.
const LANGUAGES: &[(&str, &str)] = &[
    ("python", "py"),
    ("typescript", "ts"),
    ("go", "go"),
    ("java", "java"),
    ("csharp", "cs"),
    ("rust", "rs"),
];

/// ASCII directory segments, mixed with [`UNICODE_SEGMENTS`] so both spellings
/// appear in one tree.
const ASCII_SEGMENTS: &[&str] = &["src", "lib", "core", "app", "pkg"];

/// One generated function: its name and the names it calls.
///
/// Call targets are resolved against the *whole tree*, so a generated tree
/// contains genuine cross-file call graphs rather than per-file islands. A call
/// to a name that several files define is deliberately left ambiguous — that is
/// the duplicate-symbol-name case the design asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSpec {
    pub name: String,
    pub calls: Vec<String>,
}

/// What a generated file actually contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileBody {
    /// Parseable source declaring [`FunctionSpec`]s.
    Code(Vec<FunctionSpec>),
    /// Zero bytes — the `valid → empty` transition's target state.
    Empty,
    /// Bytes that are *not* valid UTF-8 — the `valid → invalid-UTF-8`
    /// transition's target state, and the parse-failure path of Requirement 8.7.
    InvalidUtf8(Vec<u8>),
}

/// One generated file: where it lives, what language it is, what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFileSpec {
    /// Forward-slash relative path, e.g. `src/café/alpha.py`.
    pub rel_path: String,
    /// Language label as `detect_language` would report it.
    pub language: &'static str,
    pub body: FileBody,
}

impl SourceFileSpec {
    /// The exact bytes this file has on disk. This is what `Content_Hash` is
    /// taken over (Requirement 8.1 — byte-exact, deliberately not the
    /// whitespace-normalised parser hash).
    pub fn bytes(&self) -> Vec<u8> {
        match &self.body {
            FileBody::Empty => Vec::new(),
            FileBody::InvalidUtf8(bytes) => bytes.clone(),
            FileBody::Code(functions) => render_code(self.language, functions).into_bytes(),
        }
    }

    /// `sha256` of [`bytes`](Self::bytes), hex — the `File_Cache` content hash.
    pub fn content_hash(&self) -> String {
        content_hash(&self.bytes())
    }

    /// Every function name this file declares.
    pub fn declared_names(&self) -> Vec<&str> {
        match &self.body {
            FileBody::Code(functions) => functions.iter().map(|f| f.name.as_str()).collect(),
            _ => Vec::new(),
        }
    }
}

/// A whole generated repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTree {
    pub files: Vec<SourceFileSpec>,
}

impl SourceTree {
    /// `(relative path, exact bytes)` for every file, in path order.
    pub fn render(&self) -> Vec<(String, Vec<u8>)> {
        let mut rendered: Vec<(String, Vec<u8>)> = self
            .files
            .iter()
            .map(|f| (f.rel_path.clone(), f.bytes()))
            .collect();
        rendered.sort_by(|a, b| a.0.cmp(&b.0));
        rendered
    }

    /// Write the tree under `root`, creating parent directories as needed.
    ///
    /// Non-UTF-8 bodies are written as raw bytes, so the parse-failure path is
    /// exercised against a real file rather than a simulated one.
    pub fn materialize(&self, root: &Path) -> io::Result<()> {
        for (rel, bytes) in self.render() {
            let target = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, &bytes)?;
        }
        Ok(())
    }

    /// Look a file up by its forward-slash relative path.
    pub fn file(&self, rel_path: &str) -> Option<&SourceFileSpec> {
        self.files.iter().find(|f| f.rel_path == rel_path)
    }

    /// Paths in deterministic order — handy for picking an edit target from a
    /// generated ordinal without reintroducing randomness.
    pub fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.files.iter().map(|f| f.rel_path.clone()).collect();
        paths.sort();
        paths
    }

    /// Files that a parser can actually read (neither empty nor non-UTF-8).
    pub fn parseable(&self) -> Vec<&SourceFileSpec> {
        self.files
            .iter()
            .filter(|f| matches!(f.body, FileBody::Code(ref fns) if !fns.is_empty()))
            .collect()
    }
}

/// `sha256(bytes)` as lowercase hex — the byte-exact `Content_Hash` the
/// `File_Cache` is keyed on.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_code(language: &str, functions: &[FunctionSpec]) -> String {
    match language {
        "python" => render_python(functions),
        "typescript" => render_typescript(functions),
        "go" => render_go(functions),
        "java" => render_java(functions),
        "csharp" => render_csharp(functions),
        _ => render_rust(functions),
    }
}

fn render_python(functions: &[FunctionSpec]) -> String {
    let mut out = String::new();
    for f in functions {
        out.push_str(&format!("def {}():\n", f.name));
        if f.calls.is_empty() {
            out.push_str("    pass\n");
        } else {
            for call in &f.calls {
                out.push_str(&format!("    {call}()\n"));
            }
        }
        out.push('\n');
    }
    out
}

fn render_typescript(functions: &[FunctionSpec]) -> String {
    let mut out = String::new();
    for f in functions {
        out.push_str(&format!("export function {}(): void {{\n", f.name));
        for call in &f.calls {
            out.push_str(&format!("  {call}();\n"));
        }
        out.push_str("}\n\n");
    }
    out
}

fn render_go(functions: &[FunctionSpec]) -> String {
    let mut out = String::from("package main\n\n");
    for f in functions {
        out.push_str(&format!("func {}() {{\n", f.name));
        for call in &f.calls {
            out.push_str(&format!("\t{call}()\n"));
        }
        out.push_str("}\n\n");
    }
    out
}

fn render_java(functions: &[FunctionSpec]) -> String {
    let mut out = String::from("class Generated {\n");
    for f in functions {
        out.push_str(&format!("    void {}() {{\n", f.name));
        for call in &f.calls {
            out.push_str(&format!("        {call}();\n"));
        }
        out.push_str("    }\n");
    }
    out.push_str("}\n");
    out
}

fn render_csharp(functions: &[FunctionSpec]) -> String {
    let mut out = String::from("class Generated {\n");
    for f in functions {
        out.push_str(&format!("    void {}() {{\n", f.name));
        for call in &f.calls {
            out.push_str(&format!("        {call}();\n"));
        }
        out.push_str("    }\n");
    }
    out.push_str("}\n");
    out
}

fn render_rust(functions: &[FunctionSpec]) -> String {
    let mut out = String::new();
    for f in functions {
        out.push_str(&format!("pub fn {}() {{\n", f.name));
        for call in &f.calls {
            out.push_str(&format!("    {call}();\n"));
        }
        out.push_str("}\n\n");
    }
    out
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Pre-resolution skeleton of one file. Call targets are still ordinals here;
/// they become real names once the whole tree's declared names are known, which
/// is what makes the call graph *cross-file*.
#[derive(Debug, Clone)]
struct RawFile {
    depth: usize,
    segments: Vec<usize>,
    stem: usize,
    language: usize,
    kind: u8,
    function_names: Vec<usize>,
    call_hints: Vec<Vec<usize>>,
    junk: Vec<u8>,
}

fn arb_raw_file() -> impl Strategy<Value = RawFile> {
    (
        // Depth 0..=4 gives both flat files and genuinely deep nesting.
        0usize..5,
        prop::collection::vec(0usize..(ASCII_SEGMENTS.len() + UNICODE_SEGMENTS.len()), 4),
        0usize..NAME_POOL.len(),
        0usize..LANGUAGES.len(),
        // 0..=7 → 6/8 Code, 1/8 Empty, 1/8 InvalidUtf8: degenerate bodies stay
        // frequent enough to matter without swamping the parseable cases.
        0u8..8,
        prop::collection::vec(0usize..NAME_POOL.len(), 1..4),
        prop::collection::vec(prop::collection::vec(0usize..64, 0..3), 1..4),
        prop::collection::vec(any::<u8>(), 0..8),
    )
        .prop_map(
            |(depth, segments, stem, language, kind, function_names, call_hints, junk)| RawFile {
                depth,
                segments,
                stem,
                language,
                kind,
                function_names,
                call_hints,
                junk,
            },
        )
}

/// Generated trees hold 1..=6 files by default — enough for cross-file call
/// graphs and duplicate names, small enough that a shrunk counterexample is
/// readable.
pub fn arb_source_tree() -> impl Strategy<Value = SourceTree> {
    arb_source_tree_sized(1..7)
}

/// [`arb_source_tree`] with a caller-chosen file count, for properties that need
/// a larger corpus (for example P12's "independent of total repository row
/// counts" check).
pub fn arb_source_tree_sized(
    file_count: std::ops::Range<usize>,
) -> impl Strategy<Value = SourceTree> {
    prop::collection::vec(arb_raw_file(), file_count).prop_map(build_tree)
}

fn build_tree(raws: Vec<RawFile>) -> SourceTree {
    // Pass 1 — paths and declared names. Paths are de-duplicated by suffixing
    // the ordinal, so distinct generated files never collapse onto one path.
    let mut used_paths: BTreeSet<String> = BTreeSet::new();
    let mut skeletons: Vec<(String, &'static str, u8, Vec<String>, &RawFile)> = Vec::new();

    for (index, raw) in raws.iter().enumerate() {
        let (language, ext) = LANGUAGES[raw.language % LANGUAGES.len()];
        let mut path = String::new();
        for seg_index in raw.segments.iter().take(raw.depth) {
            let segment = segment_name(*seg_index);
            path.push_str(segment);
            path.push('/');
        }
        let stem = NAME_POOL[raw.stem % NAME_POOL.len()];
        let mut candidate = format!("{path}{stem}.{ext}");
        if used_paths.contains(&candidate) {
            candidate = format!("{path}{stem}_{index}.{ext}");
        }
        used_paths.insert(candidate.clone());

        // Names are drawn from the shared pool, so two files routinely declare
        // the same name — the ambiguity case P11 and the resolver care about.
        let names: Vec<String> = raw
            .function_names
            .iter()
            .map(|n| NAME_POOL[*n % NAME_POOL.len()].to_string())
            .collect();

        skeletons.push((candidate, language, raw.kind, names, raw));
    }

    // Every name declared anywhere, in a stable order, so call hints can point
    // across files.
    let mut all_names: Vec<String> = Vec::new();
    for (_, _, kind, names, _) in &skeletons {
        if body_kind(*kind) == 0 {
            for name in names {
                if !all_names.contains(name) {
                    all_names.push(name.clone());
                }
            }
        }
    }
    all_names.sort();

    // Pass 2 — resolve call hints into real names.
    let files = skeletons
        .into_iter()
        .map(|(rel_path, language, kind, names, raw)| {
            let body = match body_kind(kind) {
                1 => FileBody::Empty,
                2 => FileBody::InvalidUtf8(invalid_utf8(&raw.junk)),
                _ => {
                    let functions = names
                        .iter()
                        .enumerate()
                        .map(|(fn_index, name)| {
                            let hints = raw.call_hints.get(fn_index).cloned().unwrap_or_default();
                            let calls = if all_names.is_empty() {
                                Vec::new()
                            } else {
                                hints
                                    .iter()
                                    .map(|h| all_names[*h % all_names.len()].clone())
                                    .collect()
                            };
                            FunctionSpec {
                                name: name.clone(),
                                calls,
                            }
                        })
                        .collect();
                    FileBody::Code(functions)
                }
            };
            SourceFileSpec {
                rel_path,
                language,
                body,
            }
        })
        .collect();

    SourceTree { files }
}

/// `0` → code, `1` → empty, `2` → non-UTF-8.
fn body_kind(kind: u8) -> u8 {
    match kind {
        6 => 1,
        7 => 2,
        _ => 0,
    }
}

fn segment_name(index: usize) -> &'static str {
    if index < ASCII_SEGMENTS.len() {
        ASCII_SEGMENTS[index]
    } else {
        UNICODE_SEGMENTS[(index - ASCII_SEGMENTS.len()) % UNICODE_SEGMENTS.len()]
    }
}

/// Force a byte sequence to be invalid UTF-8 regardless of what was generated:
/// `0xFF` and `0xFE` are never valid UTF-8 lead bytes.
fn invalid_utf8(junk: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend_from_slice(junk);
    bytes.push(0xFF);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// The generator's own contract: every tree is non-empty, paths are
        /// unique, and the declared bodies round-trip through `render`.
        #[test]
        fn generated_trees_are_well_formed(tree in arb_source_tree()) {
            prop_assert!(!tree.files.is_empty());
            let paths = tree.paths();
            let unique: BTreeSet<&String> = paths.iter().collect();
            prop_assert_eq!(paths.len(), unique.len());
            for (rel, bytes) in tree.render() {
                let file = tree.file(&rel).expect("rendered path is in the tree");
                prop_assert_eq!(file.bytes(), bytes);
            }
        }

        /// Non-UTF-8 bodies really are non-UTF-8, and empty bodies really are
        /// zero bytes — the two degenerate transitions depend on it.
        #[test]
        fn degenerate_bodies_are_degenerate(tree in arb_source_tree()) {
            for file in &tree.files {
                match &file.body {
                    FileBody::Empty => prop_assert!(file.bytes().is_empty()),
                    FileBody::InvalidUtf8(_) => {
                        prop_assert!(std::str::from_utf8(&file.bytes()).is_err());
                    }
                    FileBody::Code(_) => {
                        prop_assert!(std::str::from_utf8(&file.bytes()).is_ok());
                    }
                }
            }
        }

        /// Content hashes are byte-exact: a one-byte change changes the hash.
        #[test]
        fn content_hash_is_byte_exact(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
            let base = content_hash(&bytes);
            let mut changed = bytes.clone();
            changed.push(0x01);
            prop_assert_ne!(base, content_hash(&changed));
        }
    }

    #[test]
    fn every_language_renders_something() {
        let functions = vec![FunctionSpec {
            name: "alpha".into(),
            calls: vec!["beta".into()],
        }];
        for (language, _) in LANGUAGES {
            let rendered = render_code(language, &functions);
            assert!(
                rendered.contains("alpha"),
                "{language} lost the declaration"
            );
            assert!(rendered.contains("beta"), "{language} lost the call");
        }
    }
}
