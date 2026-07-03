//! cognis-mcpd — standalone MCP stdio server binary (Task 7).
//!
//! Thin wrapper over the [`cognis_mcpd`] library `run()` entry point; the same
//! entry point is reused by the single multi-call `cognis` binary.
use std::process::ExitCode;

fn main() -> ExitCode {
    cognis_mcpd::run()
}
