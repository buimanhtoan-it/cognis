//! RRF fusion parity test (rust-engine-migration, task 5.2 / Requirement 4.1).
//!
//! Asserts `rrf_fuse` produces a fused top-k **byte-identical** to the Python
//! oracle (`cognis_retrieval.fusion`) on the same seed set — Property 2,
//! P-PAR-FUSE: `∀ hits: rrf_fuse_rust(hits) == fuse_py(hits) byte-identical`.
//!
//! The oracle is captured in `tests/fixtures/fusion_parity_golden.json` from the
//! Python oracle's `fuse_rankings` over a
//! set of synthetic seed sets, recording, per case, the input hits plus the
//! ordered `(symbol_id, score_hex)` fused output. `score_hex` is `float.hex()`
//! so we compare the raw IEEE-754 f64 bits (true byte-identity, no decimal
//! round-trip). Capturing the golden lets this run under plain `cargo test`
//! with no Python runtime, mirroring the FTS/vec parity tests in `cognis-store`.
//! The golden is checked in as frozen oracle output; there is no toolchain in
//! this repo to regenerate it.

use std::fs;
use std::path::PathBuf;

use cognis_core::Hit;
use cognis_retrieval::{rrf_fuse, rrf_fuse_ids};
use serde_json::Value;

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fusion_parity_golden.json")
}

fn load_golden() -> Value {
    let path = golden_path();
    assert!(
        path.exists(),
        "missing golden {path:?}; it is a checked-in frozen oracle fixture"
    );
    let text = fs::read_to_string(&path).expect("read golden");
    serde_json::from_str(&text).expect("parse golden json")
}

/// Reconstruct the per-layer `Vec<Hit>` input from a case's recorded flat hit
/// list, grouping by `layer` in first-appearance order — the exact partition
/// `rrf_fuse`'s caller would hand it, and the one the Python oracle groups by.
fn layers_from_case(case: &Value) -> Vec<Vec<Hit>> {
    let mut layer_order: Vec<String> = Vec::new();
    let mut groups: Vec<Vec<Hit>> = Vec::new();
    for h in case["hits"].as_array().expect("case.hits") {
        let symbol_id = h["symbol_id"].as_str().expect("hit.symbol_id").to_string();
        let score = h["score"].as_f64().expect("hit.score");
        let layer = h["layer"].as_str().expect("hit.layer").to_string();
        let hit = Hit::new(symbol_id, score, layer.clone(), "t");
        match layer_order.iter().position(|l| l == &layer) {
            Some(idx) => groups[idx].push(hit),
            None => {
                layer_order.push(layer);
                groups.push(vec![hit]);
            }
        }
    }
    groups
}

/// Parse a golden `score_hex` (Python `float.hex()`, e.g. `0x1.0a6c..p-5`) into
/// the exact f64 it denotes, so parity is asserted on raw bits.
fn parse_hex_f64(s: &str) -> f64 {
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let body = body
        .strip_prefix("0x")
        .or_else(|| body.strip_prefix("0X"))
        .expect("hex float prefix");
    let (mantissa, exp) = body.split_once('p').expect("hex float exponent");
    let exp: i32 = exp.parse().expect("exponent int");
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    let mut value = u128::from_str_radix(int_part, 16).expect("int part") as f64;
    let mut scale = 1.0_f64;
    for nibble in frac_part.chars() {
        scale /= 16.0;
        value += nibble.to_digit(16).expect("hex nibble") as f64 * scale;
    }
    let result = value * 2f64.powi(exp);
    if neg {
        -result
    } else {
        result
    }
}

#[test]
fn rrf_fuse_topk_byte_identical_to_python_oracle() {
    let golden = load_golden();
    let k = golden["k"].as_u64().expect("golden.k") as usize;
    let rrf_k = golden["rrf_k"].as_f64().expect("golden.rrf_k");
    let cases = golden["cases"].as_array().expect("golden.cases");
    assert!(!cases.is_empty(), "golden must contain at least one case");

    let mut checked_nonempty = 0usize;
    for case in cases {
        let name = case["name"].as_str().expect("case.name");
        let layers = layers_from_case(case);
        let fused = rrf_fuse(&layers, k, rrf_k);

        let expected = case["expected"].as_array().expect("case.expected");
        assert_eq!(
            fused.len(),
            expected.len(),
            "fused length diverges from oracle for case {name:?}"
        );

        for (i, (hit, exp)) in fused.iter().zip(expected.iter()).enumerate() {
            let exp_id = exp["symbol_id"].as_str().expect("expected.symbol_id");
            assert_eq!(
                hit.symbol_id, exp_id,
                "rank {i} symbol id diverges from oracle for case {name:?}"
            );

            // Byte-identity on the fused score: compare raw f64 bits against the
            // oracle's float.hex() value.
            let exp_score = parse_hex_f64(exp["score_hex"].as_str().expect("expected.score_hex"));
            assert_eq!(
                hit.score.to_bits(),
                exp_score.to_bits(),
                "rank {i} fused score not byte-identical to oracle for case {name:?}: \
                 rust={} ({:#018x}) vs py={} ({:#018x})",
                hit.score,
                hit.score.to_bits(),
                exp_score,
                exp_score.to_bits(),
            );

            // The fused hit carries the engine contract: layer + reason +
            // evidence the capsule composer (Task 5.3) consumes.
            assert_eq!(hit.layer, "fused", "rank {i} layer (case {name:?})");
            assert!(!hit.reason.is_empty(), "rank {i} reason (case {name:?})");
            assert_eq!(
                hit.evidence
                    .get("rrf_score")
                    .and_then(Value::as_f64)
                    .map(f64::to_bits),
                Some(hit.score.to_bits()),
                "rank {i} evidence.rrf_score (case {name:?})"
            );
        }

        // The id-only wrapper must agree with the oracle id ordering too.
        let ids = rrf_fuse_ids(&layers, k, rrf_k);
        let expected_ids: Vec<String> = case["expected_ids"]
            .as_array()
            .expect("case.expected_ids")
            .iter()
            .map(|v| v.as_str().expect("id string").to_string())
            .collect();
        assert_eq!(
            ids, expected_ids,
            "fused id ordering diverges from oracle for case {name:?}"
        );

        if !expected.is_empty() {
            checked_nonempty += 1;
        }
    }

    assert!(
        checked_nonempty >= 1,
        "golden should exercise at least one non-empty fusion"
    );
}
