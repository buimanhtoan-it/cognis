//! Static + Graph-Propagated Algorithmic Complexity & Optimization Engine.
//!
//! Implements the 3-tier analysis architecture:
//! * **Tier 1 (Intra-procedural AST Pass)**: Full Tree-sitter traversal over symbol
//!   bodies, classifying loop bounds (`O(1)`, `O(log n)`, `O(sqrt(n))`, `O(n)`),
//!   nested loop depth, built-in linear scans/sorts, heap allocations in loops,
//!   and blocking I/O / SQL / sequential `await` inside loops.
//! * **Tier 2 (Inter-procedural Call-Graph Propagation)**: Tarjan's Strongly
//!   Connected Components (SCC) condensation + Master Theorem recurrence solver
//!   + bottom-up DAG symbolic polynomial propagation ($\oplus = \max, \otimes = +$).
//! * **Tier 3 (Repo-Native Optimization Advisor)**: Zero-LLM symbolic rule engine
//!   (`OPT-001`..`OPT-007`) that cross-references the repository's symbols to
//!   prescribe concrete data-structure and batching refactorings.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use cognis_core::{
    ArgReduction, AstCallSite, AstComplexitySummary, BottleneckStep, ComplexityClass, Edge,
    EdgeKind, IntrinsicPattern, IoEffectKind, LoopBound, OptimizationAdvisory, Symbol,
    SymbolComplexityReport, SymbolKind,
};

use crate::parser::language_for_path;

static AST_SUMMARY_CACHE: OnceLock<Mutex<HashMap<(String, u64), AstComplexitySummary>>> =
    OnceLock::new();

fn hash_source_key(sym: &Symbol, source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    sym.content_hash.hash(&mut hasher);
    sym.line_start.hash(&mut hasher);
    source.hash(&mut hasher);
    hasher.finish()
}

/// Analyze a single [`Symbol`]'s AST complexity from its source snippet or `body_excerpt`.
/// Memoized in a bounded process-wide cache keyed by `(symbol.id, content_hash + source_hash)`
/// so repeated inter-procedural and shadow-patch evaluations skip redundant Tree-sitter parses.
pub fn analyze_symbol_ast(
    sym: &Symbol,
    full_source_override: Option<&str>,
) -> AstComplexitySummary {
    let source = full_source_override
        .or(sym.body_excerpt.as_deref())
        .unwrap_or("");
    if source.trim().is_empty()
        || !matches!(
            sym.kind,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Route
        )
    {
        return AstComplexitySummary {
            symbol_id: sym.id.clone(),
            local_time: ComplexityClass::Constant,
            local_space: ComplexityClass::Constant,
            max_loop_depth: 0,
            cyclomatic: 1,
            cognitive: 0,
            call_sites: Vec::new(),
            io_effects: Vec::new(),
            intrinsic_patterns: Vec::new(),
            has_memoization: false,
            is_direct_recursive: false,
        };
    }

    let cache_key = (sym.id.clone(), hash_source_key(sym, source));
    let cache = AST_SUMMARY_CACHE.get_or_init(|| Mutex::new(HashMap::with_capacity(1024)));
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.get(&cache_key) {
            return cached.clone();
        }
    }

    let mut walker = AstComplexityWalker::new(sym, source);
    let summary = 'parse: {
        if let Some(lang) = language_for_path(&sym.file_path) {
            let mut parser = tree_sitter::Parser::new();
            if parser.set_language(&lang.ts_language()).is_ok() {
                if let Some(tree) = parser.parse(source.as_bytes(), None) {
                    walker.visit(tree.root_node(), 0);
                    break 'parse walker.finish();
                }
            }
        }
        walker.fallback_lexical_scan();
        walker.finish()
    };

    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= 8192 {
            guard.clear();
        }
        guard.insert(cache_key, summary.clone());
    }
    summary
}

struct AstComplexityWalker<'a> {
    sym: &'a Symbol,
    src: &'a [u8],
    src_str: &'a str,
    loop_stack: Vec<LoopBound>,
    max_loop_depth: u8,
    local_time: ComplexityClass,
    local_space: ComplexityClass,
    cyclomatic: u32,
    cognitive: u32,
    call_sites_by_name: HashMap<String, AstCallSite>,
    io_effects: Vec<(IoEffectKind, u32, u8)>,
    intrinsic_patterns: Vec<IntrinsicPattern>,
    has_memoization: bool,
    is_direct_recursive: bool,
    past_declaration_header: bool,
}

impl<'a> AstComplexityWalker<'a> {
    fn new(sym: &'a Symbol, src_str: &'a str) -> Self {
        let has_memoization = src_str.contains("lru_cache")
            || src_str.contains("@cache")
            || src_str.contains("memo.get")
            || src_str.contains("memo[")
            || src_str.contains("cache.get")
            || src_str.contains("visited.has")
            || src_str.contains("visited.contains");
        Self {
            sym,
            src: src_str.as_bytes(),
            src_str,
            loop_stack: Vec::new(),
            max_loop_depth: 0,
            local_time: ComplexityClass::Constant,
            local_space: ComplexityClass::Constant,
            cyclomatic: 1,
            cognitive: 0,
            call_sites_by_name: HashMap::new(),
            io_effects: Vec::new(),
            intrinsic_patterns: Vec::new(),
            has_memoization,
            is_direct_recursive: false,
            past_declaration_header: false,
        }
    }

    fn current_loop_depth(&self) -> u8 {
        self.loop_stack
            .iter()
            .filter(|b| !matches!(b, LoopBound::Constant(_)))
            .count() as u8
    }

    fn current_loop_multiplier(&self) -> ComplexityClass {
        self.loop_stack
            .iter()
            .fold(ComplexityClass::Constant, |acc, bound| {
                acc.nest_mul(bound.to_complexity())
            })
    }

