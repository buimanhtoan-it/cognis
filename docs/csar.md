# CSAR & CSIA — Graph Diffusion Mathematics in Cognis

Cognis pairs two complementary Personalized PageRank (PPR) operators over the **Unified Code Knowledge Graph (UCKG)** in `crates/cognis-csar`:

1. **CSAR (Code Spreading-Activation Retrieval)** — **forward-push** diffusion over the symmetrized graph to recover *on-path call-flow context* with repository-size-independent work $O(1/(\alpha\varepsilon))$.
2. **CSIA (Code Spreading Impact Analysis)** — **loop-amplified reverse-push** diffusion over reversed dependency edges to quantify *upstream blast radius, hot-loop amplification, entrypoint exposure, and minimal test coverage*.

---

## Part I — CSAR (Code Spreading-Activation Retrieval)

### 1. Motivation

Standard lexical (BM25 / FTS5) and dense vector (KNN) retrieval score each symbol independently:

- A bridge function sitting on the call path *between* two query-matching symbols often shares zero tokens with the user's prompt and is missed by top-$k$ independent search.
- Widening $k$ linearly increases token noise and embedding work.

CSAR exploits the UCKG `edge` table (`calls`, `imports`, `inherits`) to diffuse relevance locally from high-precision lexical + semantic seeds.

### 2. Formulation

Let $G = (V, E)$ with $n = |V|$ indexed symbols. From FTS5 + semantic top-$k$ hits we form a normalized seed distribution $s \in \mathbb{R}^n$ ($s \ge 0$, $\|s\|_1 = 1$). With restart probability $\alpha \in (0, 1]$ and column-stochastic transition matrix $P = A D^{-1}$ (symmetrized $A = A_{\text{dir}} + A_{\text{dir}}^\top$, isolated nodes given unit self-loops), the CSAR score vector $r^*$ satisfies:

$$
r^* = \alpha s + (1 - \alpha) P r^* = \alpha (I - (1 - \alpha) P)^{-1} s
$$

- As $\alpha \to 1$, $r^* \to s$ (pure lexical/semantic seed ranking).
- As $\alpha \to 0^+$, $r^*$ approaches the stationary structural walk distribution.

### 3. Verified Mathematical Guarantees

All five theorems below are machine-checked on random graphs via `proptest` in `crates/cognis-csar`:

1. **Theorem 1 (Existence & Uniqueness):** Since $P$ is column-stochastic ($\rho(P) \le 1$) and $\alpha > 0$, $\rho((1-\alpha)P) \le 1 - \alpha < 1$, so $M = I - (1-\alpha)P$ is nonsingular with convergent Neumann series $\sum_{t \ge 0} ((1-\alpha)P)^t$.
2. **Theorem 2 (Geometric Convergence):** Power iteration $r_{t+1} = \alpha s + (1-\alpha) P r_t$ contracts at rate $\|r_t - r^*\|_1 \le (1-\alpha)^t \|r_0 - r^*\|_1$, independent of $n$.
3. **Theorem 3 (Mass Conservation):** $\mathbf{1}^\top P = \mathbf{1}^\top \implies \|r^*\|_1 = \|s\|_1 = 1$.
4. **Theorem 4 (Endpoint Interpolation):** $\lim_{\alpha \to 1} r^* = s$, smoothly interpolating between direct match relevance and graph structure.
5. **Theorem 5 (Andersen–Chung–Lang Forward-Push Locality Bound):** Maintaining estimate $p$ and residual $r$ and pushing whenever $r_u \ge \varepsilon d_u$:
   - **Conservation invariant:** $\text{ppr}(s) = p + \text{ppr}(r)$ after every push.
   - **L1 error bound:** $\|\text{ppr}(s) - p\|_1 = \|r\|_1$ at termination.
   - **Repo-size-independent work bound:** Total neighbor work satisfies $\sum_{\text{pushes}} d_u \le \frac{1}{\alpha \varepsilon}$, **independent of repository size $|V|$**.

### 4. Production Role Alongside RRF

In `cognis-retrieval`, **Reciprocal Rank Fusion (RRF)** orders primary lexical + semantic candidates (avoiding high-degree hub flooding), while **CSAR** contributes degree-controlled **on-path context symbols** (`on_path: true`) that connect matched symbols across call chains.

---

## Part II — CSIA (Code Spreading Impact Analysis)

### 5. Loop-Amplified Reverse Diffusion

When an AI agent modifies a symbol $u_0$ (for example, degrading its complexity from $O(1)$ to $O(n)$ or altering its parameter signature), impact flows **upstream** from callee $v$ to caller $u$.

Critically, a caller $u$ that invokes $v$ inside $\ell(u \to v)$ nested loops amplifies runtime degradation exponentially in nesting depth. CSIA weights each reversed call edge $(v \to u)$ by:

$$
w_{\text{impact}}(v \to u) = w_{\text{conf}}(u \to v) \cdot \beta^{\ell(u \to v)}, \qquad \beta = 4
$$

where $\ell(u \to v) \in \{0, 1, 2, \dots\}$ is the AST loop nesting depth of the call site recorded by `cognis-indexer`.

Running reverse Personalized PageRank from the edited seed set $S_{\text{edit}}$ yields each upstream symbol's impact mass $I(u)$, from which CSIA extracts:

- **Impacted Callers:** Ranked by $I(u)$ and annotated with propagated Big-O delta and loop-amplification factor $\beta^\ell$.
- **Breaking Call Sites:** Direct callers whose call-site arity/signature conflicts with a changed function signature in the Speculative Shadow AST.
- **Affected Entrypoints:** Exported routes, CLI handlers, and public API roots reached by upstream mass.
- **Minimal Covering Tests:** Test symbols (`#[test]`, `test_*`, `*.test.ts`, `*.spec.ts`) with highest reverse-push reachability to $S_{\text{edit}}$.
- **Composite Risk Score $\in [0, 1]$ (`Low` / `Medium` / `High` / `Critical`):** Combines complexity escalation severity, normalized upstream PPR mass, entrypoint exposure, and signature breakage.

---

## 6. Rust Implementation Map

| Component | Location |
| --- | --- |
| Complexity semiring lattice & guardrail types | `crates/cognis-core/src/complexity.rs` |
| Resident CSR `CodeGraph` | `crates/cognis-core/src/graph.rs`, `crates/cognis-store/src/graph_loader.rs` |
| Forward-push PPR kernel (`approximate_ppr_push`) | `crates/cognis-csar/src/push.rs` |
| Exact & power-iteration reference solvers | `crates/cognis-csar/src/solvers.rs` |
| CSAR seed diffusion (`diffuse_seed_hits`) | `crates/cognis-csar/src/diffuse.rs` |
| CSIA loop-amplified reverse impact solver | `crates/cognis-csar/src/impact.rs` |
| Static AST complexity + `OPT-001..007` + Shadow AST | `crates/cognis-indexer/src/complexity.rs`, `crates/cognis-indexer/src/shadow.rs` |
| MCP guardrail & diffusion tool handlers | `crates/cognis-mcp/src/tools.rs`, `crates/cognis-mcp/src/store_engine.rs` |
