//! Attribute extractor for the enricher stage (Task 8.2).
//!
//! Rust mirror of `cognis_indexer.enricher.attributes.AttributeExtractor`.
//! Detects side-effect / contract metadata from a symbol's body text via regex:
//!
//! * `db_table` — SQL `FROM`/`JOIN`/`INTO`/`UPDATE`/`TABLE` + identifier.
//! * `http_route` — FastAPI/Flask/Express/Hono/Gin route registration.
//! * `env_var` — `os.environ[...]`, `os.getenv(...)`, `process.env.X`,
//!   `os.Getenv("X")`.
//! * `external_call` — `requests.*`, `httpx.*`, `fetch(`, `axios.*`,
//!   `http.Get/Post/Client`.
//!
//! Results feed `SymbolAttribute` rows. Output is deduplicated by `(key, value)`
//! preserving first-occurrence order.

use std::sync::OnceLock;

use regex::Regex;

/// A single extracted attribute before it becomes a `SymbolAttribute` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAttribute {
    /// One of `db_table`, `http_route`, `env_var`, `external_call`.
    pub key: String,
    /// Extracted value (table name, route path, var name, client type, …).
    pub value: String,
}

/// SQL reserved words that can follow a keyword and must not be taken as a
/// table name (mirror the Python skip-set).
const SQL_RESERVED: &[&str] = &[
    "SET", "WHERE", "VALUES", "SELECT", "AND", "OR", "ON", "AS", "IN", "BY", "NULL",
];

fn sql_keyword_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:FROM|JOIN|INTO|UPDATE|TABLE)\s+(\w+)").unwrap())
}

fn http_route_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\s*\(\s*['"/]([^'"\)\s]+)"#)
            .unwrap()
    })
}

fn http_decorator_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\.(get|post|put|patch|delete|head|options|route)\s*\(\s*['"/]([^'"\)\s]+)"#,
        )
        .unwrap()
    })
}

