//! CSIA — Code Spreading-Impact Analysis over directed reverse CSR graphs.
//!
//! Repurposes the proven Andersen-Chung-Lang forward-push solver
//! ([`crate::approximate_ppr_push`], work bound $\le 1/(\alpha \cdot \varepsilon)$)
//! over the directed reverse dependency graph (`ImpactGraph::reverse_structural`
//! and `ImpactGraph::reverse_complexity`) to quantify blast radius, contract
//! breakages, loop-amplified complexity regressions, affected entrypoints, and
//! the minimal prioritized test suite in sub-millisecond time.

use std::collections::{HashMap, HashSet, VecDeque};

use cognis_core::{
    ChangeImpactReport, ComplexityAmplificationPath, ComplexityClass, ContractBreakageSite,
    ImpactDimensions, ImpactGraph, Result, Symbol, SymbolKind,
};

use crate::push::approximate_ppr_push;

/// Default restart probability for behavioral/structural reverse impact diffusion.
pub const CSIA_BEHAVIORAL_ALPHA: f64 = 0.15;
/// Default restart probability for long-range complexity amplification diffusion.
pub const CSIA_COMPLEXITY_ALPHA: f64 = 0.10;
/// Default forward-push residual threshold ($\le 1/(\alpha\varepsilon)$ work bound).
pub const CSIA_DEFAULT_EPS: f64 = 1e-5;

/// Input descriptor for a modified or inspected symbol in CSIA.
#[derive(Debug, Clone)]
pub struct SymbolImpactSeed {
    pub symbol_id: String,
    /// Severity of signature/contract change in `[0.0, 1.0]` (`0.0` = body-only).
    pub contract_severity: f64,
    pub contract_reason: Option<String>,
    pub old_complexity: ComplexityClass,
    pub new_complexity: ComplexityClass,
}

impl SymbolImpactSeed {
    pub fn body_edit(
        symbol_id: impl Into<String>,
        old_complexity: ComplexityClass,
        new_complexity: ComplexityClass,
    ) -> Self {
        Self {
            symbol_id: symbol_id.into(),
            contract_severity: 0.0,
            contract_reason: None,
            old_complexity,
            new_complexity,
        }
    }

    pub fn signature_edit(
        symbol_id: impl Into<String>,
        severity: f64,
        reason: impl Into<String>,
        old_complexity: ComplexityClass,
        new_complexity: ComplexityClass,
    ) -> Self {
        Self {
            symbol_id: symbol_id.into(),
            contract_severity: severity.clamp(0.0, 1.0),
            contract_reason: Some(reason.into()),
            old_complexity,
            new_complexity,
        }
    }
}

/// Heuristic test-symbol detector across supported languages.
pub fn is_test_symbol(sym: &Symbol) -> bool {
    let name_lower = sym.name.to_ascii_lowercase();
    let path_lower = sym.file_path.to_ascii_lowercase();
    name_lower.starts_with("test_")
        || name_lower.ends_with("_test")
        || path_lower.contains("/test/")
        || path_lower.contains("/tests/")
        || path_lower.starts_with("test/")
        || path_lower.starts_with("tests/")
        || path_lower.ends_with("_test.py")
        || path_lower.ends_with("_test.go")
        || path_lower.ends_with("_test.rs")
        || path_lower.ends_with(".test.ts")
        || path_lower.ends_with(".spec.ts")
        || path_lower.ends_with(".test.js")
        || path_lower.ends_with(".spec.js")
        || path_lower.ends_with("test.java")
        || path_lower.ends_with("tests.cs")
}

/// Heuristic entrypoint detector (HTTP routes, CLI handlers, `main`).
pub fn is_entrypoint_symbol(sym: &Symbol) -> bool {
    sym.kind == SymbolKind::Route
        || sym.name == "main"
        || sym.name.starts_with("handle_")
        || sym.name.ends_with("Handler")
        || sym.name.ends_with("Controller")
        || sym.file_path.contains("routes")
        || sym.file_path.contains("controllers")
        || sym.file_path.contains("api/")
}

