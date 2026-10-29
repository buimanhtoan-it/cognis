//! Diagnostic spike for multi-resolution retrieval on objective PR labels.
//! Run with `cargo test -p cognis-eval --test spike_hierarchical_retrieval -- --ignored --nocapture`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use cognis_core::{Hit, Symbol};
use cognis_eval::bench::{compute_metrics, hub_ids, load_golden_set, LexicalIndex, SymbolTable};
use cognis_retrieval::{rrf_fuse, DEFAULT_RRF_K};
use cognis_store::{Database, SymbolStore};
use serde_json::json;

const CANDIDATE_K: usize = 50;
const FINAL_K: usize = 10;
const REPOS: &[(&str, &str, &str)] = &[
    ("requests", "db/requests.db", "golden/requests_pr.json"),
    ("fastapi", "db/fastapi.db", "golden/fastapi_pr.json"),
    ("eshop", "db/eshop.db", "golden/eshop_pr.json"),
    ("petclinic", "db/petclinic.db", "golden/petclinic_pr.json"),
    ("jsoup", "db/jsoup.db", "golden/jsoup_pr.json"),
];

#[derive(Clone)]
struct Card {
    key: String,
    tokens: BTreeSet<String>,
    members: Vec<String>,
}

#[derive(Default, Clone)]
struct Sum {
    n: usize,
    candidate_recall: f64,
    recall: f64,
    precision: f64,
    mrr: f64,
    contamination: f64,
}

impl Sum {
    fn add(&mut self, order: &[String], relevant: &BTreeSet<String>, hubs: &BTreeSet<String>) {
        self.n += 1;
        self.candidate_recall += compute_metrics(order, relevant, hubs, CANDIDATE_K).recall;
        let m = compute_metrics(order, relevant, hubs, FINAL_K);
        self.recall += m.recall;
        self.precision += m.precision;
        self.mrr += m.mrr;
        self.contamination += m.contamination;
    }

    fn value(&self) -> serde_json::Value {
        let d = self.n.max(1) as f64;
        json!({
            "queries": self.n, "candidate_recall_at_50": self.candidate_recall / d,
            "recall_at_10": self.recall / d, "precision_at_10": self.precision / d,
            "mrr": self.mrr / d, "hub_contamination_at_10": self.contamination / d,
        })
    }

    fn macro_value(&self, evaluated_queries: usize) -> serde_json::Value {
        let d = self.n.max(1) as f64;
        json!({
            "queries": evaluated_queries, "repos": self.n,
            "candidate_recall_at_50": self.candidate_recall / d,
            "recall_at_10": self.recall / d, "precision_at_10": self.precision / d,
            "mrr": self.mrr / d, "hub_contamination_at_10": self.contamination / d,
        })
    }
}
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn public_dir() -> PathBuf {
    workspace_root().join(".benchmarks").join("public")
}

fn hit(id: &str, score: f64, layer: &str) -> Hit {
    Hit::new(id, score, layer, "hierarchy spike")
}

fn path_parts(path: &str) -> Vec<String> {
    path.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
}

fn module_key(symbol: &Symbol) -> String {
    let parts = path_parts(&symbol.file_path);
    if parts.len() <= 1 {
        "<root>".into()
    } else {
        parts[..parts.len() - 1].join("/")
    }
}

fn top_key(symbol: &Symbol) -> String {
    path_parts(&symbol.file_path)
        .first()
        .cloned()
        .unwrap_or_else(|| "<root>".to_string())
}

fn cards(symbols: &[Symbol], key: impl Fn(&Symbol) -> String) -> Vec<Card> {
    let mut grouped: BTreeMap<String, Vec<&Symbol>> = BTreeMap::new();
    for symbol in symbols {
        grouped.entry(key(symbol)).or_default().push(symbol);
    }
    grouped
        .into_iter()
        .map(|(key, members)| {
            let mut ids = members.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
            ids.sort();
            let text = format!(
                "{} {}",
                key,
                members
                    .iter()
                    .map(|s| format!(
                        "{} {} {}",
                        s.name,
                        s.qualified_name,
                        s.body_excerpt.as_deref().unwrap_or("")
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            Card {
                key,
                tokens: cognis_eval::bench::camel_tokens(&text)
                    .into_iter()
                    .collect(),
                members: ids,
            }
        })
        .collect()
}

fn score_cards<'a>(cards: &'a [Card], query: &str) -> Vec<&'a Card> {
    let query_tokens = cognis_eval::bench::camel_tokens(query);
    let mut scored = cards
        .iter()
        .map(|card| {
            let score = query_tokens
                .iter()
                .filter(|token| card.tokens.contains(*token))
                .count();
            (card, score)
        })
        .filter(|(_, score)| *score > 0)
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.key.cmp(&b.0.key)));
    scored.into_iter().map(|(card, _)| card).collect()
}

fn expansion_layer(cards: &[Card], query: &str, layer: &str, limit: usize) -> Vec<Hit> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (card_rank, card) in score_cards(cards, query).into_iter().enumerate() {
        for (member_rank, id) in card.members.iter().enumerate() {
            if seen.insert(id.clone()) {
                out.push(hit(
                    id,
                    1.0 / ((card_rank + 1) * 1000 + member_rank + 1) as f64,
                    layer,
                ));
                if out.len() == limit {
                    return out;
                }
            }
        }
    }
    out
}

fn symbol_layer(lexical: &LexicalIndex, query: &str) -> Vec<Hit> {
    lexical
        .scores(query)
        .into_iter()
        .take(CANDIDATE_K)
        .map(|(id, score)| hit(&id, score, "symbol"))
        .collect()
}

