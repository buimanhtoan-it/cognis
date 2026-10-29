//! Full-scan query behavior retained solely as an optimization oracle.
//!
//! This module intentionally reads every symbol or edge row. It is compiled
//! only when the dev/test-only `reference-scan` feature is explicitly enabled;
//! production query paths must use bounded store queries instead.

use std::collections::HashSet;

use cognis_core::{Hit, Result, Symbol};
use cognis_store::Database;

/// Pre-bounded-read symbol resolution and graph traversal over one database.
///
/// Keep this implementation behaviorally frozen. Its purpose is to let tests
/// compare optimized query identifiers and ordering against the old scan path.
pub struct ReferenceScan<'a> {
    db: &'a Database,
}

impl<'a> ReferenceScan<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    fn all_symbols(&self) -> Result<Vec<Symbol>> {
        self.db.list_symbols()
    }

    /// Hydrate requested ids by filtering the complete id-ordered symbol set.
    pub fn hydrate(&self, ids: &[String]) -> Result<Vec<Symbol>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        Ok(self
            .all_symbols()?
            .into_iter()
            .filter(|symbol| wanted.contains(symbol.id.as_str()))
            .collect())
    }

    /// Resolve with the pre-feature rank ladder over the complete symbol set.
    pub fn lookup(&self, name_or_id: &str, kind: Option<&str>) -> Result<Option<Symbol>> {
        let needle = name_or_id.trim();
        if needle.is_empty() {
            return Ok(None);
        }
        let needle_lower = needle.to_lowercase();
        let symbols = self.all_symbols()?;

        let kind_ok = |symbol: &Symbol| match kind {
            Some(wanted) => {
                serde_json::to_value(symbol.kind)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .as_deref()
                    == Some(wanted)
            }
            None => true,
        };

        let mut best: Option<(u32, &Symbol)> = None;
        for symbol in symbols.iter().filter(|symbol| kind_ok(symbol)) {
            let rank = if symbol.id == needle {
                6
            } else if symbol.qualified_name == needle {
                5
            } else if symbol.name == needle {
                4
            } else if symbol.name.to_lowercase() == needle_lower {
                3
            } else if symbol.name.to_lowercase().contains(&needle_lower)
                || symbol.qualified_name.to_lowercase().contains(&needle_lower)
            {
                2
            } else if symbol.id.to_lowercase().contains(&needle_lower) {
                1
            } else {
                0
            };
            if rank > 0 && best.map(|(best_rank, _)| rank > best_rank).unwrap_or(true) {
                best = Some((rank, symbol));
            }
        }
        Ok(best.map(|(_, symbol)| symbol.clone()))
    }
    /// Trace dependencies with the pre-feature all-edge scan at every hop.
    pub fn dependency_trace(
        &self,
        symbol_id: &str,
        direction: &str,
        depth: u8,
    ) -> Result<Vec<Hit>> {
        let edges = self.db.list_edges()?;
        let mut visited = HashSet::new();
        visited.insert(symbol_id.to_string());
        let mut frontier = vec![symbol_id.to_string()];
        let mut hits = Vec::new();

        for hop in 1..=depth {
            let mut next = Vec::new();
            for node in &frontier {
                for edge in &edges {
                    if edge.dst_missing() {
                        continue;
                    }
                    let neighbor = match direction {
                        "out" if edge.src_id == *node => Some(&edge.dst_id),
                        "in" if edge.dst_id == *node => Some(&edge.src_id),
                        "both" if edge.src_id == *node => Some(&edge.dst_id),
                        "both" if edge.dst_id == *node => Some(&edge.src_id),
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
}

#[cfg(test)]
mod tests {
    use cognis_core::{Edge, EdgeKind, Symbol, SymbolKind};
    use cognis_store::SymbolWriter;

    use super::*;

    fn symbol(id: &str, kind: SymbolKind, name: &str, qualified_name: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            language: "rust".to_string(),
            module: "fixture".to_string(),
            file_path: "src/lib.rs".to_string(),
            line_start: 1,
            line_end: 1,
            signature: None,
            docstring: None,
            content_hash: "fixture".to_string(),
            body_excerpt: None,
            semantic_summary: None,
            risk_score: 0.0,
            ambiguous: false,
            untrusted_flags: Vec::new(),
            updated_at: 1,
        }
    }

    fn edge(src_id: &str, dst_id: &str, missing: bool) -> Edge {
        Edge {
            src_id: src_id.to_string(),
            dst_id: dst_id.to_string(),
            kind: EdgeKind::Calls,
            confidence: 1.0,
            meta: if missing {
                serde_json::json!({ "dst_missing": true })
            } else {
                serde_json::Value::Null
            },
        }
    }

    fn fixture() -> Database {
        let mut db = Database::new(":memory:");
        db.connect().expect("migrate fixture");
        db.upsert_symbols(&[
            symbol("a-id", SymbolKind::Function, "Alpha", "fixture.Alpha"),
            symbol("b-id", SymbolKind::Method, "alpha", "fixture.alpha"),
            symbol("c-id", SymbolKind::Function, "alphabet", "fixture.alphabet"),
            symbol("d-id", SymbolKind::Function, "delta", "fixture.delta"),
        ])
        .expect("write symbols");
        db.upsert_edges(&[
            edge("a-id", "b-id", false),
            edge("b-id", "c-id", false),
            edge("c-id", "d-id", true),
        ])
        .expect("write edges");
        db
    }

    #[test]
    fn reference_lookup_preserves_rank_kind_and_id_order_ties() {
        let db = fixture();
        let scan = ReferenceScan::new(&db);

        assert_eq!(scan.lookup("b-id", None).unwrap().unwrap().id, "b-id");
        assert_eq!(
            scan.lookup("fixture.Alpha", None).unwrap().unwrap().id,
            "a-id"
        );
        assert_eq!(scan.lookup("alpha", None).unwrap().unwrap().id, "b-id");
        assert_eq!(scan.lookup("ALPHA", None).unwrap().unwrap().id, "a-id");
        assert_eq!(
            scan.lookup("alpha", Some("function")).unwrap().unwrap().id,
            "a-id"
        );
        assert!(scan.lookup("alpha", Some("class")).unwrap().is_none());
        assert!(scan.lookup("  ", None).unwrap().is_none());
    }

    #[test]
    fn reference_hydrate_and_trace_preserve_scan_order_and_missing_filter() {
        let db = fixture();
        let scan = ReferenceScan::new(&db);

        let ids = vec![
            "c-id".to_string(),
            "a-id".to_string(),
            "unknown".to_string(),
        ];
        let hydrated: Vec<_> = scan
            .hydrate(&ids)
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.id)
            .collect();
        assert_eq!(hydrated, ["a-id", "c-id"]);

        let traced: Vec<_> = scan
            .dependency_trace("a-id", "out", 3)
            .unwrap()
            .into_iter()
            .map(|hit| hit.symbol_id)
            .collect();
        assert_eq!(traced, ["b-id", "c-id"]);
        assert!(scan
            .dependency_trace("a-id", "sideways", 3)
            .unwrap()
            .is_empty());
    }
}