fn env_patterns() -> &'static [Regex] {
    static PATS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATS.get_or_init(|| {
        vec![
            Regex::new(r#"os\.environ\[['"](\w+)['"]\]"#).unwrap(),
            Regex::new(r#"os\.getenv\(\s*['"](\w+)['"]\s*(?:,|\))"#).unwrap(),
            Regex::new(r#"os\.environ\.get\(\s*['"](\w+)['"]\s*(?:,|\))"#).unwrap(),
            Regex::new(r#"process\.env\.([A-Za-z_][A-Za-z0-9_]*)"#).unwrap(),
            Regex::new(r#"process\.env\[['"]([A-Za-z_][A-Za-z0-9_]*)['"]\]"#).unwrap(),
            Regex::new(r#"os\.Getenv\(\s*["']([A-Za-z_][A-Za-z0-9_]*)["']\s*\)"#).unwrap(),
        ]
    })
}

/// Extract side-effect / contract metadata from symbol body text. Stateless.
#[derive(Debug, Default)]
pub struct AttributeExtractor;

impl AttributeExtractor {
    /// Return all detected attributes in `body`, deduped by `(key, value)`.
    pub fn extract(&self, body: &str) -> Vec<ExtractedAttribute> {
        if body.is_empty() {
            return Vec::new();
        }
        let mut seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut out: Vec<ExtractedAttribute> = Vec::new();
        let mut add = |key: &str, value: &str, out: &mut Vec<ExtractedAttribute>| {
            let pair = (key.to_string(), value.to_string());
            if seen.insert(pair) {
                out.push(ExtractedAttribute {
                    key: key.to_string(),
                    value: value.to_string(),
                });
            }
        };

        // db_table
        for caps in sql_keyword_re().captures_iter(body) {
            let table = &caps[1];
            if !SQL_RESERVED.contains(&table.to_ascii_uppercase().as_str()) {
                add("db_table", table, &mut out);
            }
        }

        // http_route — method-then-path, then decorator style.
        for caps in http_route_re().captures_iter(body) {
            let path = &caps[1];
            if path.starts_with('/') {
                add("http_route", path, &mut out);
            }
        }
        for caps in http_decorator_re().captures_iter(body) {
            let path = &caps[2];
            if path.starts_with('/') {
                add("http_route", path, &mut out);
            }
        }

        // env_var
        for pattern in env_patterns() {
            for caps in pattern.captures_iter(body) {
                add("env_var", &caps[1], &mut out);
            }
        }

        // external_call
        for (re, prefix, lower_group) in external_call_specs() {
            for caps in re.captures_iter(body) {
                let value = match lower_group {
                    Some(g) => format!("{prefix}.{}", caps[*g].to_ascii_lowercase()),
                    None => prefix.to_string(),
                };
                add("external_call", &value, &mut out);
            }
        }

        out
    }
}

/// `(regex, value_prefix, optional_capture_group_to_lowercase)` for external
/// HTTP-client call sites (mirror the per-client patterns in `attributes.py`).
fn external_call_specs() -> &'static [(Regex, &'static str, Option<usize>)] {
    static SPECS: OnceLock<Vec<(Regex, &'static str, Option<usize>)>> = OnceLock::new();
    SPECS.get_or_init(|| {
        vec![
            (
                Regex::new(r"(?i)(?:^|[^\w])requests\.(get|post|put|delete|head|patch|request)\s*\(")
                    .unwrap(),
                "requests",
                Some(1),
            ),
            (
                Regex::new(
                    r"(?i)(?:^|[^\w])httpx\.(get|post|put|delete|head|patch|request|asyncclient|client)\s*[\(\.]",
                )
                .unwrap(),
                "httpx",
                Some(1),
            ),
            (Regex::new(r"(?:^|[^\w])fetch\s*\(").unwrap(), "fetch", None),
            (
                Regex::new(r"(?i)(?:^|[^\w])axios\.(get|post|put|delete|patch|create|request)\s*[\(\.]")
                    .unwrap(),
                "axios",
                Some(1),
            ),
            (
                Regex::new(r"(?:^|[^\w])http\.Get\s*\(").unwrap(),
                "http.Get",
                None,
            ),
            (
                Regex::new(r"(?:^|[^\w])http\.Post\s*\(").unwrap(),
                "http.Post",
                None,
            ),
            (
                Regex::new(r"(?:^|[^\w])http\.Client\b").unwrap(),
                "http.Client",
                None,
            ),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(attrs: &[ExtractedAttribute], key: &str) -> Vec<String> {
        attrs
            .iter()
            .filter(|a| a.key == key)
            .map(|a| a.value.clone())
            .collect()
    }

    #[test]
    fn extracts_db_table_skips_reserved() {
        let attrs = AttributeExtractor.extract("SELECT * FROM users WHERE id = 1");
        assert_eq!(values(&attrs, "db_table"), vec!["users".to_string()]);
    }

    #[test]
    fn extracts_env_var_python_and_ts_and_go() {
        let body = r#"os.getenv("HOME"); process.env.PATH; os.Getenv("GOPATH")"#;
        let vals = values(&AttributeExtractor.extract(body), "env_var");
        assert!(vals.contains(&"HOME".to_string()));
        assert!(vals.contains(&"PATH".to_string()));
        assert!(vals.contains(&"GOPATH".to_string()));
    }

    #[test]
    fn extracts_http_route_and_external_call() {
        let body = r#"router.get("/users", h); requests.get("https://api")"#;
        let attrs = AttributeExtractor.extract(body);
        assert!(values(&attrs, "http_route").contains(&"/users".to_string()));
        assert!(values(&attrs, "external_call").contains(&"requests.get".to_string()));
    }

    #[test]
    fn dedupes_by_key_value() {
        let attrs = AttributeExtractor.extract("fetch(a); fetch(b)");
        assert_eq!(values(&attrs, "external_call"), vec!["fetch".to_string()]);
    }

    #[test]
    fn empty_body_no_attributes() {
        assert!(AttributeExtractor.extract("").is_empty());
    }
}
