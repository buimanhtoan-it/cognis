//! Secret detector for the enricher stage (Task 8.2, Requirement 9.3).
//!
//! Rust mirror of `cognis_indexer.enricher.secrets.SecretDetector`. Two
//! complementary strategies, applied in order:
//!
//! 1. **Known-shape regex patterns** — AWS access keys, GitHub PATs, Slack
//!    tokens, Google API keys, OpenAI keys, JWTs, PEM private-key headers, DSNs
//!    with embedded credentials, then password/secret assignment.
//! 2. **Shannon-entropy threshold** — any remaining quoted string literal that
//!    is ≥ 16 chars and ≥ 4.5 bits/char of entropy is flagged.
//!
//! Each match is replaced in place with `[REDACTED:<type>]`; the original
//! string is **never** returned. The enricher runs this **before** the Writer
//! persists anything (Requirement 9.3).

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

/// High-entropy threshold in bits per character.
const ENTROPY_THRESHOLD: f64 = 4.5;
/// Minimum length for the entropy check to apply.
const ENTROPY_MIN_LEN: usize = 16;

/// Known-shape `(regex, label)` patterns, most specific first (mirror
/// `secrets._PATTERNS`).
fn patterns() -> &'static [(Regex, &'static str)] {
    static PATS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATS.get_or_init(|| {
        vec![
            (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "aws-access-key"),
            (Regex::new(r"ghp_[A-Za-z0-9]{36}").unwrap(), "github-pat"),
            (
                Regex::new(r"xox[baprs]-[A-Za-z0-9-]{10,}").unwrap(),
                "slack-token",
            ),
            (
                Regex::new(r"AIza[0-9A-Za-z\-_]{35}").unwrap(),
                "google-api-key",
            ),
            (
                Regex::new(r"sk-(?:proj-)?[A-Za-z0-9]{20,}").unwrap(),
                "openai-key",
            ),
            (
                Regex::new(r"eyJ[A-Za-z0-9_=-]+\.[A-Za-z0-9_=-]+\.?[A-Za-z0-9_.+/=-]*").unwrap(),
                "jwt",
            ),
            (
                Regex::new(r"-----BEGIN [A-Z ]+PRIVATE KEY-----").unwrap(),
                "pem-private-key-header",
            ),
            (
                Regex::new(r##"[a-zA-Z][a-zA-Z0-9+.\-]*://[^/\s:@]*:[^/\s:@]+@[^\s'"#]+"##)
                    .unwrap(),
                "dsn-with-credentials",
            ),
        ]
    })
}

/// `password = "..."` / `token: '...'` assignment (mirror `_PASSWORD_RE`).
fn password_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(password|passwd|pwd|secret|api[_\-]?key|token)\s*[:=]\s*["']([^"'\n]{4,})["']"#)
            .unwrap()
    })
}

