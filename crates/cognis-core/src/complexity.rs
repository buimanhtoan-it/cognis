//! Algorithmic complexity algebra, AST/inter-procedural summaries, optimization
//! advisories, and multi-dimensional change impact models.
//!
//! Provides the dependency-neutral types shared by `cognis-indexer` (AST +
//! call-graph complexity analysis), `cognis-csar` (CSIA reverse forward-push
//! impact analysis), and `cognis-mcp` (real-time agent guardrail tools).

use serde::{Deserialize, Serialize};

/// Canonical asymptotic growth classes ordered by dominance lattice:
/// `O(1) < O(log n) < O(sqrt(n)) < O(n) < O(n log n) < O(n^2) < O(n^3) < O(2^n) < O(n!)`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComplexityClass {
    /// `O(1)` constant time/space.
    #[default]
    Constant,
    /// `O((log n)^power)` logarithmic growth.
    Logarithmic { power: u8 },
    /// `O(sqrt(n))` sub-linear growth.
    Sqrt,
    /// `O(n^degree * (log n)^log_power)` polynomial / log-linear growth.
    Polynomial { degree: u8, log_power: u8 },
    /// `O(base_calls^n)` exponential branching recursion.
    Exponential { base_calls: u8 },
    /// `O(n!)` permutation / factorial recursion.
    Factorial,
}

impl ComplexityClass {
    pub const fn constant() -> Self {
        Self::Constant
    }

    pub const fn log_n() -> Self {
        Self::Logarithmic { power: 1 }
    }

    pub const fn sqrt_n() -> Self {
        Self::Sqrt
    }

    pub const fn linear() -> Self {
        Self::Polynomial {
            degree: 1,
            log_power: 0,
        }
    }

    pub const fn n_log_n() -> Self {
        Self::Polynomial {
            degree: 1,
            log_power: 1,
        }
    }

    pub const fn quadratic() -> Self {
        Self::Polynomial {
            degree: 2,
            log_power: 0,
        }
    }

    pub const fn cubic() -> Self {
        Self::Polynomial {
            degree: 3,
            log_power: 0,
        }
    }

    pub const fn polynomial(degree: u8) -> Self {
        if degree == 0 {
            Self::Constant
        } else {
            Self::Polynomial {
                degree,
                log_power: 0,
            }
        }
    }

    /// Human-readable Big-O string (e.g. `"O(1)"`, `"O(n)"`, `"O(n log n)"`, `"O(n^2)"`).
    pub fn notation(&self) -> String {
        match *self {
            Self::Constant => "O(1)".to_string(),
            Self::Logarithmic { power: 0 } => "O(1)".to_string(),
            Self::Logarithmic { power: 1 } => "O(log n)".to_string(),
            Self::Logarithmic { power } => format!("O((log n)^{power})"),
            Self::Sqrt => "O(sqrt(n))".to_string(),
            Self::Polynomial {
                degree: 0,
                log_power: 0,
            } => "O(1)".to_string(),
            Self::Polynomial {
                degree: 0,
                log_power,
            } => Self::Logarithmic { power: log_power }.notation(),
            Self::Polynomial {
                degree: 1,
                log_power: 0,
            } => "O(n)".to_string(),
            Self::Polynomial {
                degree: 1,
                log_power: 1,
            } => "O(n log n)".to_string(),
            Self::Polynomial {
                degree: 1,
                log_power,
            } => format!("O(n (log n)^{log_power})"),
            Self::Polynomial {
                degree,
                log_power: 0,
            } => format!("O(n^{degree})"),
            Self::Polynomial {
                degree,
                log_power: 1,
            } => format!("O(n^{degree} log n)"),
            Self::Polynomial { degree, log_power } => {
                format!("O(n^{degree} (log n)^{log_power})")
            }
            Self::Exponential { base_calls } => {
                let b = base_calls.max(2);
                format!("O({b}^n)")
            }
            Self::Factorial => "O(n!)".to_string(),
        }
    }

    /// Scalar effective degree in the Asymptotic Exponent Semiring.
    /// Used for comparing growth classes and computing $\Delta \hat{c}$ amplification.
    pub fn effective_degree(&self) -> f64 {
        match *self {
            Self::Constant => 0.0,
            Self::Logarithmic { power } => 0.25 * (power as f64),
            Self::Sqrt => 0.5,
            Self::Polynomial { degree, log_power } => (degree as f64) + 0.25 * (log_power as f64),
            Self::Exponential { base_calls } => 8.0 + 0.1 * (base_calls.max(2) as f64),
            Self::Factorial => 10.0,
        }
    }

    /// Normalized risk weight in `[0.0, 1.0]`.
    pub fn risk_weight(&self) -> f64 {
        match *self {
            Self::Constant => 0.05,
            Self::Logarithmic { .. } => 0.15,
            Self::Sqrt => 0.20,
            Self::Polynomial {
                degree: 1,
                log_power: 0,
            } => 0.30,
            Self::Polynomial {
                degree: 1,
                log_power: _,
            } => 0.45,
            Self::Polynomial { degree: 2, .. } => 0.75,
            Self::Polynomial { degree: 3, .. } => 0.90,
            Self::Polynomial { .. } => 0.95,
            Self::Exponential { .. } | Self::Factorial => 1.0,
        }
    }

