# Private-byte / process-cardinality measurement

**Spec:** `mcp-process-ram-duplication` task 9.2  
**Requirements:** 2.11, 2.14 · **Preservation:** 3.10, 3.11

This directory holds the **platform-correct measurement procedure** and the
`measure.mjs` script used to record process cardinality (`A` / `H` / `I`) and
aggregate private bytes for Cognis MCP/index daemons. Results are **empirical**
for a named hardware / build / model / topology — never a universal claim.

## Why this exists

The recorded defect snapshot was **6 `mcpd` + 2 `indexd` at ~154 MiB private
bytes each (~1.23 GiB aggregate)** on one Windows topology. Acceptance after
the fix requires, for an **equivalent stabilized-idle** reproduction on the
**same machine / build / model / topology**:

| Signal | Gate |
| --- | --- |
| Heavy repository daemons | `≤ A` (no host × repository heavy fan-out) |
| `indexd` processes | `≤ I` and `≤ 1` per repository |
| Thin proxies | `≤ H` and model-free |
| Median aggregate private bytes | `≤ 0.615 GiB` across **≥ 5** clean runs (target, not assumed achieved) |
| Per-run ceiling | no run exceeds the ~1.23 GiB baseline |
| Post-stop orphans | zero owned Cognis daemon/orphan processes after client stop + grace period |
| Active-load peak | reported **separately** (not claimed reduced without evidence) |

Constants used by the script (override with flags / env):

| Name | Default | Meaning |
| --- | --- | --- |
| `BASELINE_PRIVATE_BYTES_GIB` | `1.23` | recorded defect aggregate (ceiling per run) |
| `TARGET_MEDIAN_PRIVATE_BYTES_GIB` | `0.615` | acceptance median target |
| `MIN_RUNS` | `5` | minimum clean runs for a median |
| `GRACE_PERIOD_S` | `35` | slightly above lease TTL (30 s) before orphan check |

## Isolation (preservation 3.10)

Every measurement **must**:

1. Create a throwaway isolation root under the OS temp directory  
   (`os.tmpdir()/cognis-pb-measure-<id>/`).
