//! Parser-stage tests (Task 8.1): tree-sitter extraction for TS/JS, Python, Go,
//! C#, Java + per-file textual fallback (Requirements 9.1, 9.4).

use cognis_core::{ParseStatus, SymbolKind};
use cognis_indexer::{language_for_path, parse_source};

fn names(out: &cognis_indexer::ParseOutput) -> Vec<String> {
    out.symbols.iter().map(|s| s.name.clone()).collect()
}

#[test]
fn detects_supported_languages() {
    for (path, label) in [
        ("a.ts", "typescript"),
        ("a.tsx", "typescript"),
        ("a.js", "javascript"),
        ("a.jsx", "javascript"),
        ("a.py", "python"),
        ("a.go", "go"),
        ("a.cs", "csharp"),
        ("a.java", "java"),
    ] {
        assert_eq!(
            language_for_path(path).map(|l| l.label),
            Some(label),
            "{path}"
        );
    }
    assert!(language_for_path("README.md").is_none());
    assert!(language_for_path("noext").is_none());
}

#[test]
fn python_functions_classes_methods_consts() {
    let src = r#"
GREETING = "hi"

def top_level(a, b):
    """A top-level function."""
    return a + b

class Service:
    """A service class."""

    def handle(self, req):
        return req

    async def fetch(self):
        return 1
"#;
    let out = parse_source("src/app.py", src);
    assert_eq!(out.status, ParseStatus::Ok);
    assert!(!out.fell_back);
    let n = names(&out);
    assert!(n.contains(&"top_level".to_string()));
    assert!(n.contains(&"Service".to_string()));
    assert!(n.contains(&"handle".to_string()));
    assert!(n.contains(&"fetch".to_string()));
    assert!(n.contains(&"GREETING".to_string()));

    let svc = out.symbols.iter().find(|s| s.name == "Service").unwrap();
    assert_eq!(svc.kind, SymbolKind::Class);
    assert_eq!(svc.docstring.as_deref(), Some("A service class."));

    let handle = out.symbols.iter().find(|s| s.name == "handle").unwrap();
    assert_eq!(handle.kind, SymbolKind::Method);
    // qualified_name mirrors Python: "<lang>:<file>:<Class.method>"
    assert_eq!(handle.qualified_name, "py:src/app.py:Service.handle");
    assert!(handle.id.starts_with("py:src/app.py:Service.handle@"));

    let fetch = out.symbols.iter().find(|s| s.name == "fetch").unwrap();
    assert!(fetch
        .signature
        .as_deref()
        .unwrap()
        .starts_with("async def fetch"));

    let g = out.symbols.iter().find(|s| s.name == "GREETING").unwrap();
    assert_eq!(g.kind, SymbolKind::Const);
}

#[test]
fn typescript_functions_class_interface_arrow_const() {
    let src = r#"
export interface User { id: number; }

export function createUser(name: string): User {
    return { id: 1 };
}

export const handler = (req: Request) => {
    return req;
};

export const MAX_RETRIES = 5;

export class Repo {
    find(id: number) { return id; }
    get size() { return 0; }
}
"#;
    let out = parse_source("src/users.ts", src);
    assert_eq!(out.status, ParseStatus::Ok);
    let n = names(&out);
    assert!(n.contains(&"User".to_string()));
    assert!(n.contains(&"createUser".to_string()));
    assert!(n.contains(&"handler".to_string()));
    assert!(n.contains(&"MAX_RETRIES".to_string()));
    assert!(n.contains(&"Repo".to_string()));
    assert!(n.contains(&"find".to_string()));

    let iface = out.symbols.iter().find(|s| s.name == "User").unwrap();
    assert_eq!(iface.kind, SymbolKind::Interface);
    let handler = out.symbols.iter().find(|s| s.name == "handler").unwrap();
    assert_eq!(handler.kind, SymbolKind::Function);
    let max = out
        .symbols
        .iter()
        .find(|s| s.name == "MAX_RETRIES")
        .unwrap();
    assert_eq!(max.kind, SymbolKind::Const);
    let find = out.symbols.iter().find(|s| s.name == "find").unwrap();
    assert_eq!(find.kind, SymbolKind::Method);
    assert_eq!(find.qualified_name, "ts:src/users.ts:Repo.find");
}

#[test]
fn javascript_uses_js_prefix_via_ts_grammar() {
    let src = "export function add(a, b) { return a + b; }\n";
    let out = parse_source("src/math.js", src);
    assert_eq!(out.language, Some("javascript"));
    let add = out.symbols.iter().find(|s| s.name == "add").unwrap();
    assert!(add.id.starts_with("js:src/math.js:add@"));
    assert_eq!(add.language, "javascript");
}

#[test]
fn go_functions_methods_types() {
    let src = r#"
package main

// Greeter greets.
type Greeter struct {
    name string
}

type Speaker interface {
    Speak() string
}

func New(name string) *Greeter {
    return &Greeter{name: name}
}

func (g *Greeter) Greet() string {
    return "hi " + g.name
}
"#;
    let out = parse_source("internal/greet.go", src);
    assert_eq!(out.status, ParseStatus::Ok);
    let n = names(&out);
    assert!(n.contains(&"Greeter".to_string()));
    assert!(n.contains(&"Speaker".to_string()));
    assert!(n.contains(&"New".to_string()));
    assert!(n.contains(&"Greet".to_string()));

    let g = out.symbols.iter().find(|s| s.name == "Greeter").unwrap();
    assert_eq!(g.kind, SymbolKind::Class);
    let sp = out.symbols.iter().find(|s| s.name == "Speaker").unwrap();
    assert_eq!(sp.kind, SymbolKind::Interface);
    let greet = out.symbols.iter().find(|s| s.name == "Greet").unwrap();
    assert_eq!(greet.kind, SymbolKind::Method);
    // receiver type qualifies the method
    assert_eq!(greet.qualified_name, "go:internal/greet.go:Greeter.Greet");
}

