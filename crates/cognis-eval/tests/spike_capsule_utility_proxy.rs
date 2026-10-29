//! Token-proxy capsule utility spike. These metrics are NOT coding-task success
//! and citation availability is NOT exact citation validity.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use cognis_core::{Hit, Symbol};
use cognis_eval::bench::{load_golden_set, LexicalIndex, SymbolTable};
use cognis_retrieval::{compose_capsule, DEFAULT_RRF_K};
use cognis_store::Database;
use serde_json::json;

const TOKEN_BUDGET: usize = 800;
const REPOS: &[(&str, &str, &str)] = &[
    ("requests", "db/requests.db", "golden/requests_pr.json"),
    ("fastapi", "db/fastapi.db", "golden/fastapi_pr.json"),
    ("eshop", "db/eshop.db", "golden/eshop_pr.json"),
    ("petclinic", "db/petclinic.db", "golden/petclinic_pr.json"),
    ("jsoup", "db/jsoup.db", "golden/jsoup_pr.json"),
];

#[derive(Default, Clone)]
struct Utility {
    n: usize,
    coverage: f64,
    mrr: f64,
    evidence_per_token: f64,
    unique_files: f64,
    unique_modules: f64,
    direct_prefix_retention: f64,
    duplicate_rate: f64,
    citation_availability: f64,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn token_cost(symbol: &Symbol) -> usize {
    let chars = symbol.name.chars().count()
        + symbol.file_path.chars().count()
        + symbol.body_excerpt.as_deref().unwrap_or("").chars().count();
    chars.div_ceil(4) + 8
}
fn module(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit_once('/')
        .map_or("<root>".into(), |(m, _)| m.into())
}

fn budgeted(
    order: &[String],
    by_id: &HashMap<String, Symbol>,
    budget: usize,
) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut used = 0;
    for id in order {
        let Some(symbol) = by_id.get(id) else {
            continue;
        };
        let cost = token_cost(symbol);
        if used + cost <= budget {
            out.push(id.clone());
            used += cost;
        }
    }
    (out, used)
}

fn hierarchy_order(raw: &[String], by_id: &HashMap<String, Symbol>) -> Vec<String> {
    let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in raw {
        if let Some(s) = by_id.get(id) {
            buckets
                .entry(module(&s.file_path))
                .or_default()
                .push(id.clone());
        }
    }
    let mut order = Vec::new();
    let max = buckets.values().map(Vec::len).max().unwrap_or(0);
    for rank in 0..max {
        for ids in buckets.values() {
            if let Some(id) = ids.get(rank) {
                order.push(id.clone());
            }
        }
    }
    order
}