2. Place synthetic repositories only under that root (`repos/`).
3. Redirect config homes so host MCP config is never read or written:
   - Windows: `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `HOME`
   - POSIX: `HOME`, `XDG_CONFIG_HOME`
4. Point Cognis paths into the isolation root only (`COGNIS_DB_PATH`,
   `COGNIS_REPO_ROOT`, `COGNIS_AUDIT_LOG`, `COGNIS_INDEXD_STATUS_PATH`, …).
5. **Never** open, modify, or delete the developer’s real repository `.cognis/`
   or real host `mcp.json` (`~/.cursor/mcp.json`, `~/.vscode/mcp.json`, …).

The script enforces (1)–(4) by construction. Operators must not pass a real
workspace path as `--repo` unless that path is itself a disposable clone under
a temp root.

## Cardinality definitions (requirement 2.11)

| Symbol | Name | How measured |
| --- | --- | --- |
| **A** | Active canonical repositories | Distinct canonical roots under measurement (symlink/case-resolved absolute path + DB path). Default: number of isolation repos with a live Cognis lease or status. |
| **H** | Active MCP client connections | Count of editor-facing Cognis MCP stdio processes (thin proxies + heavy stdio attachments) that belong to the isolation root. |
| **I** | Actively indexing repositories | Distinct repos with a live `indexd` process (or active `indexd-status.json` / `indexd.lease`) under the isolation root. |

Acceptance inequalities (same requirement):

```text
heavy_repository_daemons  ≤  A
indexd_processes          ≤  I   AND   ≤ 1 per repository
thin_proxies              ≤  H   AND   model-free (no ONNX / no DB)
```

### Process classification

Command-line (and env when readable) markers:

| Class | Markers |
| --- | --- |
| **mcpd (any)** | argv contains `mcpd`, or basename `cognis-mcpd` / `cognis_mcpd` |
| **thin proxy** | `--proxy`, `--transport proxy` / `--transport=proxy`, or `COGNIS_MCP_PROXY=1` |
| **heavy mcpd** | mcpd that is **not** a thin proxy |
| **indexd** | argv contains `indexd` / `daemon` / `watch`, or basename `cognis-indexd` |
| **heavy repository daemon** | heavy mcpd **or** indexd (holds DB and/or may map ONNX) |

On Windows, process env is usually unreadable; classification uses **command
line** from `Win32_Process.CommandLine` (same approach as
`apps/cognis-vscode/src/mcpRuntime.ts`).

## Private-byte metric (platform-correct)

| Platform | Metric | Source |
| --- | --- | --- |
| **Windows** | **Private bytes** (committed private) | `Get-Process … PrivateMemorySize64` (equivalent to Process Explorer “Private Bytes”) |
| **Linux** | Private-ish RSS proxy | `/proc/<pid>/status` `RssAnon` when present, else `VmRSS` — **labeled** as non-Windows proxy |
| **macOS** | RSS | `ps -o rss=` — **labeled** as non-Windows proxy |

**Aggregate** = sum over the Cognis process **tree** rooted at each matching
daemon PID (parent + descendants), so helper children are included. The script
dedupes PIDs so shared children are not double-counted.

Windows is the **authoritative** surface for the ~1.23 → 0.615 GiB target.
Cross-platform numbers are for trend only and must not be mixed into the same
median without an explicit platform label.

## Stabilized-idle vs active-load peak (3.11)

Evidence must distinguish:

1. **Process cardinality** — `A`, `H`, `I`, heavy / thin / indexd counts  
2. **Idle private bytes** — aggregate private bytes after topology is up and
   **no semantic / index work** is in flight for a settle window  
3. **Active-load peak private bytes** — max aggregate observed while driving
   tool / index load (optional `--active-load` mode)  
4. **Model mappings** — whether heavy processes are expected to hold ONNX
   (eager vs lazy policy)  
5. **Run variance** — per-run values, median, min, max across ≥5 clean runs  

Label every published figure:

> Measured on \<hostname\>, \<OS\>, build \<git-sha / version\>, model
> \<fingerprint or path\>, topology \<A,H,I description\>, n=\<runs\>,
> **empirical** — not a universal guarantee.

Do **not** present the 0.615 GiB figure as already achieved until the median
gate passes on the recorded machine/topology.

## Procedure (operator)

### Prerequisites

- Built `cognis` binary on `PATH`, or pass `--binary <path>`  
  (`cargo build -p cognis --release` → `target/release/cognis[.exe]`).
- Windows: PowerShell 5+ (used for private bytes + process tree).
- Optional ONNX assets only if the topology under test loads them; the defect
  baseline assumed eager model map per heavy process.

### Clean run loop (≥5)

```bash
# From the repository root (Node 18+).
node tests/e2e/private-bytes/measure.mjs \
  --runs 5 \
  --binary target/release/cognis.exe \
  --topology baseline-idle \
  --out tests/e2e/private-bytes/out/report.json
```

What one clean run does:

1. Create isolation root under temp; never touch real `.cognis` / host MCP config.  
2. Materialize `A` synthetic repos (default 3 to mirror multi-repo fan-out scenarios).  
3. Spawn the measured topology (default: heavy `mcpd` per repo + optional
   `indexd` — override with flags).  
4. Wait for the settle window (default 8 s) with **no tool calls** → stabilized idle.  
5. Sample process tree private bytes + classify heavy / thin / indexd.  
6. Record `A`, `H`, `I`, per-process bytes, aggregate GiB.  
7. Stop clients/daemons; wait **grace period** (default 35 s ≥ lease TTL 30 s);
   assert **zero** owned Cognis processes remain under the isolation root.  
8. Tear down isolation root.

After `N ≥ 5` runs, the script prints:

- median / min / max aggregate idle private bytes (GiB)  
- cardinality summary  
- gate checks vs baseline ceiling and median target  
- orphan survival counts  

### Topology presets

| `--topology` | Intent |
| --- | --- |
| `baseline-idle` | Reproduce multi-repo idle heavy processes (defect-style fan-out optional via `--fan-out-hosts`) |
| `fixed-idle` | Prefer thin-proxy / one-heavy-per-repo shape when the binary supports `--proxy` |
| `sample-only` | Do **not** spawn; only sample processes whose command line / env references the isolation root or `--filter-path` (for live host E2E) |

### Active-load peak (separate column)

```bash
node tests/e2e/private-bytes/measure.mjs \
  --runs 5 \
  --topology fixed-idle \
  --active-load \
  --out tests/e2e/private-bytes/out/active.json
