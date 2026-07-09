/**
 * Webview panel UI tests, driven by Playwright against the simulator pages.
 *
 * For every fixture state the simulator built (src/sim/fixtures.ts) this:
 *   1. asserts the page renders exactly the buttons the contract expects;
 *   2. clicks every enabled button and asserts each posts a `{type:'action'}`
 *      message whose id resolves to a real VS Code command (or installPrereq
 *      with an itemId) — i.e. no dead buttons;
 *   3. asserts disabled buttons post nothing.
 *
 * Run: `npm run test:e2e` (builds the pages first).
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { pathToFileURL } from "node:url";

import { expect, test } from "@playwright/test";

interface ButtonAction {
  id: string;
  itemId?: string;
  disabled: boolean;
}
interface Manifest {
  commandMap: Record<string, string>;
  fixtures: Array<{ name: string; title: string; file: string; actions: ButtonAction[] }>;
}

const SIM_DIR = path.join(__dirname, "..", "sim-dist");
const MANIFEST_PATH = path.join(SIM_DIR, "manifest.json");

if (!fs.existsSync(MANIFEST_PATH)) {
  throw new Error(
    `Simulator pages missing (${MANIFEST_PATH}). Run "npm run sim:build" first ` +
      `(or "npm run test:e2e" which builds then tests).`,
  );
}

const manifest = JSON.parse(fs.readFileSync(MANIFEST_PATH, "utf8")) as Manifest;

function pageUrl(file: string): string {
  return pathToFileURL(path.join(SIM_DIR, file)).href;
}

// Open every <details> so buttons nested in collapsed sections (danger zone,
// prerequisites) are visible and clickable.
async function expandAll(page: import("@playwright/test").Page): Promise<void> {
  await page.$$eval("details", (els) => {
    for (const d of els) (d as HTMLDetailsElement).open = true;
  });
}

async function readPosted(
  page: import("@playwright/test").Page,
): Promise<Array<{ type: string; id: string; itemId?: string }>> {
  return page.evaluate(
    () => (window as unknown as { __cognisMessages: Array<{ type: string; id: string; itemId?: string }> }).__cognisMessages,
  );
}

for (const fx of manifest.fixtures) {
  test.describe(`panel: ${fx.title}`, () => {
    const url = pageUrl(fx.file);

    test("renders exactly the expected action buttons", async ({ page }) => {
      await page.goto(url);
      const ids = await page.$$eval("[data-action]", (els) =>
        els.map((e) => e.getAttribute("data-action") ?? ""),
      );
      expect(ids.sort()).toEqual(fx.actions.map((a) => a.id).sort());
    });

    test("every enabled button posts a valid command intent", async ({ page }) => {
      await page.goto(url);
      await expandAll(page);
      const enabled = fx.actions.filter((a) => !a.disabled);
      for (const a of enabled) {
        const sel = a.itemId
          ? `[data-action="${a.id}"][data-item="${a.itemId}"]`
          : `[data-action="${a.id}"]`;
        await page.locator(sel).first().click();
      }
      const posted = await readPosted(page);
      expect(posted.length).toBe(enabled.length);
      for (const m of posted) {
        expect(m.type).toBe("action");
        if (m.id === "installPrerequisite") {
          expect(m.itemId, "installPrerequisite must carry an itemId").toBeTruthy();
        } else {
          expect(
            manifest.commandMap[m.id],
            `action "${m.id}" must map to a real command`,
          ).toBeTruthy();
        }
      }
    });

    test("disabled buttons post nothing", async ({ page }) => {
      await page.goto(url);
      await expandAll(page);
      const disabled = fx.actions.filter((a) => a.disabled);
      for (const a of disabled) {
        await page
          .locator(`[data-action="${a.id}"]`)
          .first()
          .click({ force: true })
          .catch(() => undefined);
      }
      const ids = (await readPosted(page)).map((m) => m.id);
      for (const a of disabled) {
        expect(ids).not.toContain(a.id);
      }
    });
  });
}

// ---------------------------------------------------------------------------
// Wording / jargon coherence (Requirements R12.7, R12.8; backed by R9.1, R3).
//
// For every rendered fixture page we assert the *visible* text (body
// textContent — not raw attributes) is free of the doubled command prefix,
// of internal jargon, and of user-visible "Backend". The raw MCP URL / server
// id / error string are allowed because they now live in labeled detail rows
// (Address:/Server:/Details:) — the jargon *words* themselves must be absent.
// ---------------------------------------------------------------------------

// Banned internal jargon — Requirement 9, criterion 1 (case-insensitive match
// against visible text). "transport" also covers "stdio transport".
const BANNED_JARGON = [
  "stdio transport",
  "binding port",
  "debounce queue",
  "transport",
  "handshake",
  "socket",
];

test.describe("panel wording coherence", () => {
  for (const fx of manifest.fixtures) {
    test(`${fx.name}: no doubled prefix, no jargon, Engine not Backend`, async ({ page }) => {
      await page.goto(pageUrl(fx.file));
      // Expand collapsed sections so their text is part of the visible tree.
      await expandAll(page);
      const text = (await page.locator("body").textContent()) ?? "";
      const lower = text.toLowerCase();

      // R12.7: the "Cognis: " command prefix must never be doubled.
      expect(text, 'visible text must not contain "Cognis: Cognis:"').not.toContain(
        "Cognis: Cognis:",
      );

      // R12.7 / R3: no user-visible "Backend"/"backend" — the core binary is
      // consistently called the "Engine".
      expect(lower, 'visible text must not contain "Backend"').not.toContain("backend");

      // R12.8 / R9.1: none of the banned jargon terms in visible text.
      for (const term of BANNED_JARGON) {
        expect(
          lower,
          `visible text must not contain banned jargon "${term}"`,
        ).not.toContain(term.toLowerCase());
      }
    });
  }

  test("engine-install flow uses the Engine terminology", async ({ page }) => {
    // The fresh-machine state surfaces the install-engine call to action; it
    // must speak of the "Engine" (R3) — proves the terminology is present, not
    // merely that "Backend" is absent.
    await page.goto(pageUrl("fresh-machine.html"));
    const lower = ((await page.locator("body").textContent()) ?? "").toLowerCase();
    expect(lower, 'expected user-visible "Engine" terminology').toContain("engine");
  });
});

// ---------------------------------------------------------------------------
// Round-trip command intent (Requirements R12.3–R12.6, R12.11).
//
// "Round trip" in the static simulator = complementary fixtures each render the
// button that transitions in one direction, and clicking it posts the action id
// that resolves — through manifest.commandMap — to the expected cognis.* command
// (no dead buttons). Bounding render time ≤10s is trivial in the simulator.
// ---------------------------------------------------------------------------

function fixtureByName(name: string): Manifest["fixtures"][number] {
  const fx = manifest.fixtures.find((f) => f.name === name);
  if (!fx) throw new Error(`fixture "${name}" missing from manifest`);
  return fx;
}

async function assertButtonResolves(
  page: import("@playwright/test").Page,
  file: string,
  actionId: string,
  expectedCommand: string,
): Promise<void> {
  await page.goto(pageUrl(file));
  await expandAll(page);

  const buttons = page.locator(`[data-action="${actionId}"]`);
  expect(
    await buttons.count(),
    `${file} must render at least one "${actionId}" button`,
  ).toBeGreaterThanOrEqual(1);
  await buttons.first().click();

  const posted = await readPosted(page);
  const match = posted.find((m) => m.id === actionId);
  expect(match, `clicking "${actionId}" must post an action message`).toBeTruthy();
  expect(match?.type).toBe("action");

  // The posted id must resolve to the expected real command (R12.11: any
  // unresolved id fails the round trip).
  expect(
    manifest.commandMap[actionId],
    `action "${actionId}" must resolve to "${expectedCommand}"`,
  ).toBe(expectedCommand);
}

test.describe("panel round-trip command pairs", () => {
  // R12.3: start MCP ↔ stop MCP.
  test("start ↔ stop MCP post the complementary commands", async ({ page }) => {
    await assertButtonResolves(
      page,
      fixtureByName("mcp-http-stopped").file,
      "startMcp",
      "cognis.startMcpServer",
    );
    await assertButtonResolves(
      page,
      fixtureByName("mcp-http-running").file,
      "stopMcp",
      "cognis.stopMcpServer",
    );
  });

  // R12.4: pause sync ↔ resume sync.
  test("pause ↔ resume sync post the complementary commands", async ({ page }) => {
    const running = manifest.fixtures.find((f) =>
      f.actions.some((a) => a.id === "pauseSync" && !a.disabled),
    );
    expect(running, "expected a fixture that renders an enabled pauseSync button").toBeTruthy();
    await assertButtonResolves(page, running!.file, "pauseSync", "cognis.pauseSync");
    await assertButtonResolves(
      page,
      fixtureByName("sync-paused").file,
      "resumeSync",
      "cognis.resumeSync",
    );
  });

  // R12.5: connect MCP ↔ disconnect MCP.
  test("connect ↔ disconnect MCP post the complementary commands", async ({ page }) => {
    await assertButtonResolves(
      page,
      fixtureByName("ready-not-connected").file,
      "connectMcp",
      "cognis.connectMcp",
    );
    await assertButtonResolves(
      page,
      fixtureByName("mcp-connected-disconnectable").file,
      "disconnectMcp",
      "cognis.disconnectMcp",
    );
  });

  // R12.6: cancel a running rebuild.
  test("cancel indexing posts cognis.cancelIndexing", async ({ page }) => {
    await assertButtonResolves(
      page,
      fixtureByName("indexing-cancelable").file,
      "cancelIndexing",
      "cognis.cancelIndexing",
    );
  });
});

// ---------------------------------------------------------------------------
// Minimal / Advanced surfaces by name (Requirements R8.2, R8.3, R8.4, R8.5, R8.9).
//
// The generic per-fixture loop above already enforces the UI_Contract_Invariant
// (no dead buttons) for *every* fixture — including the six minimal/advanced
// fixtures added in task 8.1. This block adds the explicit, by-name assertions
// the requirements call out: the single Unified_Control + correct label + no
// Advanced_Only_Action leak on Minimal_Surface (R8.2/R8.4), the presence of the
// Advanced_Only_Action buttons on Advanced_Surface (R8.3), and a direct
// re-verification of the UI_Contract_Invariant for both surfaces (R8.5/R8.9).
// ---------------------------------------------------------------------------

// Advanced_Only_Action set — Requirement R8.2. None of these may appear on a
// Minimal_Surface; a leak fails the UI_Contract_Invariant (R8.9).
const ADVANCED_ONLY_ACTIONS = new Set<string>([
  "clearReindex",
  "reinstallEngine",
  "coldRestart",
  "remove",
  "prepareUninstall",
  "startMcp",
  "stopMcp",
  "connectMcp",
  "disconnectMcp",
  "cancelIndexing",
  "refreshPrerequisites",
  "installAllPrerequisites",
  "health",
  "output",
]);

// Minimal fixtures (advancedMode OFF) and the Unified_Control label their
// Cognis_State must produce — R8.4.
const MINIMAL_LABEL_BY_NAME: Record<string, string> = {
  "minimal-off": "Start Cognis",
  "minimal-running": "Pause",
  "minimal-paused": "Resume",
};

// Advanced fixtures (advancedMode ON). Every advanced surface renders the
// danger-zone Advanced_Only_Action buttons regardless of Cognis_State, so this
// representative subset is present on all three (verified against the rendered
// output) — R8.3.
const ADVANCED_FIXTURE_NAMES = ["advanced-off", "advanced-running", "advanced-paused"];
const ADVANCED_EXPECTED_ACTIONS = [
  "clearReindex",
  "reinstallEngine",
  "coldRestart",
  "remove",
  "prepareUninstall",
];

// Collect every rendered data-action on the page (with its itemId, if any).
async function readRenderedActions(
  page: import("@playwright/test").Page,
): Promise<Array<{ id: string; itemId: string | null }>> {
  return page.$$eval("[data-action]", (els) =>
    els.map((e) => ({
      id: e.getAttribute("data-action") ?? "",
      itemId: e.getAttribute("data-item"),
    })),
  );
}

// Direct re-verification of the UI_Contract_Invariant: every rendered
// data-action must resolve to a registered command (or be installPrerequisite
// carrying an itemId). Any unresolved id fails (R8.5/R8.9).
function assertNoDeadButtons(
  actions: Array<{ id: string; itemId: string | null }>,
  where: string,
): void {
  for (const a of actions) {
    if (a.id === "installPrerequisite") {
      expect(a.itemId, `${where}: installPrerequisite must carry an itemId`).toBeTruthy();
    } else {
      expect(
        manifest.commandMap[a.id],
        `${where}: action "${a.id}" must resolve to a registered command`,
      ).toBeTruthy();
    }
  }
}

test.describe("panel minimal vs advanced surfaces", () => {
  for (const [name, expectedLabel] of Object.entries(MINIMAL_LABEL_BY_NAME)) {
    test(`${name}: exactly one Unified_Control, correct label, no Advanced_Only_Action leak`, async ({
      page,
    }) => {
      const fx = fixtureByName(name);
      await page.goto(pageUrl(fx.file));
      await expandAll(page);

      // R8.2 / R1.1: exactly one Unified_Control on the Minimal_Surface.
      expect(
        await page.locator("[data-unified]").count(),
        `${name} must render exactly one Unified_Control`,
      ).toBe(1);

      // R8.4: the Unified_Control label matches the Cognis_State.
      const label = ((await page.locator("[data-unified]").textContent()) ?? "").trim();
      expect(label, `${name} Unified_Control label`).toBe(expectedLabel);

      const actions = await readRenderedActions(page);

      // R8.2 / R8.9: no data-action in the Advanced_Only_Action set may leak
      // onto the Minimal_Surface.
      const leaked = actions.map((a) => a.id).filter((id) => ADVANCED_ONLY_ACTIONS.has(id));
      expect(leaked, `${name} must not leak Advanced_Only_Action(s): ${leaked.join(", ")}`).toEqual(
        [],
      );

      // R8.5: UI_Contract_Invariant — 100% of data-action resolve (no dead buttons).
      assertNoDeadButtons(actions, name);
    });
  }

  for (const name of ADVANCED_FIXTURE_NAMES) {
    test(`${name}: renders the Advanced_Only_Action buttons and has no dead buttons`, async ({
      page,
    }) => {
      const fx = fixtureByName(name);
      await page.goto(pageUrl(fx.file));
      await expandAll(page);

      const actions = await readRenderedActions(page);
      const ids = new Set(actions.map((a) => a.id));

      // R8.3: the Advanced_Surface exposes the Advanced_Only_Action buttons.
      for (const expected of ADVANCED_EXPECTED_ACTIONS) {
        expect(
          ids.has(expected),
          `${name} (advancedMode on) must render Advanced_Only_Action "${expected}"`,
        ).toBe(true);
      }

      // R8.5: UI_Contract_Invariant re-verified on the Advanced_Surface too.
      assertNoDeadButtons(actions, name);
    });
  }
});
