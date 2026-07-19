/**
 * Property 9 + Property 10 — Sharing / HTTP topology tests.
 *
 * Feature: mcp-process-ram-duplication
 *
 * **Property 9: Bug Condition** — One heavy daemon per repository, thin proxy
 * or bounded HTTP
 * **Property 10: Bug Condition** — Sharing gated behind reversible fallback
 *
 * **Validates: Requirements 2.8, 2.9, 2.11**
 *
 * Property-based:
 * * For any flag + evidence combination, the gate is fail-closed: shared HTTP
 *   only when the flag is ON *and* every required check has non-empty evidence;
 *   otherwise topology is thin-proxy/per-repository-daemon stdio.
 * * For any multi-client host set on one canonical repo, thin-proxy server
 *   blocks never count as heavy daemons (host×repository cost is a thin proxy).
 *
 * Unit:
 * * Gate-OFF path selects thin-proxy-stdio.
 * * Failed gate retains stdio without data loss (`writeHttpMcpConfig` refuses
 *   to rewrite existing mcp.json).
 * * Thin-proxy server blocks are model-free (no ONNX / no DB retention markers).
 */
// Harness first: installs the vscode stub before modules that import vscode.
import "./testHarness";

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import fc from "fast-check";

import { resetHarness } from "./testHarness";
import {
  evaluateSharingGate,
  isSharedHttpAllowed,
  REQUIRED_GATE_CHECKS,
  selectSharingTopology,
  type GateCheckEvidence,
  type GateCheckId,
  type SharingTopology,
} from "../mcpSharingGate";
import {
  canWriteSharedHttpConfig,
  writeHttpMcpConfig,
} from "../mcpConfig";
import {
  THIN_PROXY_ENV,
  PROXY_TARGET_ENV,
  buildBinaryThinProxyServerBlock,
  isHttpServerBlock,
  isThinProxyServerBlock,
} from "../mcpServer";
import { isCognisMcpServerName } from "../mcpServerName";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function allPassingEvidence(): Record<GateCheckId, GateCheckEvidence> {
  const out = {} as Record<GateCheckId, GateCheckEvidence>;
  for (const id of REQUIRED_GATE_CHECKS) {
    out[id] = { passed: true, evidence: `pbt:${id}` };
  }
  return out;
}

/** Arbitrary evidence for one check: missing / fail / pass-without-pointer / pass. */
const arbCheckEvidence: fc.Arbitrary<GateCheckEvidence | undefined> = fc.oneof(
  fc.constant(undefined),
  fc.record({
    passed: fc.constant(false),
    evidence: fc.option(fc.string({ maxLength: 24 }), { nil: undefined }),
    detail: fc.option(fc.string({ maxLength: 24 }), { nil: undefined }),
  }),
  fc.record({
    passed: fc.constant(true),
    evidence: fc.constantFrom("", "   "),
  }),
  fc.record({
    passed: fc.constant(true),
    evidence: fc
      .string({ minLength: 1, maxLength: 32 })
      .filter((s) => s.trim().length > 0),
    detail: fc.option(fc.string({ maxLength: 24 }), { nil: undefined }),
  })
);

const arbEvidenceMap: fc.Arbitrary<
  Partial<Record<GateCheckId, GateCheckEvidence>>
> = fc
  .tuple(...REQUIRED_GATE_CHECKS.map(() => arbCheckEvidence))
  .map((entries) => {
    const out: Partial<Record<GateCheckId, GateCheckEvidence>> = {};
    REQUIRED_GATE_CHECKS.forEach((id, i) => {
      const e = entries[i];
      if (e !== undefined) {
        out[id] = e;
      }
    });
    return out;
  });

function evidenceIsStrictPass(e: GateCheckEvidence | undefined): boolean {
  return e?.passed === true && (e.evidence ?? "").trim().length > 0;
}

function allChecksStrictPass(
  evidence: Partial<Record<GateCheckId, GateCheckEvidence>>
): boolean {
  return REQUIRED_GATE_CHECKS.every((id) => evidenceIsStrictPass(evidence[id]));
}

