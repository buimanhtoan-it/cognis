//! In-memory Shadow AST & Overlay CSR Graph analyzer (`ShadowWorkspace`).
//!
//! Enables `<10–15ms` pre-flight and mid-flight evaluation of unsaved buffers,
//! code snippets, or unified diffs against the repository's existing knowledge
//! graph without mutating `.cognis/uckg.db`.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use cognis_core::{
    ComplexityClass, Edge, GuardrailCapsule, ImpactGraph, OptimizationAdvisory, Symbol,
    SymbolComplexityReport,
};
use cognis_csar::{evaluate_change_impact, SymbolImpactSeed};

use crate::complexity::analyze_workspace_complexity;
use crate::parser::parse_source;
use crate::resolver::{resolve_edges, to_edges};

/// One in-flight file buffer or proposed patch submitted by an AI agent or IDE.
#[derive(Debug, Clone)]
pub struct ShadowFileEdit {
    pub file_path: String,
    pub new_content: String,
}

/// Evaluate a set of in-memory [`ShadowFileEdit`]s (or target symbol IDs/names
/// when `file_edits` is empty) against `base_symbols` and `base_edges`,
/// returning a token-budgeted [`GuardrailCapsule`].
pub fn analyze_shadow_patch(
    base_symbols: &[Symbol],
    base_edges: &[Edge],
    file_edits: &[ShadowFileEdit],
    target_queries: &[String],
    generation_label: &str,
) -> GuardrailCapsule {
    let start = Instant::now();

    // 1. Compute baseline complexity on existing symbols so we can measure Δ Big-O.
    let (base_reports, _) = analyze_workspace_complexity(base_symbols, base_edges, &HashMap::new());

    // 2. If in-memory `file_edits` are provided, build an overlay symbol & edge set.
    let edited_paths: HashSet<&str> = file_edits.iter().map(|e| e.file_path.as_str()).collect();
    let mut old_by_file_and_qname: HashMap<(String, String), &Symbol> = HashMap::new();
    let mut old_by_file_and_name: HashMap<(String, String), &Symbol> = HashMap::new();
    for sym in base_symbols {
        if edited_paths.contains(sym.file_path.as_str()) {
            old_by_file_and_qname.insert((sym.file_path.clone(), sym.qualified_name.clone()), sym);
            old_by_file_and_name.insert((sym.file_path.clone(), sym.name.clone()), sym);
        }
    }

    let mut overlay_symbols: Vec<Symbol> = base_symbols
        .iter()
        .filter(|s| !edited_paths.contains(s.file_path.as_str()))
        .cloned()
        .collect();

    let mut source_overrides: HashMap<String, String> = HashMap::new();
    let mut touched_symbol_ids: Vec<String> = Vec::new();
    let mut impact_seeds: Vec<SymbolImpactSeed> = Vec::new();

    for edit in file_edits {
        let parsed = parse_source(&edit.file_path, &edit.new_content);
        let lines: Vec<&str> = edit.new_content.lines().collect();

        for mut new_sym in parsed.symbols {
            let parsed_id = new_sym.id.clone();
            // Extract exact full function source from `new_content` using 1-based line numbers
            // so even functions > 1500 bytes are analyzed without truncation!
            let start_idx = (new_sym.line_start.saturating_sub(1) as usize).min(lines.len());
            let end_idx = (new_sym.line_end as usize).min(lines.len());
            if start_idx < end_idx {
                let full_fn_src = lines[start_idx..end_idx].join("\n");
                source_overrides.insert(parsed_id.clone(), full_fn_src);
            }

            // Preserve the old symbol ID if `(file_path, qualified_name)` or `(file_path, name)`
            // matches an existing symbol in the base snapshot so inbound edges `(caller -> old_id)` connect seamlessly!
            if let Some(old_sym) = old_by_file_and_qname
                .get(&(edit.file_path.clone(), new_sym.qualified_name.clone()))
                .or_else(|| {
                    old_by_file_and_name.get(&(edit.file_path.clone(), new_sym.name.clone()))
                })
            {
                new_sym.id = old_sym.id.clone();
                if let Some(full_src) = source_overrides.remove(&parsed_id) {
                    source_overrides.insert(new_sym.id.clone(), full_src);
                }
            }

            touched_symbol_ids.push(new_sym.id.clone());
            overlay_symbols.push(new_sym);
        }
    }

    // 3. Re-resolve edges if we have in-memory file edits; otherwise reuse `base_edges`.
    let overlay_edges: Vec<Edge> = if file_edits.is_empty() {
        base_edges.to_vec()
    } else {
        let mut kept_edges: Vec<Edge> = base_edges
            .iter()
            .filter(|e| {
                base_symbols
                    .iter()
                    .find(|s| s.id == e.src_id)
                    .is_none_or(|s| !edited_paths.contains(s.file_path.as_str()))
            })
            .cloned()
            .collect();
        let newly_resolved = to_edges(&resolve_edges(&overlay_symbols));
        for edge in newly_resolved {
            if overlay_symbols
                .iter()
                .find(|s| s.id == edge.src_id)
                .is_some_and(|s| edited_paths.contains(s.file_path.as_str()))
            {
                kept_edges.push(edge);
            }
        }
        kept_edges
    };

    // 4. Run inter-procedural complexity + optimization advisor on the overlay workspace.
    let (overlay_reports, edge_loop_depths) =
        analyze_workspace_complexity(&overlay_symbols, &overlay_edges, &source_overrides);

    // 5. Determine focus symbols and construct CSIA impact seeds.
    if touched_symbol_ids.is_empty() {
        for q in target_queries {
            let q_trim = q.trim();
            for sym in &overlay_symbols {
                if sym.id == q_trim
                    || sym.qualified_name == q_trim
                    || sym.name == q_trim
                    || sym.file_path == q_trim
                {
                    touched_symbol_ids.push(sym.id.clone());
                }
            }
        }
    }
    if touched_symbol_ids.is_empty() {
        // Default to top complexity hotspots if no specific target was passed.
        let mut sorted: Vec<&SymbolComplexityReport> = overlay_reports.values().collect();
        sorted.sort_by(|a, b| {
            b.risk_score
                .partial_cmp(&a.risk_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for r in sorted.into_iter().take(5) {
            touched_symbol_ids.push(r.symbol_id.clone());
        }
    }

    let overlay_sym_map: HashMap<String, Symbol> = overlay_symbols
        .iter()
        .cloned()
        .map(|s| (s.id.clone(), s))
        .collect();

    let mut complexity_regressions_count = 0usize;
    for sym_id in &touched_symbol_ids {
        let Some(new_rep) = overlay_reports.get(sym_id) else {
            continue;
        };
        let new_cplx = parse_notation_to_class(&new_rep.propagated_time);
        let old_cplx = base_reports
            .get(sym_id)
            .map(|r| parse_notation_to_class(&r.propagated_time))
            .unwrap_or(new_cplx);

        if new_cplx.effective_degree() > old_cplx.effective_degree() {
            complexity_regressions_count += 1;
        }

        // Check signature change if edited in memory.
        let mut contract_sev = 0.0;
        let mut contract_reason = None;
        if let Some(new_sym) = overlay_sym_map.get(sym_id) {
            if let Some(old_sym) = old_by_file_and_qname
                .get(&(new_sym.file_path.clone(), new_sym.qualified_name.clone()))
                .or_else(|| {
                    old_by_file_and_name.get(&(new_sym.file_path.clone(), new_sym.name.clone()))
                })
            {
                let old_sig = old_sym.signature.as_deref().unwrap_or("");
                if !old_sig.contains("(...") && old_sym.signature != new_sym.signature {
                    contract_sev = 0.85;
                    contract_reason = Some(format!(
                        "Signature changed from `{}` to `{}`",
                        old_sig,
                        new_sym.signature.as_deref().unwrap_or("")
                    ));
                }
            }
        }

        impact_seeds.push(SymbolImpactSeed {
            symbol_id: sym_id.clone(),
            contract_severity: contract_sev,
            contract_reason,
            old_complexity: old_cplx,
            new_complexity: new_cplx,
        });
    }

    // Also check for deleted symbols in edited files!
    for ((file_path, qname), old_sym) in &old_by_file_and_qname {
        let still_exists = overlay_symbols.iter().any(|s| {
            s.id == old_sym.id
                || (&s.file_path == file_path
                    && (&s.qualified_name == qname || s.name == old_sym.name))
        });
        if !still_exists {
            impact_seeds.push(SymbolImpactSeed::signature_edit(
                old_sym.id.clone(),
                1.0,
                format!("Symbol `{qname}` was deleted or renamed in `{file_path}`"),
                ComplexityClass::Constant,
                ComplexityClass::Constant,
            ));
        }
    }

    let node_ids: Vec<String> = overlay_symbols.iter().map(|s| s.id.clone()).collect();
    let impact_graph = ImpactGraph::build(node_ids, &overlay_edges, &edge_loop_depths);
    let impact_report = evaluate_change_impact(
        &impact_graph,
        &overlay_sym_map,
        &impact_seeds,
        &edge_loop_depths,
    )
    .unwrap_or_else(|_| cognis_core::ChangeImpactReport {
        impact_level: "low".to_string(),
        composite_risk_score: 0.0,
        dimensions: cognis_core::ImpactDimensions {
            structural_blast_radius: 0.0,
            complexity_amplification: 0.0,
            contract_breakage_risk: 0.0,
            test_coverage_gap: 0.0,
        },
        breaking_call_sites: Vec::new(),
        complexity_amplifications: Vec::new(),
        affected_entrypoints: Vec::new(),
        minimal_test_suite: Vec::new(),
        untested_high_risk_paths: Vec::new(),
        reached_symbol_count: 0,
        reached_module_count: 0,
    });

    // Collect focused complexity reports and advisories.
    let mut focused_reports: Vec<SymbolComplexityReport> = Vec::new();
    let mut advisories: Vec<OptimizationAdvisory> = Vec::new();
    let mut seen_sym = HashSet::new();
    for id in &touched_symbol_ids {
        if seen_sym.insert(id.clone()) {
            if let Some(rep) = overlay_reports.get(id) {
                advisories.extend(rep.advisories.clone());
                focused_reports.push(rep.clone());
            }
        }
    }
    focused_reports.sort_by(|a, b| {
        b.risk_score
            .partial_cmp(&a.risk_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let verdict = if !impact_report.breaking_call_sites.is_empty() {
        "WARN_CONTRACT_BREAKAGE"
    } else if complexity_regressions_count > 0
        || !impact_report.complexity_amplifications.is_empty()
    {
        "WARN_COMPLEXITY_REGRESSION"
    } else if advisories
        .iter()
        .any(|a| a.severity == "critical" || a.severity == "high")
    {
        "WARN_OPTIMIZATION_NEEDED"
    } else {
        "OK_OPTIMAL"
    }
    .to_string();

    let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
    let top_cplx = focused_reports
        .first()
        .map(|r| format!("`{}` ({})", r.qualified_name, r.propagated_time))
        .unwrap_or_else(|| "none".to_string());

    let summary_markdown = format!(
        "**Verdict: `{verdict}`** | Top Complexity: {top_cplx} | Impact: **{}** (score {:.2}, {} upstream dependents, {} advisories) | Latency: {:.2}ms",
        impact_report.impact_level.to_ascii_uppercase(),
        impact_report.composite_risk_score,
        impact_report.reached_symbol_count,
        advisories.len(),
        latency_ms
    );

    GuardrailCapsule {
        verdict,
        latency_ms,
        generation: generation_label.to_string(),
        summary_markdown,
        complexity_reports: focused_reports,
        impact_report,
        advisories,
    }
}

pub fn parse_notation_to_class(notation: &str) -> ComplexityClass {
    match notation {
        "O(1)" => ComplexityClass::Constant,
        "O(log n)" => ComplexityClass::log_n(),
        "O(sqrt(n))" => ComplexityClass::sqrt_n(),
        "O(n)" => ComplexityClass::linear(),
        "O(n log n)" => ComplexityClass::n_log_n(),
        "O(n^2)" => ComplexityClass::quadratic(),
        "O(n^3)" => ComplexityClass::cubic(),
        "O(n!)" => ComplexityClass::Factorial,
        s if s.contains("^n") => ComplexityClass::Exponential { base_calls: 2 },
        s if s.starts_with("O(n^") => ComplexityClass::Polynomial {
            degree: 4,
            log_power: 0,
        },
        _ => ComplexityClass::Constant,
    }
}
