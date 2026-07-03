//! cognis — single multi-call binary (Task 7.3, design "Single-binary
//! distribution").
//!
//! One static executable that dispatches to the CLI, the MCP daemon, or the
//! indexing daemon — busybox-style. Dispatch is resolved in two ways:
//!
//! 1. **argv[0] basename** — when the binary is installed/symlinked as
//!    `cognis-cli`, `cognis-mcpd`, or `cognis-indexd`, it behaves exactly like
//!    that tool (so existing `mcp.json` wiring and scripts keep working).
//! 2. **leading subcommand** — when invoked as `cognis`, the first argument
//!    selects the surface: `cognis mcpd …`, `cognis indexd …`, or `cognis cli …`
//!    (and any other first token, e.g. `cognis init`, is treated as a CLI
//!    subcommand).
//!
//! Each surface reuses the same `run_from(..)` entry point the standalone bins
//! call, so there is a single source of truth per tool (fallback B — three
//! separate binaries — remains available from the same crates).

use std::ffi::OsString;
use std::process::ExitCode;

/// Which surface an invocation resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Cli,
    Mcpd,
    Indexd,
}

/// Resolve the dispatch target from the program name (`argv[0]`) and the first
/// argument. `arg0_stem` is the lowercased file stem of `argv[0]`.
fn resolve_target(arg0_stem: &str, first_arg: Option<&str>) -> (Target, ConsumeFirst) {
    // 1. argv[0] basename wins (busybox-style symlink dispatch).
    if arg0_stem.contains("mcpd") {
        return (Target::Mcpd, ConsumeFirst::No);
    }
    if arg0_stem.contains("indexd") {
        return (Target::Indexd, ConsumeFirst::No);
    }
    if arg0_stem.ends_with("-cli") || arg0_stem == "cognis-cli" {
        return (Target::Cli, ConsumeFirst::No);
    }

    // 2. Otherwise (invoked as `cognis`): a leading surface token selects the
    //    target and is consumed; anything else is a CLI subcommand.
    match first_arg {
        Some("mcpd") | Some("serve") => (Target::Mcpd, ConsumeFirst::Yes),
        Some("indexd") | Some("daemon") | Some("watch") => (Target::Indexd, ConsumeFirst::Yes),
        Some("cli") => (Target::Cli, ConsumeFirst::Yes),
        _ => (Target::Cli, ConsumeFirst::No),
    }
}

/// Whether the first argument was a surface selector that must be dropped before
/// the target parses its own args.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsumeFirst {
    Yes,
    No,
}

fn main() -> ExitCode {
    let raw: Vec<OsString> = std::env::args_os().collect();
    let arg0 = raw.first().cloned().unwrap_or_default();
    let arg0_stem = std::path::Path::new(&arg0)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let first_arg = raw.get(1).map(|s| s.to_string_lossy().into_owned());

    let (target, consume) = resolve_target(arg0_stem.as_str(), first_arg.as_deref());

    // Build the argv each surface sees: keep argv[0] as a stable program name
    // and drop the surface selector token when one was consumed.
    let prog: OsString = match target {
        Target::Cli => OsString::from("cognis-cli"),
        Target::Mcpd => OsString::from("cognis-mcpd"),
        Target::Indexd => OsString::from("cognis-indexd"),
    };
    let mut args: Vec<OsString> = vec![prog];
    let rest_start = if consume == ConsumeFirst::Yes { 2 } else { 1 };
    args.extend(raw.into_iter().skip(rest_start));

    match target {
        Target::Cli => cognis_cli::run_from(args),
        Target::Mcpd => {
            // mcpd selects its transport from argv (`--transport http …`); the
            // engine is chosen via env (COGNIS_DB_PATH / fixture).
            cognis_mcpd::run_from(args)
        }
        Target::Indexd => cognis_indexd::run_from(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv0_basename_dispatch() {
        assert_eq!(resolve_target("cognis-mcpd", None).0, Target::Mcpd);
        assert_eq!(resolve_target("cognis-indexd", None).0, Target::Indexd);
        assert_eq!(resolve_target("cognis-cli", None).0, Target::Cli);
        // basename wins even with a subcommand present.
        assert_eq!(resolve_target("cognis-mcpd", Some("init")).0, Target::Mcpd);
    }

    #[test]
    fn subcommand_dispatch_for_unified_binary() {
        assert_eq!(
            resolve_target("cognis", Some("mcpd")),
            (Target::Mcpd, ConsumeFirst::Yes)
        );
        assert_eq!(
            resolve_target("cognis", Some("serve")),
            (Target::Mcpd, ConsumeFirst::Yes)
        );
        assert_eq!(
            resolve_target("cognis", Some("indexd")),
            (Target::Indexd, ConsumeFirst::Yes)
        );
        assert_eq!(
            resolve_target("cognis", Some("daemon")),
            (Target::Indexd, ConsumeFirst::Yes)
        );
        assert_eq!(
            resolve_target("cognis", Some("cli")),
            (Target::Cli, ConsumeFirst::Yes)
        );
    }

    #[test]
    fn unknown_first_token_is_a_cli_subcommand() {
        // `cognis init` / `cognis health` route to the CLI without consuming.
        assert_eq!(
            resolve_target("cognis", Some("init")),
            (Target::Cli, ConsumeFirst::No)
        );
        assert_eq!(
            resolve_target("cognis", Some("health")),
            (Target::Cli, ConsumeFirst::No)
        );
        assert_eq!(
            resolve_target("cognis", None),
            (Target::Cli, ConsumeFirst::No)
        );
    }
}
