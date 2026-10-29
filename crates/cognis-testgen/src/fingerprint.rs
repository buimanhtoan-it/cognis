//! `arb_fingerprint_pair` — vector-space fingerprint pairs.
//!
//! Design: "fingerprint pairs equal, differing in exactly one component, and
//! equal-dimension-but-different (the trap in 14.4)". Feeds P17, P18.
//!
//! Requirement 14.4 is the whole reason this generator is not just "two random
//! strings": the guard must treat two fingerprints with **equal embedding
//! dimension** and differing model identity, model checksum, tokenizer identity,
//! pooling configuration, or normalization mode as different. A generator that
//! only varied `dim` would pass a guard that compared nothing but `dim`.
//!
//! # Relationship to the production type (task 1.2 landed; 10.3 landed)
//!
//! The production `VectorSpaceFingerprint { digest, dim, model_id }` now exists
//! in `crates/cognis-store/src/publish.rs` (task 1.2), and
//! [`FingerprintSpec`] converts into it — see
//! `impl From<&FingerprintSpec> for VectorSpaceFingerprint` below. The spec is
//! **kept** rather than replaced because it additionally carries the five
//! components Requirement 14.4 enumerates, which is what lets the generator vary
//! exactly one of them; the production type only stores the derived `digest`
//! (plus `dim`/`model_id` for explaining a mismatch).
//!
//! `fingerprint_material` in `crates/cognis-embed` now includes pooling and
//! normalization mode, so the real `ModelFingerprint` derivation covers
//! everything [`FingerprintSpec::digest`] does.

use proptest::prelude::*;

use cognis_store::VectorSpaceFingerprint;

/// Mirror of the design's `VectorSpaceFingerprint`, expanded with the
/// components Requirement 14.4 enumerates so that "differing in exactly one
/// component" is expressible.
///
/// `digest` is derived from the components rather than generated independently,
/// which is what makes the equal-dimension-but-different case honest: two specs
/// that differ in tokenizer identity alone still produce different digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintSpec {
    /// Embedding dimension — the component a naive guard would compare.
    pub dim: usize,
    /// Model identity, e.g. `bge-small-en-v1.5`.
    pub model_id: String,
    /// Checksum of the model asset (`model.onnx`).
    pub model_checksum: String,
    /// Tokenizer identity / checksum (`tokenizer.json`).
    pub tokenizer_id: String,
    /// Pooling configuration (`pooling.json`) — added to `fingerprint_material`
    /// by task 10.3.
    pub pooling: String,
    /// Normalization mode — likewise added by task 10.3.
    pub normalization: String,
}