    fn node_text(&self, node: tree_sitter::Node<'_>) -> &'a str {
        node.utf8_text(self.src).unwrap_or("")
    }

    fn node_line(&self, node: tree_sitter::Node<'_>) -> u32 {
        self.sym
            .line_start
            .saturating_add(node.start_position().row as u32)
    }

    fn visit(&mut self, node: tree_sitter::Node<'_>, control_nesting: u32) {
        let kind = node.kind();

        // Mark once we enter a block/body so signature self-name doesn't count as recursion.
        if kind.contains("block")
            || kind.contains("body")
            || kind == "compound_statement"
            || kind == "expression_statement"
        {
            self.past_declaration_header = true;
        }

        // 1. Check if this AST node is a loop construct.
        if is_loop_node_kind(kind) {
            self.past_declaration_header = true;
            let bound = self.classify_loop_node(node);
            self.cyclomatic += 1;
            self.cognitive += 1 + control_nesting;

            self.loop_stack.push(bound);
            let eff_depth = self.current_loop_depth();
            self.max_loop_depth = self.max_loop_depth.max(eff_depth);
            let mult = self.current_loop_multiplier();
            self.local_time = self.local_time.seq_max(mult);

            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                self.visit(child, control_nesting + 1);
            }
            self.loop_stack.pop();
            return;
        }

        // 2. Branching constructs (if / match / switch / ternary / catch).
        if is_branch_node_kind(kind) {
            self.cyclomatic += 1;
            self.cognitive += 1 + control_nesting;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                self.visit(child, control_nesting + 1);
            }
            return;
        }

        // 3. Sequential `await` inside loop detection.
        if (kind == "await_expression" || kind == "await") && self.current_loop_depth() > 0 {
            let line = self.node_line(node);
            let depth = self.current_loop_depth();
            if !self
                .io_effects
                .iter()
                .any(|(k, l, _)| *k == IoEffectKind::SequentialAwaitInLoop && *l == line)
            {
                self.io_effects
                    .push((IoEffectKind::SequentialAwaitInLoop, line, depth));
            }
        }

        // 4. Augmented assignment (`s += chunk`) or Python `x in list` comparison inside loop.
        if (kind == "augmented_assignment"
            || kind == " augmented_assignment_expression"
            || kind == "assignment_expression")
            && self.current_loop_depth() > 0
        {
            let text = self.node_text(node);
            if text.contains("+=")
                && (text.contains('"')
                    || text.contains('\'')
                    || text.contains("str")
                    || text.contains("html")
                    || text.contains("out")
                    || text.contains("buf")
                    || text.contains("res"))
            {
                let lhs = text.split("+=").next().unwrap_or("str").trim().to_string();
                let depth = self.current_loop_depth();
                let line = self.node_line(node);
                self.intrinsic_patterns
                    .push(IntrinsicPattern::StringConcatInLoop {
                        target: lhs,
                        line,
                        loop_depth: depth,
                    });
                self.local_time = self.local_time.seq_max(
                    self.current_loop_multiplier()
                        .nest_mul(ComplexityClass::linear()),
                );
            }
        }

        if kind == "comparison_operator" && self.current_loop_depth() > 0 {
            let text = self.node_text(node);
            if let Some((_, rhs)) = text.split_once(" in ") {
                let rhs_trim = rhs.trim();
                if !rhs_trim.contains("set")
                    && !rhs_trim.contains("dict")
                    && !rhs_trim.contains("map")
                    && !rhs_trim.starts_with('{')
                {
                    let depth = self.current_loop_depth();
                    let line = self.node_line(node);
                    self.intrinsic_patterns
                        .push(IntrinsicPattern::LinearScanInLoop {
                            receiver: Some(rhs_trim.to_string()),
                            method: "in".to_string(),
                            line,
                            loop_depth: depth,
                        });
                    self.local_time = self.local_time.seq_max(
                        self.current_loop_multiplier()
                            .nest_mul(ComplexityClass::linear()),
                    );
                }
            }
        }