/// Run multi-channel CSIA impact analysis for `seeds` over `impact_graph`.
pub fn evaluate_change_impact(
    impact_graph: &ImpactGraph,
    symbols_by_id: &HashMap<String, Symbol>,
    seeds: &[SymbolImpactSeed],
    edge_loop_depths: &HashMap<(String, String), u8>,
) -> Result<ChangeImpactReport> {
    if seeds.is_empty() || impact_graph.reverse_structural.is_empty() {
        return Ok(ChangeImpactReport {
            impact_level: "low".to_string(),
            composite_risk_score: 0.0,
            dimensions: ImpactDimensions {
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
    }

    let rev_g = &impact_graph.reverse_structural;
    let seed_id_set: HashSet<&str> = seeds.iter().map(|s| s.symbol_id.as_str()).collect();

    // 1. Build normalized seed vector for reverse structural diffusion.
    let mut struct_seed_vec: Vec<(i32, f64)> = Vec::new();
    let mut cplx_seed_vec: Vec<(i32, f64)> = Vec::new();
    for s in seeds {
        if let Some(&idx) = rev_g.index.get(&s.symbol_id) {
            let base_w = 1.0 + s.contract_severity;
            struct_seed_vec.push((idx as i32, base_w));

            let delta_deg = (s.new_complexity.effective_degree()
                - s.old_complexity.effective_degree())
            .max(0.0);
            let cplx_w = if delta_deg > 0.0 {
                delta_deg
            } else {
                s.new_complexity.effective_degree().max(0.25)
            };
            cplx_seed_vec.push((idx as i32, cplx_w));
        }
    }

    let total_struct_seed: f64 = struct_seed_vec.iter().map(|(_, w)| *w).sum();
    if total_struct_seed > 0.0 {
        for item in &mut struct_seed_vec {
            item.1 /= total_struct_seed;
        }
    }
    let total_cplx_seed: f64 = cplx_seed_vec.iter().map(|(_, w)| *w).sum();
    if total_cplx_seed > 0.0 {
        for item in &mut cplx_seed_vec {
            item.1 /= total_cplx_seed;
        }
    }

    // 2. Execute O(1 / (alpha * eps)) Reverse Forward-Push on G^rev.
    let struct_push = if !struct_seed_vec.is_empty() {
        approximate_ppr_push(
            rev_g,
            &struct_seed_vec,
            CSIA_BEHAVIORAL_ALPHA,
            CSIA_DEFAULT_EPS,
        )?
    } else {
        crate::push::PushResult {
            estimate: HashMap::new(),
            residual: HashMap::new(),
            work: 0.0,
            pushes: 0,
        }
    };

    let cplx_push = if !cplx_seed_vec.is_empty() {
        approximate_ppr_push(
            &impact_graph.reverse_complexity,
            &cplx_seed_vec,
            CSIA_COMPLEXITY_ALPHA,
            CSIA_DEFAULT_EPS,
        )?
    } else {
        crate::push::PushResult {
            estimate: HashMap::new(),
            residual: HashMap::new(),
            work: 0.0,
            pushes: 0,
        }
    };

    // 3. Evaluate reached upstream dependents, entrypoints, and test sinks.
    let mut reached_symbols: Vec<(&Symbol, f64)> = Vec::new();
    let mut reached_modules: HashSet<&str> = HashSet::new();
    let mut entrypoints: Vec<(String, f64)> = Vec::new();
    let mut tests: Vec<(String, f64)> = Vec::new();
    let mut prod_mass = 0.0f64;
    let mut test_mass = 0.0f64;
    let mut weighted_criticality = 0.0f64;

    for (&node_idx, &mass) in &struct_push.estimate {
        if mass < 1e-4 {
            continue;
        }
        let Some(sym_id) = rev_g.node_ids.get(node_idx as usize) else {
            continue;
        };
        if seed_id_set.contains(sym_id.as_str()) {
            continue;
        }
        let Some(sym) = symbols_by_id.get(sym_id) else {
            continue;
        };
        reached_symbols.push((sym, mass));
        reached_modules.insert(sym.module.as_str());

        if is_test_symbol(sym) {
            test_mass += mass;
            tests.push((
                format!(
                    "{} ({}:{})",
                    sym.qualified_name, sym.file_path, sym.line_start
                ),
                mass,
            ));
        } else {
            prod_mass += mass;
            let mut crit = 1.0;
            if is_entrypoint_symbol(sym) {
                crit += 1.5;
                entrypoints.push((
                    format!(
                        "{} ({}:{})",
                        sym.qualified_name, sym.file_path, sym.line_start
                    ),
                    mass,
                ));
            }
            if !sym.untrusted_flags.is_empty() {
                crit += 0.5;
            }
            weighted_criticality += mass * crit;
        }
    }

    entrypoints.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    tests.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let reached_symbol_count = reached_symbols.len();
    let reached_module_count = reached_modules.len();

    let structural_blast_radius = if reached_symbol_count == 0 {
        0.0
    } else {
        (1.0 - (-1.4 * weighted_criticality
            - 0.18 * ((1 + reached_symbol_count) as f64).ln()
            - 0.15 * ((1 + reached_module_count) as f64).ln())
        .exp())
        .clamp(0.0, 1.0)
    };

    // 4. 1-hop Contract Breakage analysis on reverse edges.
    let mut breaking_call_sites = Vec::new();
    let mut survival_prob = 1.0f64;

    for s in seeds {
        if s.contract_severity <= 0.0 {
            continue;
        }
        let Some(&u_idx) = rev_g.index.get(&s.symbol_id) else {
            continue;
        };
        let target_name = symbols_by_id
            .get(&s.symbol_id)
            .map(|sym| sym.qualified_name.clone())
            .unwrap_or_else(|| s.symbol_id.clone());
        let (nbrs, weights) = rev_g.neighbors(u_idx);
        for (&v_idx, &w) in nbrs.iter().zip(weights.iter()) {
            if v_idx as usize == u_idx {
                continue;
            }
            let Some(caller_id) = rev_g.node_ids.get(v_idx as usize) else {
                continue;
            };
            // If the caller is also being updated in the same patch, discount breakage.
            if seed_id_set.contains(caller_id.as_str()) {
                continue;
            }
            if let Some(caller_sym) = symbols_by_id.get(caller_id) {
                let conf = w.clamp(0.1, 1.0);
                let site_sev = (s.contract_severity * conf).clamp(0.0, 1.0);
                survival_prob *= 1.0 - site_sev;
                breaking_call_sites.push(ContractBreakageSite {
                    caller_id: caller_sym.id.clone(),
                    caller_name: caller_sym.qualified_name.clone(),
                    caller_file: caller_sym.file_path.clone(),
                    caller_line: caller_sym.line_start,
                    target_symbol: target_name.clone(),
                    reason: s.contract_reason.clone().unwrap_or_else(|| {
                        "Signature or contract modified without updating caller".to_string()
                    }),
                    severity: site_sev,
                });
            }
        }
    }
    let contract_breakage_risk = (1.0 - survival_prob).clamp(0.0, 1.0);

    // 5. Tropical Loop-Weighted Complexity Amplification Paths (BFS up to 4 hops on G^rev).
    let mut complexity_amplifications = Vec::new();
    let mut max_caf = 0.0f64;
    let mut max_degree_jump = 0.0f64;

    for s in seeds {
        let delta_deg =
            (s.new_complexity.effective_degree() - s.old_complexity.effective_degree()).max(0.0);
        let seed_select_deg = s.new_complexity.effective_degree();
        if delta_deg <= 0.0 && seed_select_deg < 1.0 {
            continue;
        }
        let Some(&start_idx) = rev_g.index.get(&s.symbol_id) else {
            continue;
        };
        let seed_name = symbols_by_id
            .get(&s.symbol_id)
            .map(|sym| sym.qualified_name.clone())
            .unwrap_or_else(|| s.symbol_id.clone());

        // Queue entries: (current_node_idx, hop, cumulative_loop_depth, call_chain)
        let mut queue: VecDeque<(usize, u8, u8, Vec<String>)> = VecDeque::new();
        let mut visited: HashSet<usize> = HashSet::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0, 0, vec![seed_name]));

        while let Some((curr_idx, hop, cum_loops, chain)) = queue.pop_front() {
            if hop >= 4 {
                continue;
            }
            let curr_id = &rev_g.node_ids[curr_idx];
            let (nbrs, _) = rev_g.neighbors(curr_idx);
            for &caller_i32 in nbrs {
                let caller_idx = caller_i32 as usize;
                if !visited.insert(caller_idx) {
                    continue;
                }
                let Some(caller_id) = rev_g.node_ids.get(caller_idx) else {
                    continue;
                };
                let Some(caller_sym) = symbols_by_id.get(caller_id) else {
                    continue;
                };
                let edge_loops = edge_loop_depths
                    .get(&(caller_id.clone(), curr_id.clone()))
                    .copied()
                    .unwrap_or(0);
                let new_cum_loops = cum_loops.saturating_add(edge_loops);
                let mut next_chain = chain.clone();
                next_chain.push(caller_sym.qualified_name.clone());

                if new_cum_loops > 0 || delta_deg > 0.0 {
                    let loop_mult = ComplexityClass::polynomial(new_cum_loops);
                    let before_cplx = loop_mult.nest_mul(s.old_complexity);
                    let after_cplx = loop_mult.nest_mul(s.new_complexity);
                    let eff_jump =
                        (after_cplx.effective_degree() - before_cplx.effective_degree()).max(0.0);
                    let caf = 4.0f64.powi(i32::from(new_cum_loops)) * delta_deg.max(0.5);

                    if new_cum_loops > 0 && after_cplx.effective_degree() >= 2.0 || eff_jump >= 1.0
                    {
                        max_caf = max_caf.max(caf);
                        max_degree_jump = max_degree_jump.max(eff_jump);
                        complexity_amplifications.push(ComplexityAmplificationPath {
                            caller_id: caller_sym.id.clone(),
                            caller_name: caller_sym.qualified_name.clone(),
                            caller_file: caller_sym.file_path.clone(),
                            caller_line: caller_sym.line_start,
                            cumulative_loop_depth: new_cum_loops,
                            amplification_factor: caf,
                            time_before: before_cplx.notation(),
                            time_after: after_cplx.notation(),
                            call_chain: next_chain.clone(),
                        });
                    }
                }

                queue.push_back((caller_idx, hop + 1, new_cum_loops, next_chain));
            }
        }
    }

    complexity_amplifications.sort_by(|a, b| {
        b.amplification_factor
            .partial_cmp(&a.amplification_factor)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    complexity_amplifications.truncate(10);

    let cplx_mass_sum: f64 = cplx_push
        .estimate
        .iter()
        .filter(|(&idx, _)| {
            rev_g
                .node_ids
                .get(idx as usize)
                .is_some_and(|id| !seed_id_set.contains(id.as_str()))
        })
        .map(|(_, &m)| m)
        .sum();

    let complexity_amplification = (0.45 * max_degree_jump + 0.04 * max_caf + 0.30 * cplx_mass_sum)
        .tanh()
        .clamp(0.0, 1.0);

    // 6. Test coverage gap risk.
    let mut untested_high_risk_paths = Vec::new();
    let test_coverage_gap = if prod_mass <= 1e-4 && seeds.iter().all(|s| s.contract_severity == 0.0)
    {
        0.0
    } else {
        let gap = prod_mass / (prod_mass + 3.0 * test_mass + 1e-6);
        if tests.is_empty() && (!entrypoints.is_empty() || reached_symbol_count > 0) {
            for (ep, _) in entrypoints.iter().take(3) {
                untested_high_risk_paths.push(format!(
                    "Impacted entrypoint `{ep}` has 0 covering tests on the reverse call path"
                ));
            }
            if untested_high_risk_paths.is_empty() {
                untested_high_risk_paths.push(format!(
                    "{reached_symbol_count} upstream dependent symbol(s) impacted with 0 reached test symbols"
                ));
            }
        }
        gap.clamp(0.0, 1.0)
    };

    let composite_risk_score = (0.30 * structural_blast_radius
        + 0.35 * complexity_amplification
        + 0.25 * contract_breakage_risk
        + 0.10 * test_coverage_gap)
        .clamp(0.0, 1.0);

    let impact_level = if composite_risk_score >= 0.70
        || contract_breakage_risk >= 0.80
        || complexity_amplification >= 0.75
    {
        "critical"
    } else if composite_risk_score >= 0.45
        || contract_breakage_risk >= 0.45
        || complexity_amplification >= 0.50
    {
        "high"
    } else if composite_risk_score >= 0.20 || reached_symbol_count > 0 {
        "medium"
    } else {
        "low"
    }
    .to_string();

    Ok(ChangeImpactReport {
        impact_level,
        composite_risk_score,
        dimensions: ImpactDimensions {
            structural_blast_radius,
            complexity_amplification,
            contract_breakage_risk,
            test_coverage_gap,
        },
        breaking_call_sites,
        complexity_amplifications,
        affected_entrypoints: entrypoints.into_iter().take(10).map(|(s, _)| s).collect(),
        minimal_test_suite: tests.into_iter().take(10).map(|(s, _)| s).collect(),
        untested_high_risk_paths,
        reached_symbol_count,
        reached_module_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognis_core::{Edge, EdgeKind};

    fn mk_sym(id: &str, name: &str, kind: SymbolKind, file: &str) -> Symbol {
        Symbol {
            id: id.into(),
            kind,
            name: name.into(),
            qualified_name: name.into(),
            language: "rust".into(),
            module: file.into(),
            file_path: file.into(),
            line_start: 10,
            line_end: 25,
            signature: Some(format!("fn {name}()")),
            docstring: None,
            content_hash: "h".into(),
            body_excerpt: None,
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: vec![],
            updated_at: 0,
        }
    }

    #[test]
    fn csia_detects_loop_amplified_cubic_blowup_and_affected_route() {
        // Call chain: api_route (Route) -> batch_processor (2x loop) -> leaf_lookup (regresses O(1) -> O(n))
        let s_route = mk_sym("id:route", "api_route", SymbolKind::Route, "src/api.rs");
        let s_batch = mk_sym(
            "id:batch",
            "batch_processor",
            SymbolKind::Function,
            "src/batch.rs",
        );
        let s_leaf = mk_sym("id:leaf", "leaf_lookup", SymbolKind::Function, "src/db.rs");

        let node_ids = vec![s_route.id.clone(), s_batch.id.clone(), s_leaf.id.clone()];
        let edges = vec![
            Edge {
                src_id: s_route.id.clone(),
                dst_id: s_batch.id.clone(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                meta: serde_json::Value::Null,
            },
            Edge {
                src_id: s_batch.id.clone(),
                dst_id: s_leaf.id.clone(),
                kind: EdgeKind::Calls,
                confidence: 1.0,
                meta: serde_json::Value::Null,
            },
        ];
        let mut loop_depths = HashMap::new();
        loop_depths.insert((s_batch.id.clone(), s_leaf.id.clone()), 2u8);

        let ig = ImpactGraph::build(node_ids, &edges, &loop_depths);
        let symbols_by_id: HashMap<String, Symbol> = [s_route, s_batch, s_leaf]
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();

        let seeds = vec![SymbolImpactSeed::signature_edit(
            "id:leaf",
            0.8,
            "Added required parameter `filter`",
            ComplexityClass::constant(),
            ComplexityClass::linear(),
        )];

        let report = evaluate_change_impact(&ig, &symbols_by_id, &seeds, &loop_depths).unwrap();
        assert!(
            matches!(report.impact_level.as_str(), "high" | "critical"),
            "expected high/critical impact, got {}",
            report.impact_level
        );
        assert_eq!(report.breaking_call_sites.len(), 1);
        assert_eq!(report.breaking_call_sites[0].caller_name, "batch_processor");
        assert!(!report.complexity_amplifications.is_empty());
        assert_eq!(report.complexity_amplifications[0].time_before, "O(n^2)");
        assert_eq!(report.complexity_amplifications[0].time_after, "O(n^3)");
        assert!(!report.affected_entrypoints.is_empty());
        assert!(!report.untested_high_risk_paths.is_empty());
    }
}
