//! `arb_path_spelling` — alternative spellings of one directory.
//!
//! Design: "case variants, trailing separators, symlinks, UNC and verbatim
//! (`\\?\`) Windows prefixes". Feeds P21 (Requirement 16.5: "any two spellings
//! of the same directory resolve to one identity while distinct directories stay
//! distinct").
//!
//! A [`PathSpelling`] is a *rewrite* of a base path, not a free-standing path.
//! That is what makes the property checkable in both directions: two spellings
//! derived from the same base must canonicalize equal, and spellings derived
//! from different bases must not.
//!
//! # Platform note
//!
//! Symlink and UNC spellings cannot always be *materialised* — creating a
//! symlink on Windows needs Developer Mode or elevation, and a UNC path needs a
//! share. [`PathSpelling::is_materializable`] says whether
//! [`PathSpelling::materialize`] can be expected to succeed on this platform, so
//! a property can exercise the string-level rewrites everywhere and the on-disk
//! ones only where they are possible, rather than being silently skipped.

use std::io;
use std::path::{Path, PathBuf};

use proptest::prelude::*;

/// One way of rewriting a base path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpellingKind {
    /// The base path, unchanged. The identity element.
    AsGiven,
    /// Every ASCII letter upper-cased.
    UpperCase,
    /// Every ASCII letter lower-cased.
    LowerCase,
    /// Alternating case, which no real user types but every case-folding bug
    /// notices.
    MixedCase,
    /// One trailing separator appended.
    TrailingSeparator,
    /// Several trailing separators appended.
    RepeatedTrailingSeparators,
    /// A redundant `.` component inserted (`a/./b`).
    DotComponent,
    /// A `..` round-trip inserted (`a/b/../b`).
    DotDotRoundTrip,
    /// Backslash separators — the Windows spelling of the same path.
    BackslashSeparators,
    /// Forward-slash separators.
    ForwardSlashSeparators,
    /// Windows verbatim prefix `\\?\C:\…`.
    VerbatimPrefix,
    /// UNC path `\\server\share\…`.
    Unc,
    /// Verbatim UNC `\\?\UNC\server\share\…`.
    ///
    /// **This is the strictest case.** `\\?\UNC\server\share` and
    /// `\\server\share` name the same location, so Requirement 16.5 requires
    /// them to resolve to one identity. `canonicalize_path` in
    /// `crates/cognis-core/src/identity.rs` currently strips the `\\?\` prefix
    /// and leaves the literal `UNC` segment behind, so the two spellings do not
    /// currently collapse. The generator states the requirement rather than the
    /// current behaviour; P21 is where that gets adjudicated.
    VerbatimUnc,
    /// A symlink pointing at the base directory.
    Symlink,
}

impl SpellingKind {
    /// Every spelling kind.
    pub const ALL: [SpellingKind; 14] = [
        SpellingKind::AsGiven,
        SpellingKind::UpperCase,
        SpellingKind::LowerCase,
        SpellingKind::MixedCase,
        SpellingKind::TrailingSeparator,
        SpellingKind::RepeatedTrailingSeparators,
        SpellingKind::DotComponent,
        SpellingKind::DotDotRoundTrip,
        SpellingKind::BackslashSeparators,
        SpellingKind::ForwardSlashSeparators,
        SpellingKind::VerbatimPrefix,
        SpellingKind::Unc,
        SpellingKind::VerbatimUnc,
        SpellingKind::Symlink,
    ];

    /// Whether this rewrite is purely textual (no filesystem object needed).
    pub fn is_textual(&self) -> bool {
        !matches!(self, SpellingKind::Symlink)
    }

    /// Whether this spelling names a location that exists on the local
    /// filesystem. `Unc` and `VerbatimUnc` name a network share that a unit test
    /// will not have, so a property must compare them textually.
    pub fn is_local(&self) -> bool {
        !matches!(self, SpellingKind::Unc | SpellingKind::VerbatimUnc)
    }
}

/// One generated spelling of a base path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSpelling {
    /// The path this spelling was derived from.
    pub base: PathBuf,
    pub kind: SpellingKind,
    /// The rewritten path.
    pub spelled: PathBuf,
}

impl PathSpelling {
    /// Whether [`materialize`](Self::materialize) can be expected to succeed
    /// here: only the symlink spelling touches the filesystem, and only on a
    /// platform that permits unprivileged symlink creation.
    pub fn is_materializable(&self) -> bool {
        match self.kind {
            SpellingKind::Symlink => cfg!(unix),
            _ => self.kind.is_local(),
        }
    }

