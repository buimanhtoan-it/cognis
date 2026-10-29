//! cognis-indexd — standalone live indexing daemon binary (Task 7.3).
//!
//! Thin wrapper over the [`cognis_indexd`] library `run()` entry point; the
//! same entry point is reused by the single multi-call `cognis` binary.
use std::process::ExitCode;

fn main() -> ExitCode {
    cognis_indexd::run()
}
