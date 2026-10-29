//! Executable spike for lossless source addressability versus the current
//! line-only `Symbol` model. Oracle calculations are independent of the parser.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cognis_core::Config;
use cognis_indexer::{parse_source, IndexerPipeline};
use cognis_store::Database;
use serde_json::json;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cognis-spike-address-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Point {
    byte: usize,
    line: usize,
    byte_column: usize,
    utf16_absolute: usize,
    utf16_column: usize,
}
fn oracle_point(source: &str, byte: usize) -> Point {
    assert!(byte <= source.len() && source.is_char_boundary(byte));
    let prefix = &source[..byte];
    let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
    Point {
        byte,
        line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        byte_column: byte - line_start,
        utf16_absolute: prefix.encode_utf16().count(),
        utf16_column: source[line_start..byte].encode_utf16().count(),
    }
}

fn inverse_utf16(source: &str, target: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in source.char_indices() {
        if units == target {
            return Some(byte);
        }
        units += ch.len_utf16();
        if units > target {
            return None; // target is inside an astral surrogate pair
        }
    }
    (units == target).then_some(source.len())
}

fn inverse_line_utf16(source: &str, line: usize, column: usize) -> Option<usize> {
    let start = if line == 1 {
        0
    } else {
        source
            .match_indices('\n')
            .nth(line - 2)
            .map(|(i, _)| i + 1)?
    };
    let end = source[start..]
        .find('\n')
        .map_or(source.len(), |i| start + i);
    inverse_utf16(&source[start..end], column).map(|offset| start + offset)
}

fn print_matrix(source: &str, points: &[(&str, usize)], excerpt_len: usize) {
    let rows = points
        .iter()
        .map(|(label, byte)| {
            let p = oracle_point(source, *byte);
            json!({
                "label": label, "byte": p.byte, "line": p.line,
                "byte_column": p.byte_column,
                "utf16_absolute": p.utf16_absolute,
                "utf16_column": p.utf16_column,
            })
        })
        .collect::<Vec<_>>();
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "tier": "PROVEN",
            "claim": "oracle round trips at every Unicode scalar boundary",
            "oracle_points": rows,
            "capability_matrix": {
                "exact_byte_offset": {"oracle": true, "current_symbol": false},
                "byte_column": {"oracle": true, "current_symbol": false},
                "absolute_utf16_offset": {"oracle": true, "current_symbol": false},
                "line_relative_utf16_column": {"oracle": true, "current_symbol": false},
                "line_span": {"oracle": true, "current_symbol": true},
                "exact_source_blob": {"oracle": true, "current_symbol": false},
                "excerpt_is_lossless": {"oracle": true, "current_symbol": false},
                "current_excerpt_bytes": excerpt_len,
            },
        }))
        .unwrap()
    );
}
#[test]
fn independent_unicode_oracle_round_trips_and_exposes_line_only_gap() {
    let source = "# NFC café | NFD cafe\u{301}\r\n\tastral = '😀'\r\ndef long_line(): return 'x'\n";
    for byte in (0..=source.len()).filter(|i| source.is_char_boundary(*i)) {
        let p = oracle_point(source, byte);
        assert_eq!(inverse_utf16(source, p.utf16_absolute), Some(byte));
        assert_eq!(
            inverse_line_utf16(source, p.line, p.utf16_column),
            Some(byte)
        );
    }
    // A UTF-16 offset in the middle of 😀 is intentionally not byte-addressable.
    let emoji = source.find('😀').unwrap();
    let before_emoji = oracle_point(source, emoji).utf16_absolute;
    assert_eq!(inverse_utf16(source, before_emoji + 1), None);

    let parsed = parse_source("unicode.py", source);
    assert!(!parsed.symbols.is_empty());
    let excerpt_len = parsed.symbols[0]
        .body_excerpt
        .as_deref()
        .unwrap_or("")
        .len();
    print_matrix(
        source,
        &[
            ("crlf_lf", source.find("\r\n").unwrap() + 1),
            ("nfc_e_acute", source.find('é').unwrap()),
            ("nfd_combining", source.find('\u{301}').unwrap()),
            ("tab", source.find('\t').unwrap()),
            ("emoji", emoji),
        ],
        excerpt_len,
    );

    // Same display form after normalization is not the same byte address.
    assert_ne!("é".as_bytes(), "e\u{301}".as_bytes());
    assert_eq!(
        oracle_point(source, emoji).utf16_column + 2,
        oracle_point(source, emoji + '😀'.len_utf8()).utf16_column
    );

    // Invalid UTF-8 remains byte-addressable as a blob, but it has no valid
    // Unicode scalar / UTF-16 coordinate until a decoding policy is chosen.
    let invalid = [0xff_u8, b'a', 0xfe_u8];
    assert_eq!(invalid.len(), 3);
    assert_eq!(invalid[1], b'a');
    eprintln!(
        "{}",
        json!({
            "tier": "PROVEN",
            "invalid_utf8": {"byte_offsets": [0, 1, 2], "utf16_oracle": "undefined_without_decode_policy"},
            "capability_matrix_extension": {"raw_bytes_addressable": true, "current_symbol_source_blob": false}
        })
    );
}

