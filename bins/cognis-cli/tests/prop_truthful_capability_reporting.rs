use cognis_cli::{
    build_handshake_payload, build_health_report, build_health_report_with_capabilities,
};
use cognis_core::{handshake_payload_with_capabilities, Capabilities};
use proptest::prelude::*;

fn arb_repo_state() -> impl Strategy<Value = (String, u8)> {
    (
        proptest::string::string_regex("[a-z0-9_-]{1,16}").unwrap(),
        0u8..4,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: cognis-one-toggle-engine, Property 35: Capabilities are reported truthfully.
    /// **Validates: Requirements 6.3**
    #[test]
    fn handshake_and_health_report_the_running_binary_capabilities(
        semantic_backend in any::<bool>(),
        accelerated_vector_search in any::<bool>(),
        (repo_name, repo_state) in arb_repo_state(),
    ) {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join(repo_name);
        if repo_state >= 1 {
            std::fs::create_dir_all(&repo).unwrap();
        }
        if repo_state >= 2 {
            std::fs::create_dir_all(repo.join(".cognis")).unwrap();
        }
        if repo_state == 3 {
            std::fs::write(repo.join(".cognis/uckg.db"), b"not a sqlite database").unwrap();
        }

        let configured = Capabilities {
            semantic_backend,
            accelerated_vector_search,
        };
        let configured_handshake = serde_json::to_value(
            handshake_payload_with_capabilities(configured),
        ).unwrap();
        let configured_health = serde_json::to_value(
            build_health_report_with_capabilities(&repo, configured),
        ).unwrap();
        let configured_expected = serde_json::to_value(configured).unwrap();

        prop_assert_eq!(&configured_handshake["capabilities"], &configured_expected);
        prop_assert_eq!(&configured_health["capabilities"], &configured_expected);

        let actual = cognis_cli::resolve_running_capabilities();
        let actual_expected = serde_json::to_value(actual).unwrap();
        let actual_handshake = serde_json::to_value(build_handshake_payload()).unwrap();
        let actual_health = serde_json::to_value(build_health_report(&repo)).unwrap();

        prop_assert_eq!(&actual_handshake["capabilities"], &actual_expected);
        prop_assert_eq!(&actual_health["capabilities"], &actual_expected);
    }
}