        // 5. Call expressions (higher-order loops, intrinsics, I/O, and call graph sites).
        if is_call_node_kind(kind) {
            self.handle_call_node(node, control_nesting);
            return;
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, control_nesting);
        }
    }

    fn classify_loop_node(&self, node: tree_sitter::Node<'_>) -> LoopBound {
        let text = self.node_text(node);
        let header = text.lines().next().unwrap_or(text);

        // Check for halving / doubling logarithmic step in loop text (`*= 2`, `/= 2`, `>>= 1`, `<<= 1`, `mid =`).
        if text.contains("*= 2")
            || text.contains("/= 2")
            || text.contains("//= 2")
            || text.contains(">>= 1")
            || text.contains("<<= 1")
            || (header.contains("while") && (text.contains("mid =") || text.contains("mid :=")))
        {
            return LoopBound::Logarithmic;
        }

        // Check for sqrt bound (`i * i <=` or `sqrt(` in header).
        if header.contains("* i <=")
            || header.contains("*i <=")
            || header.contains("sqrt(")
            || header.contains("isqrt(")
        {
            return LoopBound::Sqrt;
        }

        // Check for small constant range (`range(10)` or `i < 16`).
        if let Some(idx) = header.find("range(") {
            let after = &header[idx + 6..];
            if let Some(end) = after.find(')') {
                let arg = after[..end].trim();
                if let Ok(n) = arg.parse::<u32>() {
                    if n <= 256 {
                        return LoopBound::Constant(n);
                    }
                }
            }
        }

        LoopBound::Linear { collection: None }
    }

    fn handle_call_node(&mut self, node: tree_sitter::Node<'_>, control_nesting: u32) {
        let text = self.node_text(node);
        let line = self.node_line(node);
        let callee_part = text.split('(').next().unwrap_or("").trim();
        let (receiver, bare_name) = match callee_part.rsplit_once('.') {
            Some((recv, method)) => (Some(recv.trim().to_string()), method.trim()),
            None => match callee_part.rsplit_once("::") {
                Some((recv, method)) => (Some(recv.trim().to_string()), method.trim()),
                None => (None, callee_part),
            },
        };

        // Check chained eager array allocations (`.filter(...).map(...).filter(...)`).
        let eager_chain_count = text.matches(".map(").count()
            + text.matches(".filter(").count()
            + text.matches(".slice(").count()
            + text.matches(".concat(").count();
        if eager_chain_count >= 3 {
            self.intrinsic_patterns
                .push(IntrinsicPattern::ChainedAllocation {
                    chain: callee_part.chars().take(60).collect(),
                    depth: eager_chain_count as u8,
                    line,
                });
            self.local_space = self.local_space.seq_max(ComplexityClass::linear());
        }

        // Check if this call is a functional higher-order loop (`.map`, `.forEach`, `.filter`, `.reduce`, `.Select`, `.Where`).
        let is_higher_order_loop = receiver.is_some()
            && matches!(
                bare_name,
                "map" | "forEach" | "filter" | "reduce" | "flatMap" | "Select" | "Where" | "each"
            );

        if is_higher_order_loop {
            self.loop_stack.push(LoopBound::Linear {
                collection: receiver.clone(),
            });
            let eff_depth = self.current_loop_depth();
            self.max_loop_depth = self.max_loop_depth.max(eff_depth);
            self.local_time = self.local_time.seq_max(self.current_loop_multiplier());
        }

        let loop_depth = self.current_loop_depth();
        let loop_mult = self.current_loop_multiplier();

        // Intrinsic O(n) linear scans: `.find(`, `.includes(`, `.indexOf(`, `.contains(`, `.Any(`.
        if matches!(
            bare_name,
            "find"
                | "findIndex"
                | "includes"
                | "indexOf"
                | "contains"
                | "Contains"
                | "Any"
                | "FirstOrDefault"
                | "count"
        ) {
            let scan_cost = loop_mult.nest_mul(ComplexityClass::linear());
            self.local_time = self.local_time.seq_max(scan_cost);
            if loop_depth > 0 {
                self.intrinsic_patterns
                    .push(IntrinsicPattern::LinearScanInLoop {
                        receiver: receiver.clone(),
                        method: bare_name.to_string(),
                        line,
                        loop_depth,
                    });
            }
        }

        // Intrinsic O(n log n) sort: `.sort(`, `sorted(`, `Slice(`, `OrderBy(`.
        if matches!(
            bare_name,
            "sort" | "sorted" | "Slice" | "OrderBy" | "sort_by"
        ) {
            let sort_cost = loop_mult.nest_mul(ComplexityClass::n_log_n());
            self.local_time = self.local_time.seq_max(sort_cost);
            if loop_depth > 0 {
                self.intrinsic_patterns.push(IntrinsicPattern::SortInLoop {
                    method: bare_name.to_string(),
                    line,
                    loop_depth,
                });
            }
        }

        // Intrinsic clone / allocation inside loop.
        if loop_depth > 0
            && matches!(
                bare_name,
                "clone" | "deepcopy" | "structuredClone" | "to_vec" | "to_owned"
            )
        {
            self.intrinsic_patterns.push(IntrinsicPattern::CloneInLoop {
                method: bare_name.to_string(),
                line,
                loop_depth,
            });
            self.local_space = self
                .local_space
                .seq_max(loop_mult.nest_mul(ComplexityClass::linear()));
        }

        // Heap growth inside loop (`.push(`, `.append(`, `.insert(`).
        if loop_depth > 0 && matches!(bare_name, "push" | "append" | "insert" | "put" | "Add") {
            self.local_space = self.local_space.seq_max(loop_mult);
        }

        // Database / Network / Filesystem I/O inside loop.
        if loop_depth > 0 {
            if is_database_call(bare_name, callee_part, text) {
                self.io_effects
                    .push((IoEffectKind::DatabaseQuery, line, loop_depth));
            } else if is_network_call(bare_name, callee_part) {
                self.io_effects
                    .push((IoEffectKind::NetworkRpc, line, loop_depth));
            } else if is_fs_blocking_call(bare_name, callee_part) {
                self.io_effects
                    .push((IoEffectKind::FilesystemBlocking, line, loop_depth));
            }
        }

        // Record call site for inter-procedural propagation.
        if is_valid_ident(bare_name) && !is_higher_order_loop {
            let args_text = text.split_once('(').map(|(_, a)| a).unwrap_or("");
            let arg_reduction = classify_arg_reduction(args_text);

            if self.past_declaration_header && bare_name == self.sym.name {
                self.is_direct_recursive = true;
            }

            let entry = self
                .call_sites_by_name
                .entry(bare_name.to_string())
                .or_insert_with(|| AstCallSite {
                    callee_name: bare_name.to_string(),
                    receiver: receiver.clone(),
                    line,
                    loop_depth,
                    loop_multiplier: loop_mult,
                    call_sites: 0,
                    arg_reduction,
                });
            entry.call_sites = entry.call_sites.saturating_add(1);
            if loop_depth > entry.loop_depth {
                entry.loop_depth = loop_depth;
                entry.loop_multiplier = loop_mult;
                entry.line = line;
            }
            if entry.arg_reduction == ArgReduction::None && arg_reduction != ArgReduction::None {
                entry.arg_reduction = arg_reduction;
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, control_nesting);
        }

        if is_higher_order_loop {
            self.loop_stack.pop();
        }
    }

    fn fallback_lexical_scan(&mut self) {
        let mut active_loops: u8 = 0;
        for (idx, raw_line) in self.src_str.lines().enumerate() {
            let line_no = self.sym.line_start.saturating_add(idx as u32);
            let trimmed = raw_line.trim();
            if trimmed.starts_with("for ")
                || trimmed.starts_with("for(")
                || trimmed.starts_with("while ")
                || trimmed.starts_with("while(")
            {
                active_loops = active_loops.saturating_add(1);
                self.max_loop_depth = self.max_loop_depth.max(active_loops);
                self.local_time = self
                    .local_time
                    .seq_max(ComplexityClass::polynomial(active_loops));
            }
            if active_loops > 0
                && (trimmed.contains(".find(")
                    || trimmed.contains(".includes(")
                    || trimmed.contains(".indexOf("))
            {
                self.local_time = self
                    .local_time
                    .seq_max(ComplexityClass::polynomial(active_loops + 1));
                self.intrinsic_patterns
                    .push(IntrinsicPattern::LinearScanInLoop {
                        receiver: None,
                        method: "find/includes".to_string(),
                        line: line_no,
                        loop_depth: active_loops,
                    });
            }
        }
    }

    fn finish(self) -> AstComplexitySummary {
        let mut call_sites: Vec<AstCallSite> = self.call_sites_by_name.into_values().collect();
        call_sites.sort_by_key(|a| a.line);
        AstComplexitySummary {
            symbol_id: self.sym.id.clone(),
            local_time: self.local_time,
            local_space: self.local_space,
            max_loop_depth: self.max_loop_depth,
            cyclomatic: self.cyclomatic,
            cognitive: self.cognitive,
            call_sites,
            io_effects: self.io_effects,
            intrinsic_patterns: self.intrinsic_patterns,
            has_memoization: self.has_memoization,
            is_direct_recursive: self.is_direct_recursive,
        }
    }
}

fn is_loop_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "for_statement"
            | "for_in_statement"
            | "for_each_statement"
            | "enhanced_for_statement"
            | "for_expression"
            | "while_statement"
            | "while_expression"
            | "do_statement"
            | "loop_expression"
            | "list_comprehension"
            | "dictionary_comprehension"
            | "set_comprehension"
            | "generator_expression"
    )
}

fn is_branch_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "if_statement"
            | "if_expression"
            | "elif_clause"
            | "switch_case"
            | "match_arm"
            | "ternary_expression"
            | "conditional_expression"
            | "catch_clause"
    )
}

