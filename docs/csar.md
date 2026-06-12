# CSAR — Code Spreading-Activation Retrieval

> A mathematically grounded retrieval operator that unifies cognis's semantic
> and structural layers, recovers full code flow, and has a *repo-size-independent*
> cost bound.

## 1. Motivation

Today's AI IDE retrieval (Cursor, Cody) and cognis's own mesh rank symbols with
**embedding KNN + BM25**, then merge with **Reciprocal-Rank Fusion (RRF)**.
Two structural weaknesses:

1. **Independence assumption.** Embedding/lexical scores treat each symbol on its
   own. A symbol that is *not* itself a good lexical/semantic match — but sits on
   the call path between two matched symbols — is invisible. This is exactly the
   "missing the full flow of code" failure.
2. **RRF is a heuristic.** `score = Σ 1/(k + rank)` has no model of the codebase,
   no recall guarantee, and no cost bound. To improve recall you must widen `k`
   (more grep / more embeddings), which is precisely the cost we want to avoid.

cognis already stores the **Unified Code Knowledge Graph (UCKG)**: the `edge`
table (`calls`, `imports`, `inherits`, …). CSAR exploits it.

## 2. The method

Let the codebase be a graph `G = (V, E)`, `n = |V|` symbols. From the cheap
existing signals (FTS5 + a *small* semantic top-k) we build a **seed
distribution** `s ∈ ℝⁿ`, `s ≥ 0`, `‖s‖₁ = 1`, where `sᵢ` is the normalized
relevance of symbol `i` to the query.

We diffuse `s` over `G` with a **random walk with restart** (Personalized
PageRank). With teleport/restart probability `α ∈ (0, 1]` and column-stochastic
transition matrix `P`, the CSAR score vector `r` is the fixed point of

```
r = α·s + (1 − α)·P·r                                          (PPR equation)
```

Equivalently, in closed form,

```
r = α·(I − (1 − α)·P)⁻¹·s.                                     (closed form)
```

`r` is the **unified score**: mass enters at the seeds (via the `α·s` teleport)
and spreads along code edges (via `(1−α)·P·r`). A symbol becomes important if it
is *directly* relevant (high `sᵢ`) **or** structurally close to the relevant
region. `α` is a single, interpretable knob:

- `α → 1` ⇒ `r → s` (pure semantic/lexical retrieval, today's behavior),
- `α → 0` ⇒ `r →` stationary walk distribution (pure structural/PageRank).

So CSAR is a **provable interpolation** between cognis's semantic and structural
layers — one operator instead of two layers glued by a heuristic.

### Graph construction

- Nodes: all indexed symbols.
- Edges: UCKG `edge` rows with `meta.dst_missing != 1` (same filter as the
  structural layer). Each edge weighted by its `confidence`.
- The graph is **symmetrized** (`A = A_dir + A_dirᵀ`) so diffusion reaches both
  callers and callees of a seed — recovering full flow in both directions.
- Isolated nodes get a **self-loop** so `P` stays column-stochastic and mass is
  conserved exactly (see Theorem 3).
- `P = A·D⁻¹` with `D = diag(degree)`; column `j` sums to 1.

## 3. Mathematical guarantees

Throughout, `P` is column-stochastic (`𝟙ᵀP = 𝟙ᵀ`), `α ∈ (0, 1]`,
`s` a probability vector.

### Theorem 1 — Existence & uniqueness
`M = I − (1 − α)P` is nonsingular, so the PPR equation has the unique solution
`r = α·M⁻¹·s`.

*Proof.* `P` is column-stochastic, hence its spectral radius `ρ(P) ≤ 1` (a
stochastic matrix has `ρ = 1`). Then `ρ((1−α)P) = (1−α)ρ(P) ≤ 1 − α < 1` for
`α > 0`. A matrix `I − B` with `ρ(B) < 1` is invertible and
`(I − B)⁻¹ = Σ_{t≥0} Bᵗ` (Neumann series), which converges absolutely. ∎

### Theorem 2 — Geometric convergence of power iteration
The iteration `r_{t+1} = α·s + (1−α)·P·r_t` satisfies
`‖r_t − r*‖ ≤ (1−α)ᵗ · ‖r₀ − r*‖`. So it converges to `r*` for any `r₀`, and
reaching error `δ` needs `t ≥ ln(1/δ)/ln(1/(1−α))` iterations — independent of `n`.

*Proof.* Subtract the fixed point: `r_{t+1} − r* = (1−α)P(r_t − r*)`. Taking norms
and using `‖(1−α)P‖ ≤ (1−α)` (operator norm of a sub-stochastic matrix) gives the
contraction. ∎

### Theorem 3 — Mass conservation
If `‖s‖₁ = 1` then `‖r*‖₁ = 1`.

*Proof.* `𝟙ᵀr* = α·𝟙ᵀs + (1−α)·𝟙ᵀP·r* = α + (1−α)·𝟙ᵀr*` because `𝟙ᵀP = 𝟙ᵀ`.
Solving, `𝟙ᵀr*·(1 − (1−α)) = α`, i.e. `𝟙ᵀr* = 1`. The general identity
`𝟙ᵀ·ppr(x) = 𝟙ᵀx` follows the same way and is used in Theorem 5. ∎

### Theorem 4 — Endpoint limits (semantic ⇄ structural)
`lim_{α→1} r* = s`, and (when the walk is ergodic) `lim_{α→0⁺} r* = π`, the
stationary distribution `Pπ = π`.

*Proof.* At `α = 1`, the closed form gives `r* = 1·(I − 0)⁻¹s = s`. As `α → 0⁺`,
`r*` solves `r = (1−α)Pr + α s`; the `α s` term vanishes and `r*` approaches the
fixed point of `r = Pr`, which is the stationary distribution `π`. ∎

This is the scientific core: **one operator with one parameter provably sweeps
the entire space between the two existing cognis layers.**

### Theorem 5 — Forward-push correctness (the cost-saving result)
Run Andersen–Chung–Lang forward push: keep an estimate `p` and residual `r`,
start `p = 0`, `r = s`; repeatedly pick a node `u` with `r_u ≥ ε·d_u` and *push*

```
p_u  += α·r_u
r_j  += (1−α)·r_u·A_{ju}/d_u   for every neighbor j of u
r_u   = 0.
```

Let `ppr(x) := α(I − (1−α)P)⁻¹x` be the exact PPR operator. Then:

**(5a) Invariant.** `ppr(s) = p + ppr(r)` holds after every push.

**(5b) Approximation bound.** At termination (`r_u < ε·d_u` ∀u),
`‖ppr(s) − p‖₁ = ‖ppr(r)‖₁ = ‖r‖₁`.

**(5c) Locality / cost bound.** The total work is
`Σ_pushes d_u ≤ 1/(α·ε)`, **independent of `n = |V|`**.

*Proof.*
*(5a)* Initially `p = 0`, `r = s`, true. A push at `u` sets
`p' = p + α r_u e_u` and `r' = r − r_u e_u + (1−α)r_u P e_u
     = r − r_u(I − (1−α)P)e_u`. By linearity of `ppr` and
`ppr((I−(1−α)P)e_u) = α e_u`,
`ppr(r) − ppr(r') = r_u·ppr((I−(1−α)P)e_u) = α r_u e_u = p' − p`. Hence
`p' + ppr(r') = p + ppr(r) = ppr(s)`.
*(5b)* From (5a), `ppr(s) − p = ppr(r)`; take `‖·‖₁` and use `‖ppr(x)‖₁ = ‖x‖₁`
(Theorem 3 identity). 
*(5c)* Each push adds `α·r_u` to `p` with `r_u ≥ ε·d_u`, so it adds `≥ α ε d_u`.
Total `p`-mass never exceeds `‖ppr(s)‖₁ = 1`. Summing over pushes,
`Σ α ε d_u ≤ 1`, i.e. `Σ d_u ≤ 1/(α ε)`. ∎

**Consequence.** CSAR's retrieval cost depends only on `α` and `ε`, **not on the
size of the repository**. Doubling the codebase does not increase per-query work.
This is the formal version of "saves greping cost": instead of widening `k`
(more embeddings / more lexical scans) to improve recall, CSAR expands recall by
a *local* graph diffusion whose cost is capped a priori.

## 4. What CSAR adds over RRF — and what the objective benchmark found

CSAR contributes structural properties that pure rank fusion (RRF) does not:

| Property | RRF | CSAR |
| --- | --- | --- |
| Uses code structure | no | yes (UCKG edges) |
| Recovers on-path symbols | no | yes (diffusion) |
| Theoretical recall/cost model | none | Theorems 1–5 |
| Cost vs. repo size | grows with `k` | `O(1/(αε))`, size-independent |
| Tunable semantic⇄structural | no | yes, single `α` (Theorem 4) |
| Reuses existing cheap signals | n/a | yes (seeds from FTS5 + small semantic top-k) |

> **Honest verdict (evidence-backed, do not overclaim).** On objective,
> bug-fix-derived ground truth (276 queries, 5 public repos, Python + Java), raw
> PPR diffusion is **not** a competitive *ranker*: it floods high-degree hubs
> (up to ~48% contamination) and posts the worst MRR, and every degree-corrected
> or query-conditional structural variant tried fails to beat RRF (see
> `.benchmarks/public/RESULTS.md`). The table above is therefore a list of
> structural *mechanism* properties, **not** a quality-ranking claim. The
> production engine ranks with **RRF fusion** of BM25 + dense; CSAR's value is the
> PROVEN, never-displacing, lowest-contamination **on-path context** it adds on
> top — and its size-independent cost — not primacy as the ranker.

## 5. Implementation & verification

- Algorithm + retrieval layer: `packages/retrieval/cognis_retrieval/csar.py`
  (`build_code_graph`, `personalized_pagerank_exact`,
  `personalized_pagerank_power`, `approximate_ppr_push`, `CSARLayer`).
- Unit proofs-in-code: `tests/unit/test_csar.py` checks Theorems 1–5 numerically
  on concrete graphs, plus retrieval behavior (on-path recall vs. KNN-only).
- Property-based proofs: `tests/pbt/test_csar_pbt.py` (hypothesis) checks the
  invariants on *random* graphs:
  - CP-CSAR-1: mass conservation (Thm 3),
  - CP-CSAR-2: power iteration ↔ closed form agreement + geometric bound (Thm 2),
  - CP-CSAR-3: push invariant `ppr(s) = p + ppr(r)` and residual termination (Thm 5a/5b),
  - CP-CSAR-4: push work bound `Σ d_u ≤ 1/(αε)` (Thm 5c),
  - CP-CSAR-5: `α → 1` ⇒ `r ≈ s` (Thm 4).

## 6. MCP integration

CSAR is exposed as the **on-path context mechanism** in cognis (the cross-layer
*ranking* is RRF fusion — see §4):

- **`diffuse_context(query, k, alpha, eps, ...)`** — the flagship MCP tool.
  Seeds from lexical (`_fts_search_core`) + semantic (`_semantic_search_core`)
  legs, diffuses with `diffuse_seed_hits`, and returns a unified ranked
  shortlist in one round trip. Each hit carries an `on_path` flag marking
  symbols reached via code flow rather than direct match. This replaces the
  separate `discover_symbols` + `dependency_trace` calls an agent made before.
- **`retrieve_context_capsule`** — its structural stage is CSAR-powered:
  lexical + semantic hits seed a diffusion whose newly reached (on-path)
  symbols feed the bug/root-cause sections of the capsule, instead of the old
  single-hop BFS.

`CSARLayer` also implements the standard `RetrievalLayer` protocol
(`search(query, k, db) -> list[Hit]`) for direct programmatic use, seeding from
`LexicalLayer` + `SemanticLayer`. Tunables are exposed via environment:
`COGNIS_MCP_CSAR_ALPHA` (default `0.15`), `COGNIS_MCP_CSAR_EPS` (default
`1e-5`), and `COGNIS_MCP_CSAR_SEED_K` (default `25`).
```