fn mesh_order(raw: &[String], by_id: &HashMap<String, Symbol>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen_files = BTreeSet::new();
    for id in raw {
        if let Some(s) = by_id.get(id) {
            if seen_files.insert(s.file_path.clone()) {
                out.push(id.clone());
            }
        }
    }
    for id in raw {
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

fn measure(
    selected: &[String],
    raw: &[String],
    relevant: &BTreeSet<String>,
    by_id: &HashMap<String, Symbol>,
    used: usize,
) -> Utility {
    let hit = selected.iter().filter(|id| relevant.contains(*id)).count();
    let first = selected.iter().position(|id| relevant.contains(id));
    let unique = selected.iter().collect::<BTreeSet<_>>().len();
    let files = selected
        .iter()
        .filter_map(|id| by_id.get(id).map(|s| s.file_path.clone()))
        .collect::<BTreeSet<_>>();
    let modules = files.iter().map(|p| module(p)).collect::<BTreeSet<_>>();
    let prefix = selected.iter().zip(raw).take_while(|(a, b)| a == b).count();
    Utility {
        n: 1,
        coverage: hit as f64 / relevant.len() as f64,
        mrr: first.map_or(0.0, |rank| 1.0 / (rank + 1) as f64),
        evidence_per_token: hit as f64 / used.max(1) as f64,
        unique_files: files.len() as f64,
        unique_modules: modules.len() as f64,
        direct_prefix_retention: prefix as f64 / raw.len().min(selected.len()).max(1) as f64,
        duplicate_rate: 1.0 - unique as f64 / selected.len().max(1) as f64,
        citation_availability: selected
            .iter()
            .filter(|id| {
                by_id
                    .get(*id)
                    .is_some_and(|s| !s.file_path.is_empty() && s.line_start > 0)
            })
            .count() as f64
            / selected.len().max(1) as f64,
    }
}
fn add(dst: &mut Utility, src: &Utility) {
    dst.n += src.n;
    dst.coverage += src.coverage;
    dst.mrr += src.mrr;
    dst.evidence_per_token += src.evidence_per_token;
    dst.unique_files += src.unique_files;
    dst.unique_modules += src.unique_modules;
    dst.direct_prefix_retention += src.direct_prefix_retention;
    dst.duplicate_rate += src.duplicate_rate;
    dst.citation_availability += src.citation_availability;
}

fn report(u: &Utility) -> serde_json::Value {
    let d = u.n.max(1) as f64;
    json!({"queries":u.n,"relevant_coverage":u.coverage/d,"mrr":u.mrr/d,
        "relevant_evidence_per_token_proxy":u.evidence_per_token/d,"unique_files":u.unique_files/d,
        "unique_modules":u.unique_modules/d,"direct_prefix_retention":u.direct_prefix_retention/d,
        "duplicate_rate":u.duplicate_rate/d,"citation_availability_proxy":u.citation_availability/d})
}

fn production_capsule(raw: &[String]) -> Vec<String> {
    let direct = raw
        .iter()
        .enumerate()
        .map(|(i, id)| Hit::new(id, 1.0 / (i + 1) as f64, "lexical", "proxy"))
        .collect::<Vec<_>>();
    compose_capsule(&[direct], &[], raw.len(), DEFAULT_RRF_K)
        .into_iter()
        .map(|h| h.symbol_id)
        .collect()
}

#[test]
fn synthetic_equal_budget_boundary_and_controls() {
    let symbols = vec![
        synthetic("short", "a.py", 40),
        synthetic("relevant", "b.py", 40),
        synthetic("long_irrelevant", "c.py", 4000),
    ];
    let by_id = symbols
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect::<HashMap<_, _>>();
    let relevant = BTreeSet::from(["relevant".to_string()]);
    let raw = vec!["short".into(), "relevant".into(), "long_irrelevant".into()];
    let (selected, used) = budgeted(&raw, &by_id, 50);
    assert!(used <= 50);
    assert_eq!(selected, vec!["short", "relevant"]);
    assert_eq!(
        measure(&selected, &raw, &relevant, &by_id, used).coverage,
        1.0
    );

    let too_long_first = vec!["long_irrelevant".into(), "relevant".into()];
    let (skip_oversize, _) = budgeted(&too_long_first, &by_id, 30);
    assert_eq!(
        skip_oversize,
        vec!["relevant"],
        "oversize item must not starve a later fitting item"
    );
    assert_eq!(
        production_capsule(&raw),
        raw,
        "single-layer production capsule preserves order"
    );
}

fn synthetic(id: &str, path: &str, body_chars: usize) -> Symbol {
    Symbol {
        id: id.into(),
        kind: cognis_core::SymbolKind::Function,
        name: id.into(),
        qualified_name: id.into(),
        language: "python".into(),
        module: module(path),
        file_path: path.into(),
        line_start: 1,
        line_end: 1,
        signature: None,
        docstring: None,
        content_hash: "h".into(),
        body_excerpt: Some("x".repeat(body_chars)),
        semantic_summary: None,
        risk_score: 0.0,
        ambiguous: false,
        untrusted_flags: vec![],
        updated_at: 0,
    }
}
#[test]
#[ignore = "diagnostic corpus proxy; run explicitly with --ignored --nocapture"]
fn objective_pr_capsule_utility_proxy() {
    let base = workspace_root().join(".benchmarks").join("public");
    let arms = [
        "raw_top_k",
        "production_compose_capsule",
        "hierarchy_card_proxy",
        "evidence_mesh_proxy",
    ];
    let mut macro_sums = arms
        .iter()
        .map(|a| (*a, Utility::default()))
        .collect::<HashMap<_, _>>();
    let mut repos = 0;
    for (tag, db_rel, golden_rel) in REPOS {
        let src = base.join(db_rel);
        let golden_path = base.join(golden_rel);
        if !src.is_file() || !golden_path.is_file() {
            eprintln!("[skip] {tag}: missing DB or objective PR golden");
            continue;
        }
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join(format!("{tag}.db"));
        std::fs::copy(src, &copy).unwrap();
        let db = Database::open(&copy).unwrap();
        let symbols = db.list_symbols().unwrap();
        let table = SymbolTable::from_db(&db).unwrap();
        let lexical = LexicalIndex::from_symbols(&symbols);
        let golden = load_golden_set(&golden_path).unwrap();
        let by_id = symbols
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect::<HashMap<_, _>>();
        let mut sums = arms
            .iter()
            .map(|a| (*a, Utility::default()))
            .collect::<HashMap<_, _>>();
        for q in &golden.queries {
            let relevant = table.resolve_relevant(q);
            if relevant.is_empty() {
                continue;
            }
            let raw = lexical.rank(&q.q);
            let orders = [
                ("raw_top_k", raw.clone()),
                ("production_compose_capsule", production_capsule(&raw)),
                ("hierarchy_card_proxy", hierarchy_order(&raw, &by_id)),
                ("evidence_mesh_proxy", mesh_order(&raw, &by_id)),
            ];
            for (arm, order) in orders {
                let (selected, used) = budgeted(&order, &by_id, TOKEN_BUDGET);
                let m = measure(&selected, &raw, &relevant, &by_id, used);
                add(sums.get_mut(arm).unwrap(), &m);
            }
        }
        if sums["raw_top_k"].n == 0 {
            db.close_thread_connection();
            continue;
        }
        repos += 1;
        let values = arms
            .iter()
            .map(|a| ((*a).into(), report(&sums[*a])))
            .collect::<serde_json::Map<_, _>>();
        eprintln!("{}", serde_json::to_string_pretty(&json!({"tier":"EMPIRICALLY_SUPPORTED","repo":tag,"explicit_token_proxy_budget":TOKEN_BUDGET,"arms":values})).unwrap());
        for arm in arms {
            let src = &sums[arm];
            let d = src.n as f64;
            let mut mean = src.clone();
            mean.n = 1;
            mean.coverage /= d;
            mean.mrr /= d;
            mean.evidence_per_token /= d;
            mean.unique_files /= d;
            mean.unique_modules /= d;
            mean.direct_prefix_retention /= d;
            mean.duplicate_rate /= d;
            mean.citation_availability /= d;
            add(macro_sums.get_mut(arm).unwrap(), &mean);
        }
        db.close_thread_connection();
    }
    if repos == 0 {
        eprintln!("SKIP capsule proxy: no local DB + objective PR golden pair; no corpus result fabricated.");
        return;
    }
    let values = arms
        .iter()
        .map(|a| ((*a).into(), report(&macro_sums[*a])))
        .collect::<serde_json::Map<_, _>>();
    eprintln!("{}", serde_json::to_string_pretty(&json!({
        "tier":"EMPIRICALLY_SUPPORTED","macro_unweighted_repos":repos,"arms":values,
        "proxy_warning":"NOT coding task success; token count is chars/4 proxy; citation availability is NOT exact citation validity",
        "conjectured_interpretation":"higher evidence density and diversity may help agents, but downstream patch+hidden-test evaluation is required"
    })).unwrap());
}