    /// Sequential composition ($\oplus = \max$): `C(S1; S2) = max(C(S1), C(S2))`.
    pub fn seq_max(self, other: Self) -> Self {
        if self.effective_degree() >= other.effective_degree() {
            self
        } else {
            other
        }
    }

    /// Nested / multiplicative composition ($\otimes$): loop body or callee inside loop.
    pub fn nest_mul(self, other: Self) -> Self {
        match (self, other) {
            (Self::Constant, x) | (x, Self::Constant) => x,
            (Self::Factorial, _) | (_, Self::Factorial) => Self::Factorial,
            (Self::Exponential { base_calls: b1 }, Self::Exponential { base_calls: b2 }) => {
                Self::Exponential {
                    base_calls: b1.saturating_mul(b2).max(2),
                }
            }
            (Self::Exponential { base_calls }, _) | (_, Self::Exponential { base_calls }) => {
                Self::Exponential { base_calls }
            }
            (Self::Logarithmic { power: p1 }, Self::Logarithmic { power: p2 }) => {
                Self::Logarithmic {
                    power: p1.saturating_add(p2),
                }
            }
            (Self::Sqrt, Self::Sqrt) => Self::linear(),
            (Self::Sqrt, Self::Logarithmic { power })
            | (Self::Logarithmic { power }, Self::Sqrt) => Self::Polynomial {
                degree: 1,
                log_power: power,
            },
            (Self::Sqrt, Self::Polynomial { degree, log_power })
            | (Self::Polynomial { degree, log_power }, Self::Sqrt) => Self::Polynomial {
                degree: degree.saturating_add(1),
                log_power,
            },
            (Self::Polynomial { degree, log_power }, Self::Logarithmic { power })
            | (Self::Logarithmic { power }, Self::Polynomial { degree, log_power }) => {
                Self::Polynomial {
                    degree,
                    log_power: log_power.saturating_add(power),
                }
            }
            (
                Self::Polynomial {
                    degree: d1,
                    log_power: p1,
                },
                Self::Polynomial {
                    degree: d2,
                    log_power: p2,
                },
            ) => Self::Polynomial {
                degree: d1.saturating_add(d2),
                log_power: p1.saturating_add(p2),
            },
        }
    }
}

impl PartialOrd for ComplexityClass {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.effective_degree()
            .partial_cmp(&other.effective_degree())
    }
}

/// Classification of a single loop's iteration bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopBound {
    /// Bounded by a compile-time constant literal (`range(10)`, `i < 32`).
    Constant(u32),
    /// Halving or doubling step (`i *= 2`, `i >>= 1`, `mid = (lo + hi) >> 1`).
    Logarithmic,
    /// Bounded by `i * i <= n` or `sqrt(n)`.
    Sqrt,
    /// Iterates over a collection or variable bound `n`.
    Linear { collection: Option<String> },
}

impl LoopBound {
    pub fn to_complexity(&self) -> ComplexityClass {
        match self {
            Self::Constant(_) => ComplexityClass::Constant,
            Self::Logarithmic => ComplexityClass::log_n(),
            Self::Sqrt => ComplexityClass::sqrt_n(),
            Self::Linear { .. } => ComplexityClass::linear(),
        }
    }
}

/// Blocking I/O, database, or async concurrency anti-patterns inside a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoEffectKind {
    DatabaseQuery,
    NetworkRpc,
    FilesystemBlocking,
    SequentialAwaitInLoop,
}

impl IoEffectKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::DatabaseQuery => "db_query_in_loop",
            Self::NetworkRpc => "network_rpc_in_loop",
            Self::FilesystemBlocking => "fs_blocking_in_loop",
            Self::SequentialAwaitInLoop => "sequential_await_in_loop",
        }
    }
}

/// How arguments change in a recursive call site (for Master Theorem matching).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgReduction {
    None,
    /// `n - 1`, `n - 2`, `idx + 1`, `tail`
    Decrement,
    /// `n / 2`, `n >> 1`, `mid`, `slice[..len/2]`
    Halving,
}

/// Built-in language / stdlib patterns detected inside a function or loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntrinsicPattern {
    LinearScanInLoop {
        receiver: Option<String>,
        method: String,
        line: u32,
        loop_depth: u8,
    },
    SortInLoop {
        method: String,
        line: u32,
        loop_depth: u8,
    },
    StringConcatInLoop {
        target: String,
        line: u32,
        loop_depth: u8,
    },
    CloneInLoop {
        method: String,
        line: u32,
        loop_depth: u8,
    },
    ChainedAllocation {
        chain: String,
        depth: u8,
        line: u32,
    },
}