impl FingerprintSpec {
    /// The digest the guard actually compares. Deterministic in every component,
    /// so equality of digests is equality of embedding spaces.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        // Length-prefixed so no two distinct component tuples can serialise to
        // the same byte string.
        for component in [
            self.dim.to_string(),
            self.model_id.clone(),
            self.model_checksum.clone(),
            self.tokenizer_id.clone(),
            self.pooling.clone(),
            self.normalization.clone(),
        ] {
            hasher.update(component.len().to_le_bytes());
            hasher.update(component.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// A baseline fingerprint — the shipped default embedder.
    pub fn baseline() -> Self {
        FingerprintSpec {
            dim: 384,
            model_id: "bge-small-en-v1.5".to_string(),
            model_checksum: "a".repeat(64),
            tokenizer_id: "b".repeat(64),
            pooling: "mean".to_string(),
            normalization: "l2".to_string(),
        }
    }

    /// Which component names Requirement 14.4 enumerates, in a stable order.
    pub const COMPONENTS: [&'static str; 6] = [
        "dim",
        "model_id",
        "model_checksum",
        "tokenizer_id",
        "pooling",
        "normalization",
    ];

    /// Return a copy with exactly the named component changed.
    ///
    /// Every arm is a *rotation*, never an assignment to a fixed value: the
    /// checksum and tokenizer arms originally assigned constants, and when the
    /// generated base already held that constant the "changed" fingerprint came
    /// out identical to the original. That produced a pair labelled
    /// `OneComponentDiffers` whose two sides were equal — a mismatch case that
    /// would have quietly tested the *allow* path instead of the withhold path.
    /// The crate's own self-tests caught it; rotation makes a difference
    /// structural rather than incidental.
    pub fn with_component_changed(&self, component: &str) -> Self {
        let mut next = self.clone();
        match component {
            "dim" => next.dim = if next.dim == 384 { 768 } else { 384 },
            "model_id" => next.model_id = format!("{}-alt", next.model_id),
            "model_checksum" => next.model_checksum = rotate_digest(&next.model_checksum),
            "tokenizer_id" => next.tokenizer_id = rotate_digest(&next.tokenizer_id),
            "pooling" => {
                next.pooling = if next.pooling == "mean" {
                    "cls".to_string()
                } else {
                    "mean".to_string()
                }
            }
            "normalization" => {
                next.normalization = if next.normalization == "l2" {
                    "none".to_string()
                } else {
                    "l2".to_string()
                }
            }
            other => panic!("unknown fingerprint component: {other}"),
        }
        next
    }
}

/// Feed the generated fingerprint straight into the production type
/// (task 1.2).
///
/// The conversion is *lossy by design*: the production
/// `VectorSpaceFingerprint` stores the derived [`digest`](FingerprintSpec::digest)
/// — which is what the `Vector_Space_Guard` compares — plus `dim` and
/// `model_id` for explaining a mismatch. The remaining components
/// (`model_checksum`, `tokenizer_id`, `pooling`, `normalization`) are folded
/// **into** the digest, so no information the guard needs is dropped: two specs
/// that differ only in tokenizer identity still convert to two different
/// `VectorSpaceFingerprint`s.
///
/// That is exactly the Requirement 14.4 trap made structural — the converted
/// pair keeps its equal `dim` and its differing `digest`.
impl From<&FingerprintSpec> for VectorSpaceFingerprint {
    fn from(spec: &FingerprintSpec) -> Self {
        VectorSpaceFingerprint {
            digest: spec.digest(),
            dim: spec.dim,
            model_id: spec.model_id.clone(),
        }
    }
}

impl From<FingerprintSpec> for VectorSpaceFingerprint {
    fn from(spec: FingerprintSpec) -> Self {
        VectorSpaceFingerprint::from(&spec)
    }
}

impl FingerprintSpec {
    /// The production [`VectorSpaceFingerprint`] this spec describes.
    ///
    /// Convenience alias for the [`From`] impl, so a generator body reads
    /// `spec.to_fingerprint()` rather than a turbofished `.into()`.
    pub fn to_fingerprint(&self) -> VectorSpaceFingerprint {
        VectorSpaceFingerprint::from(self)
    }
}

impl FingerprintPair {
    /// Both sides converted to the production type, in `(persisted, query)`
    /// order — the two arguments the `Vector_Space_Guard` compares.
    pub fn to_fingerprints(&self) -> (VectorSpaceFingerprint, VectorSpaceFingerprint) {
        (self.persisted.to_fingerprint(), self.query.to_fingerprint())
    }
}

/// Advance every hex character by one, wrapping `f` → `0`. Guarantees a value
/// different from the input for any non-empty digest, regardless of what the
/// input was.
fn rotate_digest(digest: &str) -> String {
    if digest.is_empty() {
        return "0".to_string();
    }
    digest
        .chars()
        .map(|c| match c {
            'f' => '0',
            'z' => 'a',
            '9' => 'a',
            other if other.is_ascii_hexdigit() || other.is_ascii_alphanumeric() => {
                char::from_u32(other as u32 + 1).unwrap_or('0')
            }
            other => other,
        })
        .collect()
}

/// How the two fingerprints in a pair relate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintRelation {
    /// Byte-identical: the guard must `Allow`.
    Equal,
    /// Exactly one component differs — named so a counterexample says which.
    OneComponentDiffers { component: &'static str },
    /// Equal embedding dimension, different space: the Requirement 14.4 trap.
    /// The guard must `Withhold`, and a `dim`-only comparison would not.
    EqualDimensionDifferentSpace { component: &'static str },
}

/// A generated pair: the persisted fingerprint, the query fingerprint, and how
/// they relate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintPair {
    pub persisted: FingerprintSpec,
    pub query: FingerprintSpec,
    pub relation: FingerprintRelation,
}

impl FingerprintPair {
    /// Whether the guard is required to withhold semantic results for this pair.
    pub fn must_withhold(&self) -> bool {
        !matches!(self.relation, FingerprintRelation::Equal)
    }

    /// Whether both sides share an embedding dimension — the case where a
    /// dimension-only check would wrongly allow scoring.
    pub fn same_dimension(&self) -> bool {
        self.persisted.dim == self.query.dim
    }
}

/// Any single fingerprint.
pub fn arb_fingerprint() -> impl Strategy<Value = FingerprintSpec> {
    (
        prop::sample::select(vec![128usize, 256, 384, 768, 1024]),
        prop::sample::select(vec![
            "bge-small-en-v1.5",
            "bge-base-en-v1.5",
            "all-MiniLM-L6-v2",
            "e5-small-v2",
        ]),
        prop::sample::select(vec!["a", "c", "e"]),
        prop::sample::select(vec!["b", "d", "f"]),
        prop::sample::select(vec!["mean", "cls", "max"]),
        prop::sample::select(vec!["l2", "none"]),
    )
        .prop_map(
            |(dim, model_id, model_checksum, tokenizer_id, pooling, normalization)| {
                FingerprintSpec {
                    dim,
                    model_id: model_id.to_string(),
                    model_checksum: model_checksum.repeat(64),
                    tokenizer_id: tokenizer_id.repeat(64),
                    pooling: pooling.to_string(),
                    normalization: normalization.to_string(),
                }
            },
        )
}

/// Components other than `dim`. Changing one of these while holding `dim` fixed
/// is exactly the Requirement 14.4 trap.
const NON_DIM_COMPONENTS: [&str; 5] = [
    "model_id",
    "model_checksum",
    "tokenizer_id",
    "pooling",
    "normalization",
];

/// `arb_fingerprint_pair` — equal, one-component-different, and
/// equal-dimension-but-different pairs.
///
/// Weighted 2 : 3 : 3 so the two mismatch shapes together dominate: a guard bug
/// is a false *allow*, and the equal case can only ever catch a false withhold.
pub fn arb_fingerprint_pair() -> impl Strategy<Value = FingerprintPair> {
    prop_oneof![
        // Equal — the guard must allow.
        2 => arb_fingerprint().prop_map(|fp| FingerprintPair {
            persisted: fp.clone(),
            query: fp,
            relation: FingerprintRelation::Equal,
        }),
        // Exactly one component differs (possibly `dim`).
        3 => (
            arb_fingerprint(),
            prop::sample::select(FingerprintSpec::COMPONENTS.to_vec()),
        )
            .prop_map(|(persisted, component)| {
                let query = persisted.with_component_changed(component);
                FingerprintPair {
                    persisted,
                    query,
                    relation: FingerprintRelation::OneComponentDiffers { component },
                }
            }),
        // Equal dimension, different space — the 14.4 trap.
        3 => (
            arb_fingerprint(),
            prop::sample::select(NON_DIM_COMPONENTS.to_vec()),
        )
            .prop_map(|(persisted, component)| {
                let query = persisted.with_component_changed(component);
                debug_assert_eq!(persisted.dim, query.dim);
                FingerprintPair {
                    persisted,
                    query,
                    relation: FingerprintRelation::EqualDimensionDifferentSpace { component },
                }
            }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Digest equality tracks component equality in both directions — the
        /// property the guard's correctness rests on.
        #[test]
        fn digest_tracks_components(pair in arb_fingerprint_pair()) {
            let same = pair.persisted == pair.query;
            prop_assert_eq!(same, pair.persisted.digest() == pair.query.digest());
        }

        /// A pair that must be withheld really does differ, and the trap case
        /// really does share a dimension.
        #[test]
        fn relations_are_honest(pair in arb_fingerprint_pair()) {
            match &pair.relation {
                FingerprintRelation::Equal => {
                    prop_assert!(!pair.must_withhold());
                    prop_assert_eq!(&pair.persisted, &pair.query);
                }
                FingerprintRelation::OneComponentDiffers { .. } => {
                    prop_assert!(pair.must_withhold());
                    prop_assert_ne!(&pair.persisted, &pair.query);
                }
                FingerprintRelation::EqualDimensionDifferentSpace { component } => {
                    prop_assert!(pair.must_withhold());
                    prop_assert!(pair.same_dimension(), "trap case must share dim");
                    prop_assert!(NON_DIM_COMPONENTS.contains(component));
                    prop_assert_ne!(pair.persisted.digest(), pair.query.digest());
                }
            }
        }

        /// Conversion into the production `VectorSpaceFingerprint` preserves
        /// the only thing the guard compares: digest equality still tracks
        /// component equality, and the equal-dimension trap survives the
        /// conversion (equal `dim`, differing `digest`).
        #[test]
        fn conversion_preserves_guard_semantics(pair in arb_fingerprint_pair()) {
            let (persisted, query) = pair.to_fingerprints();
            prop_assert_eq!(persisted.digest == query.digest, !pair.must_withhold());
            prop_assert_eq!(&persisted.digest, &pair.persisted.digest());
            prop_assert_eq!(persisted.dim, pair.persisted.dim);
            prop_assert_eq!(&persisted.model_id, &pair.persisted.model_id);
            if let FingerprintRelation::EqualDimensionDifferentSpace { .. } = pair.relation {
                prop_assert_eq!(persisted.dim, query.dim);
                prop_assert_ne!(persisted.digest, query.digest);
            }
        }

        /// Changing exactly one component changes exactly that component.
        #[test]
        fn one_component_change_is_surgical(
            base in arb_fingerprint(),
            component in prop::sample::select(FingerprintSpec::COMPONENTS.to_vec()),
        ) {
            let changed = base.with_component_changed(component);
            let differing: Vec<&str> = FingerprintSpec::COMPONENTS
                .iter()
                .copied()
                .filter(|name| !component_equal(&base, &changed, name))
                .collect();
            prop_assert_eq!(differing, vec![component]);
        }
    }

    fn component_equal(a: &FingerprintSpec, b: &FingerprintSpec, name: &str) -> bool {
        match name {
            "dim" => a.dim == b.dim,
            "model_id" => a.model_id == b.model_id,
            "model_checksum" => a.model_checksum == b.model_checksum,
            "tokenizer_id" => a.tokenizer_id == b.tokenizer_id,
            "pooling" => a.pooling == b.pooling,
            "normalization" => a.normalization == b.normalization,
            other => panic!("unknown component {other}"),
        }
    }
}
