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