/// A contextual call site extracted from a function's full Tree-sitter AST.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstCallSite {
    pub callee_name: String,
    pub receiver: Option<String>,
    pub line: u32,
    pub loop_depth: u8,
    pub loop_multiplier: ComplexityClass,
    pub call_sites: u16,
    pub arg_reduction: ArgReduction,
}

/// Intra-procedural AST complexity summary for one symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AstComplexitySummary {
    pub symbol_id: String,
    pub local_time: ComplexityClass,
    pub local_space: ComplexityClass,
    pub max_loop_depth: u8,
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub call_sites: Vec<AstCallSite>,
    pub io_effects: Vec<(IoEffectKind, u32, u8)>,
    pub intrinsic_patterns: Vec<IntrinsicPattern>,
    pub has_memoization: bool,
    pub is_direct_recursive: bool,
}

/// One step along the dominant inter-procedural complexity blame path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BottleneckStep {
    pub symbol_id: String,
    pub qualified_name: String,
    pub file_path: String,
    pub line: u32,
    pub local_contribution: String,
    pub call_loop_multiplier: String,
    pub cumulative_complexity: String,
    pub reason: String,
}

/// Actionable, zero-LLM optimization advisory grounded in repository context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationAdvisory {
    pub rule_id: String,
    pub title: String,
    pub severity: String,
    pub symbol_id: String,
    pub qualified_name: String,
    pub file_path: String,
    pub hotspot_line: u32,
    pub current_complexity: String,
    pub target_complexity: String,
    pub problem: String,
    pub prescription: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_asset_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_diff: Option<String>,
}

/// Complete intra + inter-procedural complexity report for a symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolComplexityReport {
    pub symbol_id: String,
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub local_time: String,
    pub propagated_time: String,
    pub space_complexity: String,
    pub effective_degree: f64,
    pub risk_score: f64,
    pub max_loop_depth: u8,
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub recursion_kind: String,
    pub io_in_loop: Vec<String>,
    pub bottleneck_chain: Vec<BottleneckStep>,
    pub advisories: Vec<OptimizationAdvisory>,
}

/// Multi-dimensional CSIA impact vector in `[0.0, 1.0]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactDimensions {
    pub structural_blast_radius: f64,
    pub complexity_amplification: f64,
    pub contract_breakage_risk: f64,
    pub test_coverage_gap: f64,
}

/// Upstream caller where a local complexity increase is multiplied by an enclosing loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComplexityAmplificationPath {
    pub caller_id: String,
    pub caller_name: String,
    pub caller_file: String,
    pub caller_line: u32,
    pub cumulative_loop_depth: u8,
    pub amplification_factor: f64,
    pub time_before: String,
    pub time_after: String,
    pub call_chain: Vec<String>,
}

/// 1-hop inbound caller risked by a signature/contract modification or symbol deletion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractBreakageSite {
    pub caller_id: String,
    pub caller_name: String,
    pub caller_file: String,
    pub caller_line: u32,
    pub target_symbol: String,
    pub reason: String,
    pub severity: f64,
}

/// Full CSIA (Code Spreading-Impact Analysis) report for a set of modified symbols/files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeImpactReport {
    pub impact_level: String,
    pub composite_risk_score: f64,
    pub dimensions: ImpactDimensions,
    pub breaking_call_sites: Vec<ContractBreakageSite>,
    pub complexity_amplifications: Vec<ComplexityAmplificationPath>,
    pub affected_entrypoints: Vec<String>,
    pub minimal_test_suite: Vec<String>,
    pub untested_high_risk_paths: Vec<String>,
    pub reached_symbol_count: usize,
    pub reached_module_count: usize,
}

/// Token-budgeted Agent Guardrail Capsule returned by real-time MCP tools and live deltas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuardrailCapsule {
    pub verdict: String,
    pub latency_ms: f64,
    pub generation: String,
    pub summary_markdown: String,
    pub complexity_reports: Vec<SymbolComplexityReport>,
    pub impact_report: ChangeImpactReport,
    pub advisories: Vec<OptimizationAdvisory>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complexity_semiring_laws() {
        let c1 = ComplexityClass::constant();
        let log_n = ComplexityClass::log_n();
        let n = ComplexityClass::linear();
        let n_log_n = ComplexityClass::n_log_n();
        let n2 = ComplexityClass::quadratic();

        // Sequential max
        assert_eq!(c1.seq_max(n), n);
        assert_eq!(n.seq_max(log_n), n);
        assert_eq!(n_log_n.seq_max(n2), n2);

        // Nested multiplication
        assert_eq!(n.nest_mul(c1), n);
        assert_eq!(n.nest_mul(log_n), n_log_n);
        assert_eq!(n.nest_mul(n), n2);
        assert_eq!(n.nest_mul(n2), ComplexityClass::cubic());
        assert_eq!(n2.notation(), "O(n^2)");
        assert_eq!(n_log_n.notation(), "O(n log n)");
    }
}