fn is_call_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "call_expression" | "invocation_expression" | "call" | "method_invocation"
    )
}

fn is_valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn classify_arg_reduction(args_text: &str) -> ArgReduction {
    if args_text.contains("/ 2")
        || args_text.contains("/2")
        || args_text.contains("// 2")
        || args_text.contains(">> 1")
        || args_text.contains("mid")
    {
        ArgReduction::Halving
    } else if args_text.contains("- 1")
        || args_text.contains("-1")
        || args_text.contains("- 2")
        || args_text.contains("+ 1")
    {
        ArgReduction::Decrement
    } else {
        ArgReduction::None
    }
}

fn is_database_call(bare: &str, callee: &str, full_text: &str) -> bool {
    let lower_full = full_text.to_ascii_lowercase();
    matches!(
        bare,
        "query"
            | "execute"
            | "query_row"
            | "query_map"
            | "findOne"
            | "findUnique"
            | "findFirst"
            | "findMany"
            | "raw"
    ) || callee.contains("db.")
        || callee.contains("conn.")
        || callee.contains("cursor.")
        || callee.contains("prisma.")
        || lower_full.contains("select ")
        || lower_full.contains("insert into ")
        || lower_full.contains("update ")
}

fn is_network_call(bare: &str, callee: &str) -> bool {
    matches!(bare, "fetch" | "post" | "put" | "delete")
        && (bare == "fetch"
            || callee.contains("http")
            || callee.contains("axios")
            || callee.contains("requests")
            || callee.contains("httpx")
            || callee.contains("client"))
}

fn is_fs_blocking_call(bare: &str, callee: &str) -> bool {
    matches!(
        bare,
        "readFileSync"
            | "writeFileSync"
            | "read_to_string"
            | "ReadFile"
            | "WriteFile"
            | "ReadAllText"
    ) || callee.starts_with("fs.")
}

// ---------------------------------------------------------------------------
// Tier 2 & Tier 3: Inter-Procedural Tarjan SCC Propagator & Optimization Advisor
// ---------------------------------------------------------------------------

