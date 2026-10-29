#![cfg(feature = "reference-scan")]

use std::collections::{BTreeMap, HashSet};

use cognis_core::{Edge, EdgeKind, Hit, Result, Symbol, SymbolKind};
use cognis_mcp::reference_scan::ReferenceScan;
use cognis_store::{Direction, GenerationSnapshot, IndexPublisher, PublishBatch, PublishReason};
use cognis_testgen::{arb_source_tree, FileBody, SourceTree};
use proptest::prelude::*;

struct Fixture {
    symbols: Vec<Symbol>,
    edges: Vec<Edge>,
}

struct LookupQuery {
    needle: String,
    kind: Option<String>,
}

fn kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Method => "method",
        SymbolKind::Interface => "interface",
        SymbolKind::Route => "route",
        SymbolKind::Module => "module",
        SymbolKind::Var => "var",
        SymbolKind::Const => "const",
    }
}

fn fixture_from_tree(tree: &SourceTree, missing_seed: u8) -> Fixture {
    let mut nodes = Vec::new();
    for (file_index, file) in tree.files.iter().enumerate() {
        let FileBody::Code(functions) = &file.body else {
            continue;
        };
        for (function_index, function) in functions.iter().enumerate() {
            let id = format!("sym-{file_index:02}-{function_index:02}-{}", function.name);
            let kind = if (file_index + function_index) % 2 == 0 {
                SymbolKind::Function
            } else {
                SymbolKind::Method
            };
            nodes.push((
                Symbol {
                    id,
                    kind,
                    name: function.name.clone(),
                    qualified_name: format!("{}::{}", file.rel_path, function.name),
                    language: file.language.to_string(),
                    module: file.rel_path.clone(),
                    file_path: file.rel_path.clone(),
                    line_start: (function_index as u32 * 2) + 1,
                    line_end: (function_index as u32 * 2) + 2,
                    signature: None,
                    docstring: None,
                    content_hash: file.content_hash(),
                    body_excerpt: None,
                    semantic_summary: None,
                    risk_score: 0.0,
                    ambiguous: false,
                    untrusted_flags: Vec::new(),
                    updated_at: 1,
                },
                function.calls.clone(),
            ));
        }
    }
    if nodes.is_empty() {
        nodes.push((
            Symbol {
                id: "sym-fallback".to_string(),
                kind: SymbolKind::Function,
                name: "fallback".to_string(),
                qualified_name: "generated::fallback".to_string(),
                language: "rust".to_string(),
                module: "generated".to_string(),
                file_path: "generated.rs".to_string(),
                line_start: 1,
                line_end: 1,
                signature: None,
                docstring: None,
                content_hash: "fallback".to_string(),
                body_excerpt: None,
                semantic_summary: None,
                risk_score: 0.0,
                ambiguous: false,
                untrusted_flags: Vec::new(),
                updated_at: 1,
            },
            Vec::new(),
        ));
    }

    let mut ids_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (symbol, _) in &nodes {
        ids_by_name
            .entry(symbol.name.clone())
            .or_default()
            .push(symbol.id.clone());
    }
    for ids in ids_by_name.values_mut() {
        ids.sort();
    }

    let mut edges = Vec::new();
    for (source_index, (symbol, calls)) in nodes.iter().enumerate() {
        for (call_index, call) in calls.iter().enumerate() {
            let Some(targets) = ids_by_name.get(call) else {
                continue;
            };
            let target = &targets[(source_index + call_index) % targets.len()];
            edges.push(Edge {
                src_id: symbol.id.clone(),
                dst_id: target.clone(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                meta: if (source_index + call_index + missing_seed as usize) % 7 == 0 {
                    serde_json::json!({ "dst_missing": true })
                } else {
                    serde_json::Value::Null
                },
            });
        }
    }
    edges.sort_by(|left, right| {
        (&left.src_id, &left.dst_id, edge_kind_name(left.kind)).cmp(&(
            &right.src_id,
            &right.dst_id,
            edge_kind_name(right.kind),
        ))
    });
    edges.dedup_by(|left, right| {
        left.src_id == right.src_id && left.dst_id == right.dst_id && left.kind == right.kind
    });

    Fixture {
        symbols: nodes.into_iter().map(|(symbol, _)| symbol).collect(),
        edges,
    }
}

fn edge_kind_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::Imports => "imports",
        EdgeKind::Inherits => "inherits",
        EdgeKind::Implements => "implements",
        EdgeKind::Reads => "reads",
        EdgeKind::Writes => "writes",
        EdgeKind::RoutesTo => "routes_to",
        EdgeKind::Tests => "tests",
    }
}

fn lookup_query(symbols: &[Symbol], selector: u8, kind_selector: u8) -> LookupQuery {
    let symbol = &symbols[selector as usize % symbols.len()];
    let needle = match selector % 7 {
        0 => symbol.id.clone(),
        1 => symbol.qualified_name.clone(),
        2 => symbol.name.clone(),
        3 => symbol.name.to_uppercase(),
        4 => symbol.name.chars().take(2).collect(),
        5 => format!("  {}  ", symbol.id),
        _ => "definitely-absent".to_string(),
    };
    let kind = match kind_selector % 4 {
        0 => None,
        1 => Some(kind_name(symbol.kind).to_string()),
        2 => Some("class".to_string()),
        _ => Some("interface".to_string()),
    };
    LookupQuery { needle, kind }
}

