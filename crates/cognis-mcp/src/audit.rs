//! Hashed-argument audit log (Requirement 3.4).
//!
//! Rust mirror of `apps/cognis-mcpd/cognis_mcpd/audit.py`. Every tool call
//! appends one JSON line to `.cognis/audit.log`:
//!
//! ```json
//! {"ts":"2024-01-02T03:04:05Z","tool":"symbol_search","args_hash":"<sha256>","ok":true}
//! ```
//!
//! The raw arguments are **never** written — only a SHA-256 of their canonical
//! JSON serialization — so a query string or path that may contain a secret is
//! recorded as an opaque, correlatable hash rather than cleartext. Audit
//! failures are swallowed: logging must never prevent a tool from returning a
//! result to the client (design § Error Handling).
//!
//! ## Hash note (not a contract surface)
//!
//! The extension never reads the audit log, so the hash is *not* part of the
//! invariant JSON contract and need not be byte-identical to the Python
//! `hashlib.sha256(json.dumps(args, sort_keys=True))`. We hash the canonical
//! (sorted-key, compact) `serde_json` rendering — stable across runs for the
//! same arguments, which is all the audit trail needs.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Default audit log path (relative to the working directory).
pub fn default_audit_path() -> PathBuf {
    Path::new(".cognis").join("audit.log")
}

/// SHA-256 (hex) of the canonical JSON serialization of `args`.
///
/// `serde_json` renders object keys in sorted order by default (no
/// `preserve_order` feature), giving a stable, sorted-key encoding analogous to
/// Python's `json.dumps(args, sort_keys=True)`. The raw argument *values* never
/// leave this function — only the digest does.
pub fn hash_args(args: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(args).unwrap_or_else(|_| "null".to_string());
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Format a `SystemTime` as an ISO-8601 UTC instant (`YYYY-MM-DDTHH:MM:SSZ`),
/// matching the Python `time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())`.
///
/// Uses the civil-from-days algorithm (Howard Hinnant) so no date/time crate is
/// needed. A pre-epoch time (clock skew) renders as the epoch.
fn iso8601_utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;

    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hour, minute, second) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );

    // days since 1970-01-01 → civil (y, m, d). Algorithm by Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// An append-only audit sink bound to a log path.
///
/// Cloning is cheap (clones the path). Writes are best-effort: any IO error is
/// silently dropped so the audit trail never blocks a tool result.
#[derive(Debug, Clone)]
pub struct AuditLog {
    path: PathBuf,
}

impl Default for AuditLog {
    fn default() -> Self {
        AuditLog {
            path: default_audit_path(),
        }
    }
}

impl AuditLog {
    /// Bind an audit log to `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        AuditLog { path: path.into() }
    }

    /// The bound audit log path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry: `tool`, the SHA-256 of `args`, and the `ok` flag.
    ///
    /// Best-effort — a missing parent directory is created; any failure is
    /// swallowed (the call returns `()` regardless) so auditing never sinks a
    /// tool result.
    pub fn record(&self, tool: &str, args: &serde_json::Value, ok: bool) {
        let _ = self.try_record(tool, args, ok);
    }

    /// The fallible core of [`record`], surfaced for tests. Builds the JSONL
    /// entry and appends it.
    ///
    /// [`record`]: AuditLog::record
    pub fn try_record(
        &self,
        tool: &str,
        args: &serde_json::Value,
        ok: bool,
    ) -> std::io::Result<()> {
        let entry = serde_json::json!({
            "ts": iso8601_utc(SystemTime::now()),
            "tool": tool,
            "args_hash": hash_args(args),
            "ok": ok,
        });
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(&entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_order_independent() {
        let a = serde_json::json!({"query": "secret-token", "k": 10});
        let b = serde_json::json!({"k": 10, "query": "secret-token"});
        // Sorted-key canonicalization ⇒ key order does not change the hash.
        assert_eq!(hash_args(&a), hash_args(&b));
        // 64 hex chars = 32 bytes of SHA-256.
        assert_eq!(hash_args(&a).len(), 64);
    }

    #[test]
    fn hash_differs_for_different_args() {
        let a = serde_json::json!({"query": "alpha"});
        let b = serde_json::json!({"query": "beta"});
        assert_ne!(hash_args(&a), hash_args(&b));
    }

    #[test]
    fn iso8601_known_epochs() {
        assert_eq!(iso8601_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // 1_700_000_000 = 2023-11-14T22:13:20Z
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(iso8601_utc(t), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn record_never_logs_raw_args() {
        let dir = std::env::temp_dir().join(format!("cognis-audit-{}", std::process::id()));
        let path = dir.join("audit.log");
        let _ = std::fs::remove_file(&path);
        let log = AuditLog::new(&path);

        let args = serde_json::json!({"query": "super-secret-password", "k": 5});
        log.record("symbol_search", &args, true);
        log.record("symbol_search", &args, false);

        let contents = std::fs::read_to_string(&path).unwrap();
        // Two JSONL entries, neither containing the raw secret.
        assert_eq!(contents.lines().count(), 2);
        assert!(
            !contents.contains("super-secret-password"),
            "raw argument leaked into audit log: {contents}"
        );
        let first: serde_json::Value =
            serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(first["tool"], "symbol_search");
        assert_eq!(first["ok"], true);
        assert_eq!(first["args_hash"], hash_args(&args));
        assert!(first["ts"].as_str().unwrap().ends_with('Z'));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