/// Compute inter-procedural complexity reports, call-edge loop depths, and
/// repo-grounded optimization advisories across `symbols` and `edges`.
pub fn analyze_workspace_complexity(
    symbols: &[Symbol],
    edges: &[Edge],
    source_overrides: &HashMap<String, String>,
) -> (
    HashMap<String, SymbolComplexityReport>,
    HashMap<(String, String), u8>,
) {
    let n = symbols.len();
    let mut sym_by_id: HashMap<String, &Symbol> = HashMap::with_capacity(n);
    let mut id_to_idx: HashMap<String, usize> = HashMap::with_capacity(n);
    let mut ast_by_id: HashMap<String, AstComplexitySummary> = HashMap::with_capacity(n);

    // Index existing repository containers/batch helpers by module & name for Tier 3 Advisor.
    let mut repo_helpers_by_module: HashMap<String, Vec<&Symbol>> = HashMap::new();
    for (i, sym) in symbols.iter().enumerate() {
        sym_by_id.insert(sym.id.clone(), sym);
        id_to_idx.insert(sym.id.clone(), i);
        repo_helpers_by_module
            .entry(sym.module.clone())
            .or_default()
            .push(sym);

        let override_src = source_overrides.get(&sym.id).map(String::as_str);
        let ast = analyze_symbol_ast(sym, override_src);
        ast_by_id.insert(sym.id.clone(), ast);
    }

    // Build directed call graph `u -> v` and match AST call sites to resolved `Calls` edges.
    let mut adj: Vec<Vec<CallEdgeMeta>> = vec![Vec::new(); n];
    let mut edge_loop_depths: HashMap<(String, String), u8> = HashMap::new();

    for edge in edges {
        if edge.kind != EdgeKind::Calls || edge.dst_missing() {
            continue;
        }
        let (Some(&u), Some(&v)) = (id_to_idx.get(&edge.src_id), id_to_idx.get(&edge.dst_id))
        else {
            continue;
        };
        let callee_sym = &symbols[v];
        let mut loop_depth = edge
            .meta
            .get("loop_depth")
            .and_then(|val| val.as_u64())
            .map(|d| d as u8)
            .unwrap_or(0);
        let mut call_sites = 1u16;
        let mut call_line = symbols[u].line_start;
        let mut arg_red = ArgReduction::None;

        if let Some(ast) = ast_by_id.get(&edge.src_id) {
            if let Some(cs) = ast
                .call_sites
                .iter()
                .find(|cs| cs.callee_name == callee_sym.name)
            {
                loop_depth = loop_depth.max(cs.loop_depth);
                call_sites = cs.call_sites;
                call_line = cs.line;
                arg_red = cs.arg_reduction;
            }
        }
        edge_loop_depths.insert((edge.src_id.clone(), edge.dst_id.clone()), loop_depth);
        adj[u].push((v, loop_depth, call_sites, call_line, arg_red));
    }

    // Also add direct recursive self-edges discovered by the AST walker.
    for (u, sym) in symbols.iter().enumerate() {
        if let Some(ast) = ast_by_id.get(&sym.id) {
            if ast.is_direct_recursive {
                if let Some(cs) = ast.call_sites.iter().find(|c| c.callee_name == sym.name) {
                    if !adj[u].iter().any(|(v, ..)| *v == u) {
                        adj[u].push((u, cs.loop_depth, cs.call_sites, cs.line, cs.arg_reduction));
                    }
                }
            }
        }
    }

    // Run Tarjan's SCC algorithm (produces SCCs in reverse topological order: leaves first!).
    let sccs = tarjan_scc(n, &adj);

    let mut propagated_time: Vec<ComplexityClass> = vec![ComplexityClass::Constant; n];
    let mut propagated_space: Vec<ComplexityClass> = vec![ComplexityClass::Constant; n];
    let mut recursion_kind: Vec<&'static str> = vec!["none"; n];
    let mut dominant_callee: Vec<Option<(usize, u8, u32)>> = vec![None; n];
    let mut transitive_io: Vec<Option<(IoEffectKind, usize, u32)>> = vec![None; n];

    for scc in &sccs {
        let scc_set: HashSet<usize> = scc.iter().copied().collect();
        let is_recursive_scc =
            scc.len() > 1 || (scc.len() == 1 && adj[scc[0]].iter().any(|(v, ..)| *v == scc[0]));

        if is_recursive_scc {
            let r_kind = if scc.len() > 1 {
                "mutual_recursion"
            } else {
                "direct_recursion"
            };
            let mut total_branch_calls: u16 = 0;
            let mut max_loop_in_cycle: u8 = 0;
            let mut reduction = ArgReduction::None;
            let mut local_work = ComplexityClass::Constant;
            let mut any_memo = false;

            for &u in scc {
                recursion_kind[u] = r_kind;
                if let Some(ast) = ast_by_id.get(&symbols[u].id) {
                    local_work = local_work.seq_max(ast.local_time);
                    any_memo |= ast.has_memoization;
                }
                for &(v, l_depth, sites, _, arg_red) in &adj[u] {
                    if scc_set.contains(&v) {
                        total_branch_calls = total_branch_calls.saturating_add(sites);
                        max_loop_in_cycle = max_loop_in_cycle.max(l_depth);
                        if reduction == ArgReduction::None && arg_red != ArgReduction::None {
                            reduction = arg_red;
                        }
                    }
                }
            }

            // Master Theorem classification on the recursive SCC.
            let scc_time = if !any_memo && (total_branch_calls >= 2 || max_loop_in_cycle > 0) {
                match reduction {
                    ArgReduction::Halving => ComplexityClass::n_log_n().seq_max(local_work),
                    _ => ComplexityClass::Exponential {
                        base_calls: total_branch_calls.clamp(2, 8) as u8,
                    },
                }
            } else {
                match reduction {
                    ArgReduction::Halving => ComplexityClass::log_n().nest_mul(local_work),
                    _ => ComplexityClass::linear().nest_mul(local_work),
                }
            };
            let scc_space = match reduction {
                ArgReduction::Halving => ComplexityClass::log_n(),
                _ => ComplexityClass::linear(),
            };

            for &u in scc {
                propagated_time[u] = scc_time;
                propagated_space[u] = scc_space;
            }
        }

        // Propagate from callees outside the SCC (which are already solved due to reverse topo order).
        for &u in scc {
            let sym = &symbols[u];
            let ast = &ast_by_id[&sym.id];
            let mut best_time = propagated_time[u].seq_max(ast.local_time);
            let mut best_space = propagated_space[u].seq_max(ast.local_space);

            if let Some(&(io_kind, io_line, _)) = ast.io_effects.first() {
                transitive_io[u] = Some((io_kind, u, io_line));
            }

            for &(v, loop_depth, _, call_line, _) in &adj[u] {
                if scc_set.contains(&v) {
                    continue;
                }
                let edge_mult = ComplexityClass::polynomial(loop_depth);
                let induced_time = edge_mult.nest_mul(propagated_time[v]);
                if induced_time.effective_degree() > best_time.effective_degree()
                    || (induced_time.effective_degree() >= 2.0 && dominant_callee[u].is_none())
                {
                    best_time = best_time.seq_max(induced_time);
                    dominant_callee[u] = Some((v, loop_depth, call_line));
                }
                best_space = best_space.seq_max(propagated_space[v]);

                // Lift I/O effects if called inside a loop (`loop_depth >= 1`) or if `v` already has loop I/O.
                if let Some((io_kind, orig_node, orig_line)) = transitive_io[v] {
                    if loop_depth > 0 && transitive_io[u].is_none() {
                        transitive_io[u] = Some((io_kind, orig_node, orig_line));
                    }
                } else if loop_depth > 0 {
                    // Check if callee `v` performs a direct DB/network call even at depth 0 inside `v`.
                    if symbol_has_direct_io(&symbols[v]) && transitive_io[u].is_none() {
                        transitive_io[u] =
                            Some((IoEffectKind::DatabaseQuery, v, symbols[v].line_start));
                    }
                }
            }

            propagated_time[u] = best_time;
            propagated_space[u] = best_space;
        }
    }

    // Build per-symbol reports, bottleneck chains, and Tier 3 Optimization Advisories.
    let mut reports = HashMap::with_capacity(n);
    for (u, sym) in symbols.iter().enumerate() {
        let ast = &ast_by_id[&sym.id];
        let prop_time = propagated_time[u];
        let prop_space = propagated_space[u];

        // Reconstruct bottleneck chain along `dominant_callee`.
        let mut chain = Vec::new();
        let mut curr = u;
        let mut visited_chain = HashSet::new();
        visited_chain.insert(curr);
        chain.push(BottleneckStep {
            symbol_id: sym.id.clone(),
            qualified_name: sym.qualified_name.clone(),
            file_path: sym.file_path.clone(),
            line: sym.line_start,
            local_contribution: ast.local_time.notation(),
            call_loop_multiplier: "O(1)".to_string(),
            cumulative_complexity: prop_time.notation(),
            reason: if dominant_callee[u].is_some() {
                "Calls helper inside loop / hot path".to_string()
            } else {
                "Local AST body complexity".to_string()
            },
        });

        while let Some((next_v, l_depth, call_line)) = dominant_callee[curr] {
            if !visited_chain.insert(next_v) || chain.len() >= 6 {
                break;
            }
            let next_sym = &symbols[next_v];
            let next_ast = &ast_by_id[&next_sym.id];
            chain.push(BottleneckStep {
                symbol_id: next_sym.id.clone(),
                qualified_name: next_sym.qualified_name.clone(),
                file_path: next_sym.file_path.clone(),
                line: call_line,
                local_contribution: next_ast.local_time.notation(),
                call_loop_multiplier: ComplexityClass::polynomial(l_depth).notation(),
                cumulative_complexity: propagated_time[next_v].notation(),
                reason: format!(
                    "Invoked at {}:{} inside loop depth {}",
                    sym.file_path, call_line, l_depth
                ),
            });
            curr = next_v;
        }

        let advisories = synthesize_advisories(
            u,
            sym,
            ast,
            prop_time,
            recursion_kind[u],
            dominant_callee[u],
            transitive_io[u],
            symbols,
            &repo_helpers_by_module,
        );

        let has_io_in_loop = !ast.io_effects.is_empty() || transitive_io[u].is_some();
        let is_rec = recursion_kind[u] != "none";
        let risk_score = (0.45 * prop_time.risk_weight()
            + 0.30 * if has_io_in_loop { 1.0 } else { 0.0 }
            + 0.15 * (f64::from(ast.cognitive) / 25.0).min(1.0)
            + 0.10 * if is_rec { 1.0 } else { 0.0 })
        .clamp(0.0, 1.0);

        let mut io_labels: Vec<String> = ast
            .io_effects
            .iter()
            .map(|(k, line, d)| format!("{} (line {line}, loop depth {d})", k.label()))
            .collect();
        if let Some((io_k, target_idx, line)) = transitive_io[u] {
            if target_idx != u {
                io_labels.push(format!(
                    "transitive_{} via `{}` ({}:{})",
                    io_k.label(),
                    symbols[target_idx].qualified_name,
                    symbols[target_idx].file_path,
                    line
                ));
            }
        }

        reports.insert(
            sym.id.clone(),
            SymbolComplexityReport {
                symbol_id: sym.id.clone(),
                qualified_name: sym.qualified_name.clone(),
                kind: serde_json::to_value(sym.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "function".to_string()),
                file_path: sym.file_path.clone(),
                line_start: sym.line_start,
                line_end: sym.line_end,
                local_time: ast.local_time.notation(),
                propagated_time: prop_time.notation(),
                space_complexity: prop_space.notation(),
                effective_degree: prop_time.effective_degree(),
                risk_score,
                max_loop_depth: ast.max_loop_depth,
                cyclomatic: ast.cyclomatic,
                cognitive: ast.cognitive,
                recursion_kind: recursion_kind[u].to_string(),
                io_in_loop: io_labels,
                bottleneck_chain: chain,
                advisories,
            },
        );
    }

    (reports, edge_loop_depths)
}

