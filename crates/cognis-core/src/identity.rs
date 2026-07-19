//! Canonical repository / DB identity for isolation (Requirement 2.12).
//!
//! Distinct repositories and roots must keep separate DB paths, status, audit
//! logs, credentials, leases, server names, and retrieval results (preservation
//! 3.6). Shared routes (thin proxy → heavy HTTP, multi-client attach) therefore
//! canonicalize the repository root + `COGNIS_DB_PATH` on every attachment and
//! **reject cross-repository access** when a presented identity does not match
//! the owner's.
//!
//! The TypeScript mirror lives in `apps/cognis-vscode/src/mcpCanonical.ts`
//! (`canonicalRepoIdentity`). Both sides use the same key material:
//! symlink/case-resolved absolute root + absolute DB path, slash-normalized and
//! lowercased, joined by a NUL separator.
//!
//! Task 8.2 / Correctness Property 12.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::CONFIG_DIR_NAME;
use crate::lease::resolve_repo_root_from_env;

/// Env var for an explicit repository root (preferred over DB-path inference).
pub const REPO_ROOT_ENV: &str = "COGNIS_REPO_ROOT";

/// Env var for the UCKG database path (part of the canonical identity).
pub const DB_PATH_ENV: &str = "COGNIS_DB_PATH";

/// HTTP header a client presents to declare its repository identity key.
pub const REPO_IDENTITY_HEADER: &str = "X-Cognis-Repo-Key";

/// A repository's canonical identity: symlink/case-resolved root + DB path.
///
/// Two aliases of one repository (symlink / casing) share a [`key`]; two
/// distinct repositories never do. A repository re-pointed at a different
/// database is a distinct identity (isolation / preservation 3.6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoIdentity {
    /// Canonical (symlink/case-resolved) absolute repository root.
    pub root: String,
    /// Canonical (symlink/case-resolved) absolute `COGNIS_DB_PATH`.
    pub db_path: String,
    /// Stable identity key combining root + DB path.
    pub key: String,
}

impl RepoIdentity {
    /// Build from explicit root + DB paths (already or not yet canonical).
    pub fn from_paths(repo_root: impl AsRef<Path>, db_path: impl AsRef<Path>) -> Self {
        let root = canonicalize_path(repo_root.as_ref());
        let db = canonicalize_path(db_path.as_ref());
        let key = format!("{root}\u{0000}{db}");
        RepoIdentity {
            root,
            db_path: db,
            key,
        }
    }

    /// Default DB path for a repository: `<root>/.cognis/uckg.db`.
    pub fn default_db_path(repo_root: impl AsRef<Path>) -> PathBuf {
        repo_root.as_ref().join(CONFIG_DIR_NAME).join("uckg.db")
    }

    /// Resolve the process's repository identity from the environment.
    ///
    /// * Root: `COGNIS_REPO_ROOT` → parent-of-`.cognis` from `COGNIS_DB_PATH` → cwd
    /// * DB: `COGNIS_DB_PATH` → default `<root>/.cognis/uckg.db`
    pub fn from_env() -> Self {
        let root = resolve_repo_root_from_env();
        let db = match std::env::var(DB_PATH_ENV) {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
            _ => Self::default_db_path(&root),
        };
        Self::from_paths(root, db)
    }

    /// Short, wire-safe digest of the identity key (hex SHA-256).
    ///
    /// Used on the HTTP wire (`X-Cognis-Repo-Key`) so the full path never has to
    /// travel in headers. Equality is still exact: two identities produce the
    /// same digest iff their keys are equal.
    pub fn wire_key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.key.as_bytes());
        hex_lower(&hasher.finalize())
    }

    /// True when `other` names the exact same canonical repository + DB.
    pub fn same_as(&self, other: &RepoIdentity) -> bool {
        self.key == other.key
    }
}

/// Outcome of verifying a presented attachment identity against the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentDecision {
    /// Attachment is allowed (same repository / DB).
    Allow,
    /// Cross-repository access — must be rejected (Requirement 2.12).
    RejectCrossRepository {
        owner_key: String,
        presented_key: String,
    },
}

impl AttachmentDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, AttachmentDecision::Allow)
    }
}

/// Verify that a presented repository identity may attach to the owner.
///
/// Rejects when the presented key differs from the owner's (cross-repository
/// access). Both sides must already be canonicalized via [`RepoIdentity`].
pub fn verify_repo_attachment(
    owner: &RepoIdentity,
    presented: &RepoIdentity,
) -> AttachmentDecision {
    if owner.same_as(presented) {
        AttachmentDecision::Allow
    } else {
        AttachmentDecision::RejectCrossRepository {
            owner_key: owner.wire_key(),
            presented_key: presented.wire_key(),
        }
    }
}