fn trace_bounded(
    snapshot: &GenerationSnapshot<'_>,
    symbol_id: &str,
    direction: Direction,
    depth: u8,
) -> Result<Vec<Hit>> {
    let mut visited = HashSet::new();
    visited.insert(symbol_id.to_string());
    let mut frontier = vec![symbol_id.to_string()];
    let mut hits = Vec::new();

    for hop in 1..=depth {
        let edges = snapshot.edges_incident(&frontier, direction)?;
        let mut next = Vec::new();
        for node in &frontier {
            for edge in &edges {
                if edge.dst_missing() {
                    continue;
                }
                let neighbor = match direction {
                    Direction::Out if edge.src_id == *node => Some(&edge.dst_id),
                    Direction::In if edge.dst_id == *node => Some(&edge.src_id),
                    Direction::Both if edge.src_id == *node => Some(&edge.dst_id),
                    Direction::Both if edge.dst_id == *node => Some(&edge.src_id),
                    _ => None,
                };
                if let Some(neighbor) = neighbor {
                    if visited.insert(neighbor.clone()) {
                        hits.push(
                            Hit::new(
                                neighbor.clone(),
                                1.0 / hop as f64,
                                "structural",
                                "dependency",
                            )
                            .with_evidence(serde_json::json!({ "depth": hop })),
                        );
                        next.push(neighbor.clone());
                    }
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(hits)
}

fn symbol_ids(symbols: Vec<Symbol>) -> Vec<String> {
    symbols.into_iter().map(|symbol| symbol.id).collect()
}

fn hit_ids(hits: Vec<Hit>) -> Vec<String> {
    hits.into_iter().map(|hit| hit.symbol_id).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // Feature: cognis-one-toggle-engine, Property 14: For any database and any
    // query, at the same committed source_generation, bounded-read queries
    // return the same result identifiers in the same order as reference scans.
    // **Validates: Requirements 9.8**
    #[test]
    fn bounded_queries_match_reference_scan_at_the_same_generation(
        tree in arb_source_tree(),
        lookup_selector in any::<u8>(),
        kind_selector in any::<u8>(),
        hydrate_selectors in prop::collection::vec(any::<u8>(), 0..12),
        trace_selector in any::<u8>(),
        direction_selector in 0u8..3,
        depth in 0u8..6,
        missing_seed in any::<u8>(),
    ) {
        let fixture = fixture_from_tree(&tree, missing_seed);
        let mut db = cognis_store::Database::new(":memory:");
        db.connect().expect("migrate generated database");

        let mut batch = PublishBatch::empty(PublishReason::Initial);
        batch.symbol_upserts = fixture.symbols.clone();
        batch.edge_upserts = fixture.edges;
        let receipt = db.publish(&batch).expect("publish generated database");
        let snapshot = GenerationSnapshot::open(&db).expect("pin committed generation");
        prop_assert_eq!(snapshot.source_generation(), receipt.source_generation);

        let reference = ReferenceScan::new(&db);
        let query = lookup_query(&fixture.symbols, lookup_selector, kind_selector);
        let kind = query.kind.as_deref();
        let optimized_lookup = snapshot
            .resolve_symbol(&query.needle, kind)
            .expect("bounded lookup")
            .map(|symbol| symbol.id);
        let reference_lookup = reference
            .lookup(&query.needle, kind)
            .expect("reference lookup")
            .map(|symbol| symbol.id);
        prop_assert_eq!(optimized_lookup, reference_lookup);

        let mut requested_ids: Vec<String> = hydrate_selectors
            .iter()
            .map(|selector| fixture.symbols[*selector as usize % fixture.symbols.len()].id.clone())
            .collect();
        requested_ids.push("absent-symbol".to_string());
        let optimized_hydrate = symbol_ids(
            snapshot
                .hydrate_ids(&requested_ids)
                .expect("bounded hydration"),
        );
        let reference_hydrate = symbol_ids(
            reference
                .hydrate(&requested_ids)
                .expect("reference hydration"),
        );
        prop_assert_eq!(optimized_hydrate, reference_hydrate);

        let start = &fixture.symbols[trace_selector as usize % fixture.symbols.len()].id;
        let direction = match direction_selector {
            0 => Direction::Out,
            1 => Direction::In,
            _ => Direction::Both,
        };
        let optimized_trace = hit_ids(
            trace_bounded(&snapshot, start, direction, depth).expect("bounded traversal"),
        );
        let reference_trace = hit_ids(
            reference
                .dependency_trace(start, direction.as_str(), depth)
                .expect("reference traversal"),
        );
        prop_assert_eq!(optimized_trace, reference_trace);

        prop_assert_eq!(snapshot.source_generation(), receipt.source_generation);
    }
}