fn symbol_has_direct_io(sym: &Symbol) -> bool {
    let Some(body) = sym.body_excerpt.as_deref() else {
        return false;
    };
    let lower = body.to_ascii_lowercase();
    lower.contains("select ")
        || lower.contains("insert into")
        || lower.contains("db.query")
        || lower.contains("db.execute")
        || lower.contains("fetch(")
        || lower.contains("requests.")
}

#[allow(clippy::too_many_arguments)]
fn synthesize_advisories(
    u: usize,
    sym: &Symbol,
    ast: &AstComplexitySummary,
    prop_time: ComplexityClass,
    rec_kind: &str,
    dom_callee: Option<(usize, u8, u32)>,
    trans_io: Option<(IoEffectKind, usize, u32)>,
    all_symbols: &[Symbol],
    repo_helpers_by_module: &HashMap<String, Vec<&Symbol>>,
) -> Vec<OptimizationAdvisory> {
    let mut out = Vec::new();

    // Rule OPT-001a: Direct linear scan inside loop.
    for pat in &ast.intrinsic_patterns {
        match pat {
            IntrinsicPattern::LinearScanInLoop {
                receiver,
                method,
                line,
                loop_depth,
            } => {
                let target_col = receiver.as_deref().unwrap_or("collection");
                let asset = find_repo_indexed_helper(&sym.module, repo_helpers_by_module);
                out.push(OptimizationAdvisory {
                    rule_id: "OPT-001".to_string(),
                    title: "Linear Collection Scan Inside Loop".to_string(),
                    severity: if *loop_depth >= 2 { "critical" } else { "high" }.to_string(),
                    symbol_id: sym.id.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    file_path: sym.file_path.clone(),
                    hotspot_line: *line,
                    current_complexity: prop_time.notation(),
                    target_complexity: "O(n)".to_string(),
                    problem: format!(
                        "Calling `{target_col}.{method}(...)` (O(m) linear scan) inside loop (depth {loop_depth}) at line {line} causes quadratic O(n*m) scaling."
                    ),
                    prescription: format!(
                        "Pre-index `{target_col}` into a HashMap/HashSet (`Map`/`Set` in TS/JS, `dict`/`set` in Python, `HashMap` in Rust/Java) once before the loop in O(m) time, then perform O(1) lookups inside the loop."
                    ),
                    repo_asset_ref: asset,
                    suggested_diff: None,
                });
            }
            IntrinsicPattern::SortInLoop {
                method,
                line,
                loop_depth,
            } => {
                out.push(OptimizationAdvisory {
                    rule_id: "OPT-005".to_string(),
                    title: "Redundant Sorting Inside Loop".to_string(),
                    severity: "high".to_string(),
                    symbol_id: sym.id.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    file_path: sym.file_path.clone(),
                    hotspot_line: *line,
                    current_complexity: prop_time.notation(),
                    target_complexity: "O(n log n)".to_string(),
                    problem: format!(
                        "Sorting via `{method}` (O(n log n)) inside loop (depth {loop_depth}) at line {line} degrades runtime to O(n^2 log n)."
                    ),
                    prescription: "Hoist `.sort()` outside the loop (Loop-Invariant Code Motion) so it runs once in O(n log n), or maintain order incrementally with a BinaryHeap / BTreeSet (O(log n) per insertion).".to_string(),
                    repo_asset_ref: None,
                    suggested_diff: None,
                });
            }
            IntrinsicPattern::StringConcatInLoop {
                target,
                line,
                loop_depth,
            } => {
                out.push(OptimizationAdvisory {
                    rule_id: "OPT-004".to_string(),
                    title: "Quadratic String/Array Concatenation in Loop".to_string(),
                    severity: "medium".to_string(),
                    symbol_id: sym.id.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    file_path: sym.file_path.clone(),
                    hotspot_line: *line,
                    current_complexity: prop_time.notation(),
                    target_complexity: "O(n)".to_string(),
                    problem: format!(
                        "Repeatedly concatenating `{target} += ...` inside loop (depth {loop_depth}) at line {line} reallocates and copies buffers on every iteration."
                    ),
                    prescription: "Accumulate chunks in a pre-allocated buffer (`String::with_capacity` in Rust, `''.join(parts)` in Python, `strings.Builder` in Go, `StringBuilder` in Java/C#, `parts.push(); parts.join('')` in TS/JS).".to_string(),
                    repo_asset_ref: None,
                    suggested_diff: None,
                });
            }
            IntrinsicPattern::CloneInLoop {
                method,
                line,
                loop_depth,
            } => {
                out.push(OptimizationAdvisory {
                    rule_id: "OPT-005".to_string(),
                    title: "Heap Deep-Clone Inside Hot Loop".to_string(),
                    severity: "medium".to_string(),
                    symbol_id: sym.id.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    file_path: sym.file_path.clone(),
                    hotspot_line: *line,
                    current_complexity: prop_time.notation(),
                    target_complexity: "O(n) time, O(1) allocs".to_string(),
                    problem: format!(
                        "Calling `.{method}()` inside loop (depth {loop_depth}) at line {line} causes O(n) heap allocations."
                    ),
                    prescription: "Borrow by reference (`&T` / slice view) or reuse a single pre-allocated scratch buffer across iterations via `.clear()`.".to_string(),
                    repo_asset_ref: None,
                    suggested_diff: None,
                });
            }
            IntrinsicPattern::ChainedAllocation { chain, depth, line } => {
                out.push(OptimizationAdvisory {
                    rule_id: "OPT-007".to_string(),
                    title: "Multi-Pass Eager Array Allocation Chain".to_string(),
                    severity: "low".to_string(),
                    symbol_id: sym.id.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    file_path: sym.file_path.clone(),
                    hotspot_line: *line,
                    current_complexity: format!("{depth}x O(n) space"),
                    target_complexity: "O(1) intermediate space".to_string(),
                    problem: format!(
                        "Chained collection pipeline `{chain}` at line {line} materializes {depth} intermediate arrays."
                    ),
                    prescription: "Fuse into a single-pass `for..of` / `.reduce()` loop or use a lazy iterator/generator pipeline.".to_string(),
                    repo_asset_ref: None,
                    suggested_diff: None,
                });
            }
        }
    }

    // Rule OPT-001b: Cross-function hidden quadratic loop (`u` loops and calls `v` which is >= O(n)).
    if let Some((callee_idx, call_loop_depth, call_line)) = dom_callee {
        if call_loop_depth > 0 && prop_time.effective_degree() >= 2.0 {
            let callee = &all_symbols[callee_idx];
            let asset = find_repo_indexed_helper(&callee.module, repo_helpers_by_module)
                .or_else(|| find_repo_indexed_helper(&sym.module, repo_helpers_by_module));
            out.push(OptimizationAdvisory {
                rule_id: "OPT-001".to_string(),
                title: "Cross-Function Hidden Quadratic Bottleneck".to_string(),
                severity: if prop_time.effective_degree() >= 3.0 {
                    "critical"
                } else {
                    "high"
                }
                .to_string(),
                symbol_id: sym.id.clone(),
                qualified_name: sym.qualified_name.clone(),
                file_path: sym.file_path.clone(),
                hotspot_line: call_line,
                current_complexity: prop_time.notation(),
                target_complexity: "O(n)".to_string(),
                problem: format!(
                    "`{}` invokes `{}` ({}:{}) inside a loop (depth {call_loop_depth} at line {call_line}), multiplying callee's linear work into {}.",
                    sym.qualified_name,
                    callee.qualified_name,
                    callee.file_path,
                    callee.line_start,
                    prop_time.notation()
                ),
                prescription: format!(
                    "Hoist or batch the call to `{}` outside the loop, or pre-index the lookup table in `{}` into a HashMap/Set so per-iteration lookup is O(1).",
                    callee.name, callee.module
                ),
                repo_asset_ref: asset,
                suggested_diff: None,
            });
        }
    }

    // Rule OPT-002 & OPT-003: Direct or Transitive I/O or Sequential Await in Loop.
    for &(io_kind, line, depth) in &ast.io_effects {
        match io_kind {
            IoEffectKind::DatabaseQuery | IoEffectKind::NetworkRpc => {
                let asset = find_repo_batch_helper(&sym.module, repo_helpers_by_module);
                out.push(OptimizationAdvisory {
                    rule_id: "OPT-002".to_string(),
                    title: "N+1 Query / Network Call Inside Loop".to_string(),
                    severity: "critical".to_string(),
                    symbol_id: sym.id.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    file_path: sym.file_path.clone(),
                    hotspot_line: line,
                    current_complexity: format!("O(n) I/O round-trips (loop depth {depth})"),
                    target_complexity: "O(1) batched I/O round-trip".to_string(),
                    problem: format!(
                        "Executing `{}` inside loop at line {line} triggers N+1 sequential I/O operations.",
                        io_kind.label()
                    ),
                    prescription: "Collect target IDs/keys inside the loop and execute a single bulk query (`WHERE id IN (...)`, `findMany`, or batch RPC) before or after the loop.".to_string(),
                    repo_asset_ref: asset,
                    suggested_diff: None,
                });
            }
            IoEffectKind::SequentialAwaitInLoop => {
                out.push(OptimizationAdvisory {
                    rule_id: "OPT-003".to_string(),
                    title: "Sequential `await` Inside Loop".to_string(),
                    severity: "high".to_string(),
                    symbol_id: sym.id.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    file_path: sym.file_path.clone(),
                    hotspot_line: line,
                    current_complexity: "O(n * latency) serial wait".to_string(),
                    target_complexity: "O(latency) concurrent execution".to_string(),
                    problem: format!(
                        "Awaiting promises/futures sequentially inside a loop at line {line} serializes independent async tasks."
                    ),
                    prescription: "Collect tasks and await concurrently via `Promise.all(...)`, `asyncio.gather(...)`, `futures::future::join_all(...)`, or `Task.WhenAll(...)`.".to_string(),
                    repo_asset_ref: None,
                    suggested_diff: None,
                });
            }
            IoEffectKind::FilesystemBlocking => {
                out.push(OptimizationAdvisory {
                    rule_id: "OPT-002".to_string(),
                    title: "Blocking Filesystem I/O Inside Loop".to_string(),
                    severity: "high".to_string(),
                    symbol_id: sym.id.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    file_path: sym.file_path.clone(),
                    hotspot_line: line,
                    current_complexity: "O(n) sync disk syscalls".to_string(),
                    target_complexity: "O(1) buffered/cached read".to_string(),
                    problem: format!(
                        "Synchronous filesystem call inside loop at line {line} blocks the thread per iteration."
                    ),
                    prescription: "Hoist file reads outside the loop or use buffered streaming / in-memory caching.".to_string(),
                    repo_asset_ref: None,
                    suggested_diff: None,
                });
            }
        }
    }

    if let Some((io_kind, target_idx, line)) = trans_io {
        if target_idx != u {
            let callee = &all_symbols[target_idx];
            let asset = find_repo_batch_helper(&callee.module, repo_helpers_by_module);
            out.push(OptimizationAdvisory {
                rule_id: "OPT-002".to_string(),
                title: "Transitive N+1 I/O Across Function Boundary".to_string(),
                severity: "critical".to_string(),
                symbol_id: sym.id.clone(),
                qualified_name: sym.qualified_name.clone(),
                file_path: sym.file_path.clone(),
                hotspot_line: sym.line_start,
                current_complexity: "O(n) transitive I/O calls".to_string(),
                target_complexity: "O(1) batched I/O".to_string(),
                problem: format!(
                    "`{}` calls `{}` inside a loop, which executes `{}` at {}:{line}.",
                    sym.qualified_name,
                    callee.qualified_name,
                    io_kind.label(),
                    callee.file_path
                ),
                prescription: format!(
                    "Refactor `{}` to accept a slice/batch of keys and return a `HashMap<Key, Value>` in a single I/O round-trip.",
                    callee.qualified_name
                ),
                repo_asset_ref: asset,
                suggested_diff: None,
            });
        }
    }

    // Rule OPT-006: Unmemoized Exponential Branching Recursion.
    if rec_kind != "none" && matches!(prop_time, ComplexityClass::Exponential { .. }) {
        out.push(OptimizationAdvisory {
            rule_id: "OPT-006".to_string(),
            title: "Unmemoized Exponential Branching Recursion".to_string(),
            severity: "critical".to_string(),
            symbol_id: sym.id.clone(),
            qualified_name: sym.qualified_name.clone(),
            file_path: sym.file_path.clone(),
            hotspot_line: sym.line_start,
            current_complexity: prop_time.notation(),
            target_complexity: "O(n) with Memoization / DP".to_string(),
            problem: format!(
                "`{}` performs branching recursion ({rec_kind}) with decrementing arguments and no detected memoization cache, resulting in {} exponential blowup.",
                sym.qualified_name,
                prop_time.notation()
            ),
            prescription: "Add memoization (`@functools.lru_cache` in Python, a `HashMap` / `Map` cache keyed by parameters, or a bottom-up Dynamic Programming table) to eliminate overlapping subproblems.".to_string(),
            repo_asset_ref: None,
            suggested_diff: None,
        });
    }

    out
}