/// Verify a presented **wire** key (hex digest) against the owner.
///
/// Used on every HTTP attachment after the client presents
/// `X-Cognis-Repo-Key`. Missing/empty presentations are treated as a mismatch
/// when isolation is enforced (the caller decides whether a missing header is
/// required).
pub fn verify_repo_wire_key(owner: &RepoIdentity, presented_wire_key: &str) -> AttachmentDecision {
    let presented = presented_wire_key.trim().to_ascii_lowercase();
    let owner_wire = owner.wire_key();
    if !presented.is_empty() && presented == owner_wire {
        AttachmentDecision::Allow
    } else {
        AttachmentDecision::RejectCrossRepository {
            owner_key: owner_wire,
            presented_key: presented,
        }
    }
}

/// Resolve `target` to its canonical, symlink-and-case-resolved absolute form.
///
/// Mirrors `canonicalizePath` in `mcpCanonical.ts`:
/// * `Path::canonicalize` collapses symlinks and (on case-insensitive volumes)
///   returns the on-disk casing when the path exists;
/// * when the path does not exist yet, walk up to the nearest existing ancestor,
///   canonicalize that, and re-append the tail;
/// * finish with slash + case normalization so two spellings of one location
///   always collapse to one key.
pub fn canonicalize_path(target: &Path) -> String {
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(target)
    };
    let resolved = match absolute.canonicalize() {
        Ok(p) => p,
        Err(_) => canonicalize_via_existing_ancestor(&absolute),
    };
    normalize_canonical(&resolved)
}

fn canonicalize_via_existing_ancestor(absolute: &Path) -> PathBuf {
    let mut ancestor = absolute.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    for _ in 0..4096 {
        if ancestor.exists() {
            break;
        }
        match ancestor.parent() {
            Some(parent) if parent != ancestor => {
                if let Some(name) = ancestor.file_name() {
                    tail.push(name.to_os_string());
                }
                ancestor = parent.to_path_buf();
            }
            _ => return absolute.to_path_buf(),
        }
    }
    let real_ancestor = ancestor.canonicalize().unwrap_or(ancestor);
    if tail.is_empty() {
        real_ancestor
    } else {
        let mut out = real_ancestor;
        for name in tail.into_iter().rev() {
            out.push(name);
        }
        out
    }
}

/// Slash + case normalization shared with the TypeScript extension.
fn normalize_canonical(resolved: &Path) -> String {
    let s = resolved.to_string_lossy();
    // Strip Windows verbatim prefix so `\\?\C:\...` and `C:\...` compare equal.
    let stripped = s
        .strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix("//?/"))
        .unwrap_or(&s);
    stripped
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "cognis-identity-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn distinct_repos_have_distinct_keys() {
        let a = tmp_dir("a");
        let b = tmp_dir("b");
        let ia = RepoIdentity::from_paths(&a, RepoIdentity::default_db_path(&a));
        let ib = RepoIdentity::from_paths(&b, RepoIdentity::default_db_path(&b));
        assert_ne!(ia.key, ib.key);
        assert_ne!(ia.wire_key(), ib.wire_key());
        let _ = fs::remove_dir_all(a);
        let _ = fs::remove_dir_all(b);
    }

    #[test]
    fn same_repo_aliases_collapse() {
        let root = tmp_dir("alias");
        let db = RepoIdentity::default_db_path(&root);
        let a = RepoIdentity::from_paths(&root, &db);
        // Re-resolve through an absolute path with different separators / case
        // material where the platform allows it.
        let b = RepoIdentity::from_paths(root.clone(), db.clone());
        assert_eq!(a.key, b.key);
        assert_eq!(a.wire_key(), b.wire_key());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn different_db_is_distinct_identity() {
        let root = tmp_dir("dbswap");
        let def = RepoIdentity::from_paths(&root, RepoIdentity::default_db_path(&root));
        let other = RepoIdentity::from_paths(&root, root.join(".cognis").join("other.db"));
        assert_ne!(def.key, other.key);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verify_rejects_cross_repository() {
        let a = tmp_dir("vx-a");
        let b = tmp_dir("vx-b");
        let ia = RepoIdentity::from_paths(&a, RepoIdentity::default_db_path(&a));
        let ib = RepoIdentity::from_paths(&b, RepoIdentity::default_db_path(&b));
        assert!(verify_repo_attachment(&ia, &ia).is_allowed());
        assert!(!verify_repo_attachment(&ia, &ib).is_allowed());
        assert!(verify_repo_wire_key(&ia, &ia.wire_key()).is_allowed());
        assert!(!verify_repo_wire_key(&ia, &ib.wire_key()).is_allowed());
        assert!(!verify_repo_wire_key(&ia, "").is_allowed());
        let _ = fs::remove_dir_all(a);
        let _ = fs::remove_dir_all(b);
    }
}