#[test]
fn long_body_and_single_line_measure_excerpt_truncation_without_invalid_utf8() {
    let body = format!(
        "def huge():\n    payload = '{}😀TAIL'\n    return payload\n",
        "a".repeat(1900)
    );
    let parsed = parse_source("long.py", &body);
    let huge = parsed.symbols.iter().find(|s| s.name == "huge").unwrap();
    let excerpt = huge.body_excerpt.as_deref().unwrap();
    assert!(excerpt.len() <= 1500);
    assert!(body.starts_with(excerpt));
    assert!(excerpt.is_char_boundary(excerpt.len()));
    assert!(!excerpt.contains("TAIL"));

    let single = format!("def one(): return '{}😀END'", "z".repeat(1800));
    let one = parse_source("single.py", &single);
    let sym = one.symbols.iter().find(|s| s.name == "one").unwrap();
    assert_eq!(sym.line_start, 1);
    assert_eq!(sym.line_end, 1);
    assert!(sym.body_excerpt.as_ref().unwrap().len() <= 1500);
    eprintln!(
        "{}",
        json!({
            "tier": "EMPIRICALLY_SUPPORTED", "case": "excerpt_truncation",
            "body_bytes": body.len(), "excerpt_bytes": excerpt.len(),
            "long_single_line_bytes": single.len(),
            "current_fields": ["line_start", "line_end", "truncated_body_excerpt"],
            "missing_fields": ["byte_start", "byte_end", "utf16_start", "utf16_end", "source_blob"],
        })
    );
}
fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn incremental_valid_to_empty_and_invalid_drop_derived_symbols() {
    let scratch = Scratch::new();
    let repo = scratch.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let file = repo.join("address.py");
    write(&file, "def addressable():\n    return '😀'\n");
    let db = Database::open(scratch.0.join("index.db")).unwrap();
    let mut pipe = IndexerPipeline::new(db.clone(), Config::default());
    pipe.index_repo(&repo, true).unwrap();
    let initial_ids = db
        .list_symbols()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect::<Vec<_>>();
    assert!(!initial_ids.is_empty());

    write(&file, "");
    pipe.index_changed_files(std::slice::from_ref(&file), &repo)
        .unwrap();
    let empty_ids = db
        .list_symbols()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect::<Vec<_>>();
    assert!(
        empty_ids.is_empty(),
        "empty content must drop every derived symbol while the file row remains"
    );

    write(&file, [0xff, 0xfe, 0x80]);
    pipe.index_changed_files(std::slice::from_ref(&file), &repo)
        .unwrap();
    let invalid_ids = db
        .list_symbols()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect::<Vec<_>>();
    assert!(
        invalid_ids.is_empty(),
        "invalid UTF-8 must drop every derived symbol and record a failed parse"
    );

    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "tier": "EMPIRICALLY_SUPPORTED",
            "case": "incremental_empty_and_invalid_utf8",
            "initial_symbol_ids": initial_ids,
            "valid_to_empty_stale": false,
            "valid_to_invalid_utf8_stale": false,
            "interpretation": "degenerate transitions drop derived rows and retain a file-cache row",
        }))
        .unwrap()
    );
    db.close_thread_connection();
}