fn find_repo_indexed_helper(
    module: &str,
    repo_helpers_by_module: &HashMap<String, Vec<&Symbol>>,
) -> Option<String> {
    let candidates = repo_helpers_by_module.get(module)?;
    for sym in candidates {
        let lower = sym.name.to_ascii_lowercase();
        if lower.contains("index")
            || lower.contains("map")
            || lower.contains("by_id")
            || lower.contains("lookup")
            || lower.contains("cache")
            || lower.contains("hydrate")
        {
            return Some(format!(
                "{} ({}:{})",
                sym.qualified_name, sym.file_path, sym.line_start
            ));
        }
    }
    None
}

fn find_repo_batch_helper(
    module: &str,
    repo_helpers_by_module: &HashMap<String, Vec<&Symbol>>,
) -> Option<String> {
    let candidates = repo_helpers_by_module.get(module)?;
    for sym in candidates {
        let lower = sym.name.to_ascii_lowercase();
        if lower.contains("batch")
            || lower.contains("bulk")
            || lower.contains("many")
            || lower.contains("hydrate_ids")
            || lower.contains("all")
        {
            return Some(format!(
                "{} ({}:{})",
                sym.qualified_name, sym.file_path, sym.line_start
            ));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tarjan's Strongly Connected Components (O(V + E))
// ---------------------------------------------------------------------------

type CallEdgeMeta = (usize, u8, u16, u32, ArgReduction);

fn tarjan_scc(n: usize, adj: &[Vec<CallEdgeMeta>]) -> Vec<Vec<usize>> {
    struct State {
        index: usize,
        indices: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }

    fn strongconnect(u: usize, adj: &[Vec<CallEdgeMeta>], st: &mut State) {
        st.indices[u] = Some(st.index);
        st.lowlink[u] = st.index;
        st.index += 1;
        st.stack.push(u);
        st.on_stack[u] = true;

        for &(v, ..) in &adj[u] {
            if st.indices[v].is_none() {
                strongconnect(v, adj, st);
                st.lowlink[u] = st.lowlink[u].min(st.lowlink[v]);
            } else if st.on_stack[v] {
                st.lowlink[u] = st.lowlink[u].min(st.indices[v].unwrap());
            }
        }

        if st.lowlink[u] == st.indices[u].unwrap() {
            let mut scc = Vec::new();
            while let Some(w) = st.stack.pop() {
                st.on_stack[w] = false;
                scc.push(w);
                if w == u {
                    break;
                }
            }
            st.sccs.push(scc);
        }
    }

    let mut state = State {
        index: 0,
        indices: vec![None; n],
        lowlink: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        sccs: Vec::new(),
    };

    for u in 0..n {
        if state.indices[u].is_none() {
            strongconnect(u, adj, &mut state);
        }
    }
    // Tarjan naturally emits SCCs in reverse topological order (sinks/callees first).
    state.sccs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_source;
    use crate::resolver::{resolve_edges, to_edges};

    #[test]
    fn detects_interprocedural_quadratic_loop_and_prescribes_opt_001() {
        let src = r#"
function findUser(userId: string, users: any[]) {
    return users.find(u => u.id === userId);
}

function lookupTax(order: any, users: any[]) {
    return findUser(order.userId, users);
}

function processOrders(orders: any[], users: any[]) {
    const out = [];
    for (const order of orders) {
        out.push(lookupTax(order, users));
    }
    return out;
}
"#;
        let parsed = parse_source("src/orders.ts", src);
        let edges = to_edges(&resolve_edges(&parsed.symbols));
        let (reports, _) = analyze_workspace_complexity(&parsed.symbols, &edges, &HashMap::new());

        let proc_sym = parsed
            .symbols
            .iter()
            .find(|s| s.name == "processOrders")
            .unwrap();
        let report = &reports[&proc_sym.id];
        assert_eq!(report.local_time, "O(n)");
        assert_eq!(
            report.propagated_time, "O(n^2)",
            "expected inter-procedural propagation to catch O(n^2) through lookupTax -> findUser"
        );
        assert_eq!(report.bottleneck_chain.len(), 3);
        assert!(report.advisories.iter().any(|a| a.rule_id == "OPT-001"));
    }

    #[test]
    fn detects_unmemoized_exponential_recursion_opt_006() {
        let src = r#"
def fib(n):
    if n <= 1:
        return n
    return fib(n - 1) + fib(n - 2)
"#;
        let parsed = parse_source("src/math.py", src);
        let edges = to_edges(&resolve_edges(&parsed.symbols));
        let (reports, _) = analyze_workspace_complexity(&parsed.symbols, &edges, &HashMap::new());

        let fib_sym = parsed.symbols.iter().find(|s| s.name == "fib").unwrap();
        let report = &reports[&fib_sym.id];
        assert_eq!(report.propagated_time, "O(2^n)");
        assert_eq!(report.recursion_kind, "direct_recursion");
        assert!(report.advisories.iter().any(|a| a.rule_id == "OPT-006"));
    }
}