    /// Create the filesystem object this spelling needs, if any, and return the
    /// path to use.
    ///
    /// For every textual spelling this is a no-op returning [`Self::spelled`].
    /// For [`SpellingKind::Symlink`] it creates a symlink at
    /// [`Self::spelled`] pointing at [`Self::base`]; on Windows without the
    /// symlink privilege this returns the underlying `io::Error` so the caller
    /// can skip rather than fail.
    pub fn materialize(&self) -> io::Result<PathBuf> {
        if self.kind != SpellingKind::Symlink {
            return Ok(self.spelled.clone());
        }
        create_dir_symlink(&self.base, &self.spelled)?;
        Ok(self.spelled.clone())
    }

    /// Whether this spelling and `other` must resolve to the same identity:
    /// true exactly when they were derived from the same base
    /// (Requirement 16.5).
    pub fn must_match(&self, other: &PathSpelling) -> bool {
        self.base == other.base
    }
}

#[cfg(unix)]
fn create_dir_symlink(base: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(base, link)
}

#[cfg(windows)]
fn create_dir_symlink(base: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(base, link)
}

#[cfg(not(any(unix, windows)))]
fn create_dir_symlink(_base: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symlinks unsupported on this platform",
    ))
}

/// Apply a spelling rewrite to a base path.
pub fn spell(base: &Path, kind: SpellingKind) -> PathSpelling {
    let raw = base.to_string_lossy().to_string();
    let spelled = match kind {
        SpellingKind::AsGiven => raw.clone(),
        SpellingKind::UpperCase => raw.to_ascii_uppercase(),
        SpellingKind::LowerCase => raw.to_ascii_lowercase(),
        SpellingKind::MixedCase => alternate_case(&raw),
        SpellingKind::TrailingSeparator => format!("{raw}{}", std::path::MAIN_SEPARATOR),
        SpellingKind::RepeatedTrailingSeparators => {
            let sep = std::path::MAIN_SEPARATOR;
            format!("{raw}{sep}{sep}{sep}")
        }
        SpellingKind::DotComponent => insert_dot_component(&raw),
        SpellingKind::DotDotRoundTrip => insert_dotdot_round_trip(&raw),
        SpellingKind::BackslashSeparators => raw.replace('/', "\\"),
        SpellingKind::ForwardSlashSeparators => raw.replace('\\', "/"),
        SpellingKind::VerbatimPrefix => verbatim(&raw),
        SpellingKind::Unc => unc(&raw),
        SpellingKind::VerbatimUnc => verbatim_unc(&raw),
        SpellingKind::Symlink => symlink_sibling(base),
    };
    PathSpelling {
        base: base.to_path_buf(),
        kind,
        spelled: PathBuf::from(spelled),
    }
}

