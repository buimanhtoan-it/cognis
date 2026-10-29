//! Pure synthetic adversarial retrieval constructions. Each test either pins a
//! construction-level invariant or explicitly records a finite counterexample.

use std::collections::{BTreeSet, HashMap};

use cognis_core::{Hit, Symbol, SymbolKind};
use cognis_eval::bench::{compute_metrics, LexicalIndex};
use cognis_retrieval::{compose_capsule, rrf_fuse, DEFAULT_RRF_K};
use serde_json::json;

fn symbol(id: &str, name: &str, path: &str, body: &str) -> Symbol {
    Symbol {
        id: id.into(),
        kind: SymbolKind::Function,
        name: name.into(),
        qualified_name: name.into(),
        language: "python".into(),
        module: path.rsplit_once('/').map_or("root", |x| x.0).into(),
        file_path: path.into(),
        line_start: 1,
        line_end: 1,
        signature: None,
        docstring: None,
        content_hash: "h".into(),
        body_excerpt: Some(body.into()),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: vec![],
        updated_at: 0,
    }
}

fn hit(id: &str, score: f64, layer: &str) -> Hit {
    Hit::new(id, score, layer, "adversarial")
}
fn rank(symbols: &[Symbol], query: &str) -> Vec<String> {
    LexicalIndex::from_symbols(symbols).rank(query)
}
fn position(order: &[String], id: &str) -> Option<usize> {
    order.iter().position(|x| x == id)
}

#[test]
fn query_term_stuffing_in_irrelevant_module_card_is_a_finite_counterexample() {
    let docs = [
        symbol("relevant", "authorize", "src/auth.rs", "verify credential"),
        symbol(
            "stuffed",
            "module_card",
            "docs/map.md",
            &"authorize verify credential ".repeat(30),
        ),
    ];
    let order = rank(&docs, "authorize verify credential");
    assert_eq!(order[0], "stuffed");
    eprintln!(
        "{}",
        json!({"tier":"EMPIRICALLY_SUPPORTED","counterexample":"query_term_stuffing","order":order})
    );
}

#[test]
fn broad_parent_module_can_drown_exact_symbol_under_rrf() {
    let exact = vec![hit("exact", 10.0, "symbol"), hit("other", 9.0, "symbol")];
    let parent_a = vec![hit("broad", 1.0, "module_a"), hit("exact", 0.5, "module_a")];
    let parent_b = vec![hit("broad", 1.0, "module_b"), hit("noise", 0.5, "module_b")];
    let fused = rrf_fuse(&[exact, parent_a, parent_b], 4, DEFAULT_RRF_K);
    assert_eq!(fused[0].symbol_id, "broad");
    eprintln!(
        "{}",
        json!({"tier":"EMPIRICALLY_SUPPORTED","counterexample":"broad_parent_drowning_exact_symbol","order":fused.iter().map(|h|&h.symbol_id).collect::<Vec<_>>()})
    );
}

#[test]
fn duplicate_names_are_path_disambiguated_but_name_only_query_is_ambiguous() {
    let docs = [
        symbol("a", "parse", "src/alpha/parser.py", "parse input"),
        symbol("b", "parse", "src/beta/parser.py", "parse input"),
    ];
    let ambiguous = rank(&docs, "parse");
    assert_eq!(
        ambiguous,
        vec!["a", "b"],
        "tie is deterministic by id, not semantic resolution"
    );
    let disambiguated = rank(&docs, "beta parser parse");
    assert_eq!(disambiguated[0], "b");
}
#[test]
fn long_irrelevant_hit_exposes_naive_budget_starvation_and_skip_control() {
    let costs = HashMap::from([("long", 100usize), ("relevant", 5usize)]);
    let order = ["long", "relevant"];
    let mut used = 0;
    let mut naive = Vec::new();
    for id in order {
        let cost = costs[id];
        if used + cost > 10 {
            break;
        }
        used += cost;
        naive.push(id);
    }
    assert!(
        naive.is_empty(),
        "naive stop-on-oversize starves all later evidence"
    );
    let mut used = 0;
    let mut skip = Vec::new();
    for id in order {
        let cost = costs[id];
        if used + cost > 10 {
            continue;
        }
        used += cost;
        skip.push(id);
    }
    assert_eq!(skip, vec!["relevant"]);
}

#[test]
fn star_hub_contamination_is_exact_by_construction() {
    let order = vec![
        "hub".into(),
        "leaf1".into(),
        "leaf2".into(),
        "relevant".into(),
    ];
    let relevant = BTreeSet::from(["relevant".to_string()]);
    let hubs = BTreeSet::from(["hub".to_string()]);
    let m = compute_metrics(&order, &relevant, &hubs, 3);
    assert_eq!(m.recall, 0.0);
    assert!((m.contamination - 1.0 / 3.0).abs() < 1e-12);
    assert_eq!(m.mrr, 0.25);
}

#[test]
fn additive_graph_context_cannot_displace_direct_prefix() {
    let direct = vec![
        hit("d1", 3.0, "lexical"),
        hit("d2", 2.0, "lexical"),
        hit("d3", 1.0, "lexical"),
    ];
    let graph = vec![hit("hub", 1000.0, "csar"), hit("d1", 999.0, "csar")];
    let composed = compose_capsule(&[direct], &graph, 5, DEFAULT_RRF_K);
    let ids = composed
        .iter()
        .map(|h| h.symbol_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(&ids[..3], &["d1", "d2", "d3"]);
    assert_eq!(ids[3], "hub");
    eprintln!(
        "{}",
        json!({"tier":"PROVEN","invariant":"compose_capsule keeps direct fused ranking as an immutable prefix","order":ids})
    );
}

#[test]
fn relevant_just_outside_candidate_k_is_unrecoverable_without_candidate_growth() {
    let candidate_k = 3;
    let full = vec!["a".into(), "b".into(), "c".into(), "relevant".into()];
    let relevant = BTreeSet::from(["relevant".to_string()]);
    let hubs = BTreeSet::new();
    let candidate = &full[..candidate_k];
    assert_eq!(
        compute_metrics(candidate, &relevant, &hubs, candidate_k).recall,
        0.0
    );
    assert_eq!(
        compute_metrics(&full, &relevant, &hubs, full.len()).recall,
        1.0
    );
    assert!(position(candidate, "relevant").is_none());
}