```

Active peak is stored under `active_load_peak_private_bytes_gib` and is **never**
substituted for the idle median gate.

### Interpreting the JSON report

```json
{
  "schema_version": 1,
  "evidence_tier": "empirical",
  "platform": { "os": "win32", "metric": "private_bytes", "authoritative": true },
  "hardware_label": "...",
  "build": { "binary": "...", "version": "..." },
  "constants": {
    "baseline_private_bytes_gib": 1.23,
    "target_median_private_bytes_gib": 0.615,
    "min_runs": 5,
    "grace_period_s": 35
  },
  "topology": { "name": "baseline-idle", "A": 3, "H": 3, "I": 2 },
  "runs": [ /* per-run samples */ ],
  "summary": {
    "n": 5,
    "idle_aggregate_private_bytes_gib": { "median": 0.0, "min": 0.0, "max": 0.0 },
    "cardinality": { "heavy_daemons": {}, "thin_proxies": {}, "indexd": {} },
    "gates": {
      "median_at_or_below_target": false,
      "no_run_above_baseline": true,
      "heavy_le_A": true,
      "indexd_le_I": true,
      "thin_le_H": true,
      "zero_orphans_after_grace": true
    }
  }
}
```

Gates may fail on unfixed code — that is expected evidence for the bug
condition `processCardinalityOrMeasuredPrivateBytesExceedsTarget`. After the
fix, re-run on the same machine/topology and attach the report to the release
notes / development criteria (task 9.3).

## Manual Windows one-shot (sanity)

When debugging a single live tree without the full harness:

```powershell
# Private bytes for one PID (bytes → GiB)
$p = Get-Process -Id <pid>
"{0:N3} GiB" -f ($p.PrivateMemorySize64 / 1GB)

# Command lines for cognis mcpd / indexd
Get-CimInstance Win32_Process |
  Where-Object { $_.CommandLine -match 'mcpd|indexd|cognis' } |
  Select-Object ProcessId, ParentProcessId, CommandLine
```

Prefer `measure.mjs` for acceptance: it enforces isolation, multi-run median,
tree aggregation, and gate labels.

## Safety rules

- Do not kill processes outside the isolation root / measured PID set.  
- Orphan checks only count Cognis daemons tied to the isolation path.  
- Prefer safe non-destruction when ownership is ambiguous (preservation 3.9).  
- Never publish a single-run number as the acceptance median.

## Related surfaces

- Process probe (extension): `apps/cognis-vscode/src/mcpRuntime.ts`  
- Thin-proxy classification: `apps/cognis-vscode/src/mcpServer.ts`  
- Lease TTL / grace: `crates/cognis-core/src/lease.rs` (`DEFAULT_LEASE_TTL` = 30 s)  
- Development criteria: `docs/development-criteria.md` (Pillar 4 process/RAM
  rows point here; release claims must stay empirical and must not present
  0.615 GiB as already achieved without a named harness report)  
- E2E overview: `docs/e2e-testing.md`  
- User/admin migration, warm policy, multi-host lifecycle: `docs/mcp-client-config.md`  
- Loopback / credentials / fingerprints: `docs/security.md`  
- Changelog: `CHANGELOG.md` `[Unreleased]`