#[test]
fn csharp_nested_types_and_methods() {
    let src = r#"
namespace App.Auth {
    /// <summary>Validates tokens.</summary>
    public class JwtValidator {
        public bool Validate(string token) { return true; }

        public class Options {
            public void Reset() { }
        }
    }

    public interface IClock {
        long Now();
    }

    public enum Status { Active, Inactive }
}
"#;
    let out = parse_source("src/Auth/Jwt.cs", src);
    assert_eq!(out.status, ParseStatus::Ok);
    let n = names(&out);
    assert!(n.contains(&"JwtValidator".to_string()));
    assert!(n.contains(&"Validate".to_string()));
    assert!(n.contains(&"Options".to_string()));
    assert!(n.contains(&"Reset".to_string()));
    assert!(n.contains(&"IClock".to_string()));
    assert!(n.contains(&"Status".to_string()));

    let validate = out.symbols.iter().find(|s| s.name == "Validate").unwrap();
    assert_eq!(validate.kind, SymbolKind::Method);
    assert_eq!(
        validate.qualified_name,
        "cs:src/Auth/Jwt.cs:JwtValidator.Validate"
    );
    // nested method qualifies through both type scopes
    let reset = out.symbols.iter().find(|s| s.name == "Reset").unwrap();
    assert_eq!(
        reset.qualified_name,
        "cs:src/Auth/Jwt.cs:JwtValidator.Options.Reset"
    );
    let iface = out.symbols.iter().find(|s| s.name == "IClock").unwrap();
    assert_eq!(iface.kind, SymbolKind::Interface);
}

#[test]
fn java_classes_interfaces_methods() {
    let src = r#"
package auth;

/** Validates tokens. */
public class JwtValidator {
    public boolean validate(String token) {
        return true;
    }

    interface Inner {
        void run();
    }
}

interface Clock {
    long now();
}

enum Status { ACTIVE, INACTIVE }
"#;
    let out = parse_source("src/main/java/auth/JwtValidator.java", src);
    assert_eq!(out.status, ParseStatus::Ok);
    let n = names(&out);
    assert!(n.contains(&"JwtValidator".to_string()));
    assert!(n.contains(&"validate".to_string()));
    assert!(n.contains(&"Inner".to_string()));
    assert!(n.contains(&"Clock".to_string()));
    assert!(n.contains(&"Status".to_string()));

    let v = out.symbols.iter().find(|s| s.name == "validate").unwrap();
    assert_eq!(v.kind, SymbolKind::Method);
    assert_eq!(
        v.qualified_name,
        "java:src/main/java/auth/JwtValidator.java:JwtValidator.validate"
    );
    let clock = out.symbols.iter().find(|s| s.name == "Clock").unwrap();
    assert_eq!(clock.kind, SymbolKind::Interface);
}

#[test]
fn all_symbols_validate_and_have_well_formed_ids() {
    let py = parse_source("m.py", "def f():\n    return 1\n");
    for s in &py.symbols {
        s.validate().expect("symbol must satisfy core invariants");
        // id == "<lang>:<file>:<qual>@<hash>" and qualified_name is the prefix.
        let prefix = s.id.rsplit_once('@').unwrap().0;
        assert_eq!(prefix, s.qualified_name);
        assert!(s.line_end >= s.line_start);
    }
}

// --- Fallback / fault-tolerance (Requirement 9.4) ---

#[test]
fn unsupported_extension_falls_back_to_textual_module() {
    let out = parse_source("notes.md", "# Title\n\nsome prose content\n");
    assert!(out.fell_back);
    assert_eq!(out.status, ParseStatus::Failed);
    assert_eq!(out.language, None);
    assert_eq!(out.symbols.len(), 1);
    let s = &out.symbols[0];
    assert_eq!(s.kind, SymbolKind::Module);
    assert_eq!(s.name, "notes");
    assert!(s.body_excerpt.is_some());
    s.validate().unwrap();
}

#[test]
fn unparseable_supported_source_falls_back_not_aborts() {
    // Heavily broken C# that the grammar cannot turn into any type/method.
    let src = "@@@ <<< not valid code >>> ;;; ((( ";
    let out = parse_source("Broken.cs", src);
    // Either way the file is not lost and the batch can continue.
    assert!(!out.symbols.is_empty() || out.status == ParseStatus::Partial);
    if out.fell_back {
        assert_eq!(out.symbols[0].kind, SymbolKind::Module);
        assert_eq!(out.status, ParseStatus::Failed);
    }
}

#[test]
fn empty_file_is_ok_with_no_symbols() {
    let out = parse_source("empty.py", "   \n  \n");
    assert!(out.symbols.is_empty());
    assert_eq!(out.status, ParseStatus::Ok);
}

#[test]
fn batch_continues_past_a_failing_file() {
    // Simulate a batch: a good file, a broken one, another good file.
    let batch = [
        ("a.py", "def a():\n    return 1\n"),
        ("weird.xyz", "binary-ish \u{0}\u{1} content"),
        ("b.go", "package m\nfunc B() {}\n"),
    ];
    let mut total = 0;
    for (path, src) in batch {
        let out = parse_source(path, src); // must never panic
        total += out.symbols.len();
    }
    // All three produced at least one symbol (good ones structured, weird one
    // via textual fallback) — the failing file did not abort the batch.
    assert!(total >= 3);
}