function withTempHome(): string {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-sharing-home-"));
  process.env.USERPROFILE = home;
  process.env.HOME = home;
  return home;
}

function mkRepo(tag: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), `cognis-sharing-${tag}-`));
}

function cleanup(...dirs: string[]): void {
  for (const d of dirs) {
    try {
      fs.rmSync(d, { recursive: true, force: true });
    } catch {
      /* best effort */
    }
  }
}

/**
 * Count heavy cognis stdio blocks a host would start from a config map.
 * Thin proxies and HTTP url blocks are excluded (Requirements 2.8, 2.11).
 */
function heavyCognisCount(servers: Record<string, unknown>): number {
  return Object.entries(servers).filter(
    ([name, block]) =>
      isCognisMcpServerName(name) &&
      !isHttpServerBlock(block) &&
      !isThinProxyServerBlock(block)
  ).length;
}

function thinProxyCognisCount(servers: Record<string, unknown>): number {
  return Object.entries(servers).filter(
    ([name, block]) =>
      isCognisMcpServerName(name) && isThinProxyServerBlock(block)
  ).length;
}

// ---------------------------------------------------------------------------
// Property 10 — Sharing gated behind reversible fallback
// ---------------------------------------------------------------------------

test("Property 10: for any flag+evidence, sharing is fail-closed and fallback is thin-proxy-stdio", () => {
  // **Validates: Requirements 2.9**
  fc.assert(
    fc.property(fc.boolean(), arbEvidenceMap, (flagEnabled, evidence) => {
      const decision = evaluateSharingGate(flagEnabled, evidence);
      const shouldOpen = flagEnabled && allChecksStrictPass(evidence);

      assert.equal(decision.flagEnabled, flagEnabled);
      assert.equal(decision.sharingEnabled, shouldOpen);
      assert.equal(
        decision.topology,
        shouldOpen ? "shared-http" : "thin-proxy-stdio"
      );
      assert.equal(decision.checks.length, REQUIRED_GATE_CHECKS.length);
      assert.equal(isSharedHttpAllowed(flagEnabled, evidence), shouldOpen);
      assert.equal(
        selectSharingTopology(flagEnabled, evidence),
        decision.topology
      );

      if (!shouldOpen) {
        assert.ok(
          decision.fallbackReason,
          "closed/failed gate must record a fallback reason"
        );
        // Failed-gate path (flag ON but checks incomplete) must promise no data
        // loss; flag-OFF path must name the default-OFF flag.
        if (flagEnabled) {
          assert.match(decision.fallbackReason!, /no data loss/i);
          assert.match(decision.fallbackReason!, /gate checks failed/i);
        } else {
          assert.match(decision.fallbackReason!, /flag is OFF/i);
        }
        assert.notEqual(decision.topology, "shared-http" as SharingTopology);
      } else {
        assert.equal(decision.fallbackReason, undefined);
        assert.ok(decision.checks.every((c) => c.passed));
      }
    }),
    { numRuns: 120 }
  );
});

test("Property 10: any single failing check keeps the gate closed", () => {
  // **Validates: Requirements 2.9**
  fc.assert(
    fc.property(
      fc.constantFrom(...REQUIRED_GATE_CHECKS),
      fc.boolean(),
      (failId, useEmptyPointer) => {
        const evidence = allPassingEvidence();
        evidence[failId] = useEmptyPointer
          ? { passed: true, evidence: "  " }
          : { passed: false, evidence: "fail", detail: "injected" };
        const decision = evaluateSharingGate(true, evidence);
        assert.equal(decision.sharingEnabled, false);
        assert.equal(decision.topology, "thin-proxy-stdio");
        assert.match(decision.fallbackReason ?? "", new RegExp(failId));
        assert.match(decision.fallbackReason ?? "", /no data loss/i);
      }
    ),
    { numRuns: 40 }
  );
});

// ---------------------------------------------------------------------------
// Property 9 — Thin proxy / one-heavy topology at the config boundary
// ---------------------------------------------------------------------------