/// Quoted string literal of 16+ chars (single or double) for the entropy scan.
fn quoted_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"'([^'\n]{16,})'|"([^"\n]{16,})""#).unwrap())
}

/// Detect and redact secret-shaped strings. Stateless; safe to share.
#[derive(Debug, Default)]
pub struct SecretDetector;

impl SecretDetector {
    /// Replace every secret-shaped substring in `text` with `[REDACTED:<type>]`.
    ///
    /// Returns `(redacted_text, types_found)` where `types_found` lists unique
    /// redaction labels in first-occurrence order. When nothing matches the
    /// original text is returned unchanged with an empty list.
    pub fn redact(&self, text: &str) -> (String, Vec<String>) {
        if text.is_empty() {
            return (text.to_string(), Vec::new());
        }

        let mut out = text.to_string();
        let mut found: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut record = |label: &str, found: &mut Vec<String>| {
            if seen.insert(label.to_string()) {
                found.push(label.to_string());
            }
        };

        // 1. Known-shape regex patterns (sequential, each over the running text).
        for (pattern, label) in patterns() {
            if pattern.is_match(&out) {
                record(label, &mut found);
                out = pattern
                    .replace_all(&out, format!("[REDACTED:{label}]").as_str())
                    .into_owned();
            }
        }

        // 2. Password / secret assignment — keep the keyword, redact the value.
        if password_re().is_match(&out) {
            record("password-assignment", &mut found);
            out = password_re()
                .replace_all(&out, |caps: &regex::Captures| {
                    format!("{}=\"[REDACTED:password-assignment]\"", &caps[1])
                })
                .into_owned();
        }

        // 3. Entropy scan on any quoted strings that survived steps 1+2.
        let mut high_entropy_seen = false;
        out = quoted_re()
            .replace_all(&out, |caps: &regex::Captures| {
                let (value, quote) = match (caps.get(1), caps.get(2)) {
                    (Some(m), _) => (m.as_str(), '\''),
                    (_, Some(m)) => (m.as_str(), '"'),
                    _ => return caps[0].to_string(),
                };
                if is_high_entropy(value) {
                    high_entropy_seen = true;
                    format!("{quote}[REDACTED:high-entropy]{quote}")
                } else {
                    caps[0].to_string()
                }
            })
            .into_owned();
        if high_entropy_seen {
            record("high-entropy", &mut found);
        }

        (out, found)
    }
}

/// Shannon entropy (bits per character) of `value`; `0.0` for empty input.
pub fn shannon_entropy(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<char, usize> = HashMap::new();
    let mut total = 0.0f64;
    for ch in value.chars() {
        *counts.entry(ch).or_insert(0) += 1;
        total += 1.0;
    }
    -counts
        .values()
        .map(|&n| {
            let p = n as f64 / total;
            p * p.log2()
        })
        .sum::<f64>()
}

/// `true` when `value` looks like a secret on entropy alone (≥ 16 chars AND
/// entropy ≥ 4.5 bits/char).
pub fn is_high_entropy(value: &str) -> bool {
    value.chars().count() >= ENTROPY_MIN_LEN && shannon_entropy(value) >= ENTROPY_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_aws_access_key() {
        let (out, types) = SecretDetector.redact("key = 'AKIAIOSFODNN7EXAMPLE'");
        assert!(out.contains("[REDACTED:aws-access-key]"));
        assert!(types.contains(&"aws-access-key".to_string()));
    }

    #[test]
    fn redacts_password_assignment_keeps_keyword() {
        let (out, types) = SecretDetector.redact(r#"password = "hunter2secret""#);
        assert!(out.contains("password=\"[REDACTED:password-assignment]\""));
        assert_eq!(types, vec!["password-assignment".to_string()]);
    }

    #[test]
    fn redacts_dsn_with_credentials() {
        let (out, types) = SecretDetector.redact("url = 'postgres://user:s3cr3t@db:5432/app'");
        assert!(out.contains("[REDACTED:dsn-with-credentials]"));
        assert!(types.contains(&"dsn-with-credentials".to_string()));
    }

    #[test]
    fn high_entropy_quoted_string_flagged() {
        // 24 distinct chars → entropy log2(24) ≈ 4.58 ≥ 4.5 threshold.
        let secret = "aB3dE6gH9jK2mN5pQ8sT1vW4";
        let (out, types) = SecretDetector.redact(&format!("nonce = '{secret}'"));
        assert!(types.contains(&"high-entropy".to_string()));
        assert!(out.contains("[REDACTED:high-entropy]"));
    }

    #[test]
    fn clean_text_unchanged() {
        let text = "def add(a, b):\n    return a + b";
        let (out, types) = SecretDetector.redact(text);
        assert_eq!(out, text);
        assert!(types.is_empty());
    }

    #[test]
    fn empty_entropy_is_zero() {
        assert_eq!(shannon_entropy(""), 0.0);
        assert!(!is_high_entropy("short"));
    }
}
