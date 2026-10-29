// Feature: non-code-artifact-coverage, Property 15: Name normalization is idempotent and convention-invariant
//
// Property 15 (Validates: Requirements 8.2):
//   For any identifier, CamelCase -> snake_case normalization via
//   `normalize_ident` is idempotent (`normalize(normalize(x)) == normalize(x)`),
//   and the CamelCase and snake_case spellings of the same logical identifier
//   normalize to the same string.
//
// The normalization rule (see `cognis_indexer::normalize_ident`): insert an
// underscore boundary at each lower->upper transition and at the end of a
// consecutive-uppercase run that begins a new word, then lowercase every
// segment and collapse consecutive/leading/trailing underscores. This test
// stays inside that rule's domain by generating identifiers over the ASCII
// letter/digit/underscore charset.

use cognis_indexer::normalize_ident;
use proptest::prelude::*;

/// Arbitrary identifier over the ASCII identifier charset: a mix of CamelCase,
/// snake_case, SCREAMING_SNAKE, digits, and underscores. Length 0..=24 so the
/// empty string and underscore-only edge cases are exercised too.
fn arb_identifier() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9_]{0,24}").unwrap()
}

/// A logical identifier: a non-empty sequence of lowercase word tokens. Tokens
/// are >= 2 chars so that CamelCase rendering (capitalize-first-letter) never
/// produces an ambiguous consecutive-uppercase run — CamelCase is genuinely
/// lossy for single-letter words, which is outside the convention-invariance
/// domain, not a defect in the normalizer.
fn arb_logical_tokens() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(
        proptest::string::string_regex("[a-z][a-z]{1,7}").unwrap(),
        1..=6,
    )
}

/// Render tokens in CamelCase: capitalize the first char of each token and
/// concatenate (e.g. ["user", "account"] -> "UserAccount").
fn to_camel_case(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| {
            let mut cs = t.chars();
            match cs.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Render tokens in snake_case: join with underscores (e.g. "user_account").
fn to_snake_case(tokens: &[String]) -> String {
    tokens.join("_")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Aspect 1 — idempotence: normalizing an already-normalized identifier is
    /// a no-op for any identifier in the rule's domain.
    #[test]
    fn normalize_ident_is_idempotent(id in arb_identifier()) {
        let once = normalize_ident(&id);
        let twice = normalize_ident(&once);
        prop_assert_eq!(
            &twice,
            &once,
            "normalize_ident must be idempotent: normalize({:?}) = {:?} but normalize(normalize) = {:?}",
            id, once, twice
        );
    }

    /// Aspect 2 — convention-invariance: the CamelCase and snake_case spellings
    /// of the same logical identifier normalize to the same string, namely the
    /// snake_case joining of the lowercase tokens.
    #[test]
    fn normalize_ident_is_convention_invariant(tokens in arb_logical_tokens()) {
        let camel = to_camel_case(&tokens);
        let snake = to_snake_case(&tokens);
        let expected = tokens.join("_");

        let norm_camel = normalize_ident(&camel);
        let norm_snake = normalize_ident(&snake);

        prop_assert_eq!(
            &norm_camel,
            &norm_snake,
            "CamelCase {:?} and snake_case {:?} must normalize identically",
            camel, snake
        );
        prop_assert_eq!(
            &norm_camel,
            &expected,
            "CamelCase {:?} must normalize to the snake_case joining {:?}",
            camel, expected
        );
    }
}