test("Property 9: multi-host thin-proxy blocks never count as heavy daemons", () => {
  // **Validates: Requirements 2.8, 2.11**
  // For any number of hosts attaching to one canonical repo via thin-proxy
  // server blocks, heavy count stays 0 and thin-proxy count equals the host set.
  fc.assert(
    fc.property(
      fc.integer({ min: 1, max: 8 }),
      fc.stringMatching(/^[a-z0-9-]{1,12}$/),
      (hostCount, slug) => {
        const servers: Record<string, unknown> = {};
        // One cognis entry name (canonical repo) — each "host" is modeled as a
        // separate thin-proxy block under a distinct key only for the counter
        // algebra; production writes one name per repo, and each host process
        // starts that block as a thin proxy (not a heavy).
        for (let i = 0; i < hostCount; i++) {
          const name =
            i === 0 ? `cognis-${slug}` : `cognis-${slug}-host${i}`;
          servers[name] = buildBinaryThinProxyServerBlock(
            "/bin/cognis",
            { COGNIS_DB_PATH: `/repos/${slug}/.cognis/uckg.db` },
            `http://127.0.0.1:${50000 + i}/mcp`
          );
        }
        assert.equal(
          heavyCognisCount(servers),
          0,
          "thin-proxy blocks must never count as heavy daemons"
        );
        assert.equal(thinProxyCognisCount(servers), hostCount);
        for (const block of Object.values(servers)) {
          assert.equal(isThinProxyServerBlock(block), true);
          // Model-free markers: proxy args + env; no ONNX force.
          const b = block as {
            args?: string[];
            env?: Record<string, string>;
          };
          assert.ok(b.args?.includes("--proxy") || b.env?.[THIN_PROXY_ENV] === "1");
          assert.equal(b.env?.[THIN_PROXY_ENV], "1");
          assert.equal(b.env?.COGNIS_ONNX_MODEL_DIR, undefined);
        }
      }
    ),
    { numRuns: 60 }
  );
});

// ---------------------------------------------------------------------------
// Unit: gate-off path → thin-proxy; failed gate retains stdio without data loss
// ---------------------------------------------------------------------------

test("unit: gate-OFF path selects thin-proxy-stdio even with full evidence", () => {
  // **Validates: Requirements 2.8, 2.9**
  const decision = evaluateSharingGate(false, allPassingEvidence());
  assert.equal(decision.topology, "thin-proxy-stdio");
  assert.equal(decision.sharingEnabled, false);
  assert.equal(isSharedHttpAllowed(false, allPassingEvidence()), false);
  assert.equal(selectSharingTopology(false, allPassingEvidence()), "thin-proxy-stdio");
});

test("unit: thin-proxy server block is model-free (no ONNX / retains no DB ownership)", () => {
  // **Validates: Requirements 2.8, 2.11**
  // The proxy may *forward* COGNIS_DB_PATH so a spawned heavy opens the right
  // DB, but the block itself is classified as thin: it never maps ONNX and
  // never counts as a heavy owner.
  const block = buildBinaryThinProxyServerBlock(
    "/opt/cognis/cognis",
    {
      COGNIS_DB_PATH: "/repo/.cognis/uckg.db",
      COGNIS_REPO_ROOT: "/repo",
    },
    "http://127.0.0.1:50123/mcp"
  );
  assert.equal(isThinProxyServerBlock(block), true);
  assert.equal(isHttpServerBlock(block), false);
  assert.deepEqual(block.args, [
    "mcpd",
    "--proxy",
    "--proxy-target",
    "http://127.0.0.1:50123/mcp",
  ]);
  assert.equal(block.env[THIN_PROXY_ENV], "1");
  assert.equal(block.env[PROXY_TARGET_ENV], "http://127.0.0.1:50123/mcp");
  // No ONNX session path is forced onto the thin proxy process.
  assert.equal(block.env.COGNIS_ONNX_MODEL_DIR, undefined);
  assert.equal(block.env.ORT_DYLIB_PATH, undefined);
  // Heavy counter algebra: this block contributes 0 heavy daemons.
  assert.equal(
    heavyCognisCount({ "cognis-example": block }),
    0
  );
});