fn alternate_case(raw: &str) -> String {
    raw.chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

fn insert_dot_component(raw: &str) -> String {
    let sep = if raw.contains('\\') { '\\' } else { '/' };
    match raw.rsplit_once(sep) {
        Some((head, tail)) if !head.is_empty() && !tail.is_empty() => {
            format!("{head}{sep}.{sep}{tail}")
        }
        _ => format!("{raw}{sep}."),
    }
}

fn insert_dotdot_round_trip(raw: &str) -> String {
    let sep = if raw.contains('\\') { '\\' } else { '/' };
    match raw.rsplit_once(sep) {
        Some((head, tail)) if !head.is_empty() && !tail.is_empty() => {
            format!("{head}{sep}{tail}{sep}..{sep}{tail}")
        }
        _ => raw.to_string(),
    }
}

/// `C:\a\b` → `\\?\C:\a\b`. Already-verbatim and non-drive paths are returned
/// unchanged, so the rewrite never produces nonsense.
fn verbatim(raw: &str) -> String {
    if raw.starts_with(r"\\?\") || raw.starts_with("//?/") {
        return raw.to_string();
    }
    let windows_style = raw.replace('/', "\\");
    if has_drive_prefix(&windows_style) {
        format!(r"\\?\{windows_style}")
    } else {
        raw.to_string()
    }
}

/// `C:\a\b` → `\\localhost\C$\a\b` — the administrative-share spelling of a
/// local drive path.
fn unc(raw: &str) -> String {
    let windows_style = raw.replace('/', "\\");
    match drive_split(&windows_style) {
        Some((drive, rest)) => format!(r"\\localhost\{drive}${rest}"),
        None => windows_style,
    }
}

/// `C:\a\b` → `\\?\UNC\localhost\C$\a\b`.
fn verbatim_unc(raw: &str) -> String {
    let as_unc = unc(raw);
    match as_unc.strip_prefix(r"\\") {
        Some(rest) => format!(r"\\?\UNC\{rest}"),
        None => as_unc,
    }
}

fn has_drive_prefix(windows_style: &str) -> bool {
    drive_split(windows_style).is_some()
}

/// Split `C:\a\b` into `("C", "\a\b")`.
fn drive_split(windows_style: &str) -> Option<(char, &str)> {
    let mut chars = windows_style.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    if chars.next()? != ':' {
        return None;
    }
    Some((drive, &windows_style[2..]))
}

/// Where the symlink spelling puts its link: a sibling of the base directory.
fn symlink_sibling(base: &Path) -> String {
    let file_name = base
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    parent
        .join(format!("{file_name}_symlink"))
        .to_string_lossy()
        .to_string()
}

/// `arb_path_spelling` — a spelling of a caller-supplied base path.
///
/// The base is supplied rather than generated because P21 needs a *real*
/// directory (usually a `tempfile::TempDir`) to canonicalize against; a
/// generated path that does not exist would exercise the
/// nearest-existing-ancestor fallback instead of the case/symlink logic.
pub fn arb_path_spelling(base: PathBuf) -> impl Strategy<Value = PathSpelling> {
    prop::sample::select(SpellingKind::ALL.to_vec()).prop_map(move |kind| spell(&base, kind))
}

/// Two spellings of the same base, for the "any two spellings collapse to one
/// identity" direction of Requirement 16.5.
pub fn arb_path_spelling_pair(
    base: PathBuf,
) -> impl Strategy<Value = (PathSpelling, PathSpelling)> {
    (
        prop::sample::select(SpellingKind::ALL.to_vec()),
        prop::sample::select(SpellingKind::ALL.to_vec()),
    )
        .prop_map(move |(left, right)| (spell(&base, left), spell(&base, right)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            r"C:\repos\Cognis\workspace"
        } else {
            "/repos/Cognis/workspace"
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Every spelling records the base it came from, so `must_match` is
        /// well defined.
        #[test]
        fn spellings_remember_their_base(spelling in arb_path_spelling(base())) {
            prop_assert_eq!(&spelling.base, &base());
            prop_assert!(!spelling.spelled.as_os_str().is_empty());
        }

        /// Two spellings of one base must match; and the relation is symmetric.
        #[test]
        fn same_base_spellings_must_match(
            (left, right) in arb_path_spelling_pair(base()),
        ) {
            prop_assert!(left.must_match(&right));
            prop_assert!(right.must_match(&left));
        }

        /// Spellings of different bases must not be required to match — the
        /// "distinct directories stay distinct" half of Requirement 16.5.
        #[test]
        fn different_bases_must_not_match(kind in prop::sample::select(SpellingKind::ALL.to_vec())) {
            let other = base().join("nested");
            let left = spell(&base(), kind);
            let right = spell(&other, kind);
            prop_assert!(!left.must_match(&right));
        }
    }

    #[test]
    fn case_variants_differ_textually_but_share_a_base() {
        let upper = spell(&base(), SpellingKind::UpperCase);
        let lower = spell(&base(), SpellingKind::LowerCase);
        assert_ne!(upper.spelled, lower.spelled);
        assert!(upper.must_match(&lower));
    }

    #[test]
    fn trailing_separators_are_appended() {
        let one = spell(&base(), SpellingKind::TrailingSeparator);
        let many = spell(&base(), SpellingKind::RepeatedTrailingSeparators);
        let one_str = one.spelled.to_string_lossy().to_string();
        let many_str = many.spelled.to_string_lossy().to_string();
        assert!(one_str.ends_with(std::path::MAIN_SEPARATOR));
        assert!(many_str.len() > one_str.len());
    }

    #[test]
    fn windows_prefixes_are_well_formed() {
        // The rewrites are exercised on a drive-style path regardless of host
        // platform, so the Windows-specific spellings are generated on CI's
        // Linux runners too.
        let drive = PathBuf::from(r"C:\repos\cognis");
        let verbatim = spell(&drive, SpellingKind::VerbatimPrefix);
        assert_eq!(
            verbatim.spelled.to_string_lossy(),
            r"\\?\C:\repos\cognis",
            "verbatim prefix"
        );
        let unc = spell(&drive, SpellingKind::Unc);
        assert_eq!(
            unc.spelled.to_string_lossy(),
            r"\\localhost\C$\repos\cognis",
            "UNC share"
        );
        let verbatim_unc = spell(&drive, SpellingKind::VerbatimUnc);
        assert_eq!(
            verbatim_unc.spelled.to_string_lossy(),
            r"\\?\UNC\localhost\C$\repos\cognis",
            "verbatim UNC"
        );
    }

    #[test]
    fn symlink_spelling_is_a_sibling_of_the_base() {
        let spelling = spell(&base(), SpellingKind::Symlink);
        assert_eq!(spelling.spelled.parent(), base().parent());
        assert_ne!(spelling.spelled, base());
    }

    #[test]
    fn dot_and_dotdot_rewrites_keep_the_tail() {
        let dot = spell(&base(), SpellingKind::DotComponent);
        let dotdot = spell(&base(), SpellingKind::DotDotRoundTrip);
        assert!(dot.spelled.to_string_lossy().contains("workspace"));
        assert!(dotdot.spelled.to_string_lossy().contains(".."));
    }
}