fn arm_order(
    name: &str,
    query: &str,
    lexical: &LexicalIndex,
    files: &[Card],
    modules: &[Card],
    tops: &[Card],
) -> Vec<String> {
    let symbol = symbol_layer(lexical, query);
    let layers = match name {
        "symbol_only" => vec![symbol],
        "file_symbol" => vec![symbol, expansion_layer(files, query, "file", CANDIDATE_K)],
        "module_file_symbol" => vec![
            symbol,
            expansion_layer(files, query, "file", CANDIDATE_K),
            expansion_layer(modules, query, "module", CANDIDATE_K),
        ],
        "repo_map_fetch" => vec![
            symbol,
            expansion_layer(tops, query, "repo_map", CANDIDATE_K),
            expansion_layer(modules, query, "module", CANDIDATE_K),
            expansion_layer(files, query, "file", CANDIDATE_K),
        ],
        _ => unreachable!(),
    };
    rrf_fuse(&layers, CANDIDATE_K, DEFAULT_RRF_K)
        .into_iter()
        .map(|h| h.symbol_id)
        .collect()
}
#[test]
#[ignore = "diagnostic corpus benchmark; run explicitly with --ignored --nocapture"]
fn objective_pr_hierarchical_ablation() {
    let base = public_dir();
    let arms = [
        "symbol_only",
        "file_symbol",
        "module_file_symbol",
        "repo_map_fetch",
    ];
    let mut macro_sums: HashMap<&str, Sum> =
        arms.iter().map(|arm| (*arm, Sum::default())).collect();
    let mut repos_run = 0usize;
    let mut evaluated_queries = 0usize;

    for (tag, db_rel, golden_rel) in REPOS {
        let db_src = base.join(db_rel);
        let golden_path = base.join(golden_rel);
        if !db_src.is_file() || !golden_path.is_file() {
            eprintln!("{{\"tier\":\"EMPIRICALLY_SUPPORTED\",\"repo\":\"{tag}\",\"status\":\"skipped_missing_local_db_or_pr_golden\"}}");
            continue;
        }
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join(format!("{tag}.db"));
        std::fs::copy(&db_src, &db_path).unwrap();
        let db = Database::open(&db_path).unwrap();
        let symbols = db.list_symbols().unwrap();
        let table = SymbolTable::from_db(&db).unwrap();
        let golden = load_golden_set(&golden_path).unwrap();
        let lexical = LexicalIndex::from_symbols(&symbols);
        let files = cards(&symbols, |s| s.file_path.clone());
        let modules = cards(&symbols, module_key);
        let tops = cards(&symbols, top_key);
        let hubs = hub_ids(&db.build_code_graph(None).unwrap(), 0.10);
        let mut sums: HashMap<&str, Sum> = arms.iter().map(|arm| (*arm, Sum::default())).collect();

        for query in &golden.queries {
            let relevant = table.resolve_relevant(query);
            if relevant.is_empty() {
                continue;
            }
            for arm in arms {
                let order = arm_order(arm, &query.q, &lexical, &files, &modules, &tops);
                sums.get_mut(arm).unwrap().add(&order, &relevant, &hubs);
            }
        }
        if sums["symbol_only"].n == 0 {
            eprintln!("{{\"repo\":\"{tag}\",\"status\":\"skipped_no_resolved_pr_queries\"}}");
            db.close_thread_connection();
            continue;
        }
        repos_run += 1;
        evaluated_queries += sums["symbol_only"].n;
        let report = arms
            .iter()
            .map(|arm| ((*arm).to_string(), sums[*arm].value()))
            .collect::<serde_json::Map<_, _>>();
        eprintln!("{}", serde_json::to_string_pretty(&json!({"tier":"EMPIRICALLY_SUPPORTED","repo":tag,"candidate_k":CANDIDATE_K,"final_k":FINAL_K,"arms":report})).unwrap());
        for arm in arms {
            let source = &sums[arm];
            let target = macro_sums.get_mut(arm).unwrap();
            let d = source.n as f64;
            target.n += 1;
            target.candidate_recall += source.candidate_recall / d;
            target.recall += source.recall / d;
            target.precision += source.precision / d;
            target.mrr += source.mrr / d;
            target.contamination += source.contamination / d;
        }
        db.close_thread_connection();
    }

    if repos_run == 0 {
        eprintln!("SKIP hierarchical spike: no local .benchmarks/public DB + *_pr.json pair; no empirical corpus result fabricated.");
        return;
    }
    let macro_report = arms
        .iter()
        .map(|arm| {
            (
                (*arm).to_string(),
                macro_sums[*arm].macro_value(evaluated_queries),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let strongest = arms
        .iter()
        .max_by(|a, b| {
            let sa = &macro_sums[**a];
            let sb = &macro_sums[**b];
            (sa.recall / sa.n as f64)
                .partial_cmp(&(sb.recall / sb.n as f64))
                .unwrap()
                .then_with(|| a.cmp(b))
        })
        .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&json!({
        "tier":"EMPIRICALLY_SUPPORTED", "macro_unweighted_repos":repos_run,
        "arms":macro_report, "strongest_arm_by_recall_at_10":strongest,
        "honest_caveats":["lexical hierarchy proxy, not dense multi-resolution embeddings","objective PR labels only; concept sets omitted","file/module cards repeat symbol text and use no fitted constants","finite local snapshot; test passes regardless winner"]
    })).unwrap());
}