test("unit: failed gate writeHttpMcpConfig retains existing stdio config without data loss", () => {
  // **Validates: Requirements 2.9** (preservation 3.8)
  const home = withTempHome();
  const repo = mkRepo("gate-fail");
  try {
    // Ensure shared-HTTP flag is OFF (default) and no evidence is present.
    delete process.env.COGNIS_MCP_SHARED_HTTP;
    delete process.env.COGNIS_MCP_SHARING_GATE_EVIDENCE;
    resetHarness(repo, {
      appName: "Cursor",
      config: {
        cognis: {
          mcpHost: "cursor",
          mcpConfigScope: "workspace",
          mcpSharedHttpEnabled: false,
        },
      },
    });

    // Seed an existing thin-proxy stdio mcp.json (the compatible path).
    const configPath = path.join(repo, ".cursor", "mcp.json");
    fs.mkdirSync(path.dirname(configPath), { recursive: true });
    const original = {
      mcpServers: {
        "cognis-seed": buildBinaryThinProxyServerBlock("/bin/cognis", {
          COGNIS_DB_PATH: path.join(repo, ".cognis", "uckg.db"),
        }),
        "user-other": {
          command: "npx",
          args: ["-y", "some-other-mcp"],
        },
      },
    };
    const originalJson = JSON.stringify(original, null, 2);
    fs.writeFileSync(configPath, originalJson, "utf8");

    assert.equal(canWriteSharedHttpConfig(repo), false);

    const result = writeHttpMcpConfig(repo, "http://127.0.0.1:59999/mcp");
    assert.equal(result.written, false, "gate-closed write must refuse");
    assert.equal(result.gate.sharingEnabled, false);
    assert.equal(result.gate.topology, "thin-proxy-stdio");
    assert.ok(result.gate.fallbackReason);

    // Byte-for-byte retention of the previous config (no data loss).
    const after = fs.readFileSync(configPath, "utf8");
    assert.equal(after, originalJson);

    const parsed = JSON.parse(after) as {
      mcpServers: Record<string, unknown>;
    };
    assert.ok(parsed.mcpServers["user-other"], "non-Cognis entry preserved");
    assert.equal(
      isThinProxyServerBlock(parsed.mcpServers["cognis-seed"]),
      true,
      "stdio thin-proxy path retained"
    );
    assert.equal(isHttpServerBlock(parsed.mcpServers["cognis-seed"]), false);
  } finally {
    cleanup(home, repo);
  }
});

test("unit: failed gate with flag ON still refuses HTTP rewrite (no data loss)", () => {
  // **Validates: Requirements 2.9**
  const home = withTempHome();
  const repo = mkRepo("gate-flag-on-fail");
  try {
    process.env.COGNIS_MCP_SHARED_HTTP = "1";
    // No evidence file / env → every check missing → fail-closed.
    delete process.env.COGNIS_MCP_SHARING_GATE_EVIDENCE;
    resetHarness(repo, {
      appName: "Cursor",
      config: {
        cognis: {
          mcpHost: "cursor",
          mcpConfigScope: "workspace",
          mcpSharedHttpEnabled: true,
        },
      },
    });

    const configPath = path.join(repo, ".cursor", "mcp.json");
    fs.mkdirSync(path.dirname(configPath), { recursive: true });
    const original = {
      mcpServers: {
        "cognis-seed": {
          command: "/bin/cognis",
          args: ["mcpd", "--proxy"],
          env: { [THIN_PROXY_ENV]: "1", COGNIS_DB_PATH: "/db" },
        },
      },
    };
    const originalJson = JSON.stringify(original, null, 2);
    fs.writeFileSync(configPath, originalJson, "utf8");

    const result = writeHttpMcpConfig(repo, "http://127.0.0.1:58888/mcp");
    assert.equal(result.written, false);
    assert.equal(result.gate.flagEnabled, true);
    assert.equal(result.gate.sharingEnabled, false);
    assert.equal(result.gate.topology, "thin-proxy-stdio");
    assert.match(result.gate.fallbackReason ?? "", /no data loss/i);
    assert.equal(fs.readFileSync(configPath, "utf8"), originalJson);
  } finally {
    delete process.env.COGNIS_MCP_SHARED_HTTP;
    cleanup(home, repo);
  }
});
