/**
 * Panel simulator builder.
 *
 * Renders every {@link FIXTURES} PanelContext through the production
 * `renderPanelHtml`, then rewrites the markup so it runs in a *plain browser*:
 *
 *   1. strips the VS Code Content-Security-Policy meta (it forbids the inline
 *      bridge stub and is only meaningful inside the webview host);
 *   2. injects an `acquireVsCodeApi()` stub that records every posted message to
 *      `window.__cognisMessages` (the same `{type:'action', id, itemId?}` the
 *      real extension receives), so a test — or a human — can click the actual
 *      buttons and observe the exact command intents.
 *
 * Output: `sim-dist/<name>.html`, an `index.html` launcher, and `manifest.json`
 * describing each page's buttons and the real action→command map. The HTML is
 * openable directly in any browser; Playwright drives the same files in CI.
 */
import "./installVscodeStub";

import * as fs from "node:fs";
import * as path from "node:path";

import { ACTION_COMMANDS, renderPanelHtml } from "../panel";
import { FIXTURES } from "./fixtures";

const APP_ROOT = path.join(__dirname, "..", "..");
const OUT_DIR = path.join(APP_ROOT, "sim-dist");

const BRIDGE_STUB = `
  <script>
    window.__cognisMessages = [];
    window.acquireVsCodeApi = function () {
      return {
        postMessage: function (m) { window.__cognisMessages.push(m); },
        getState: function () { return undefined; },
        setState: function () {},
      };
    };
  </script>`;

interface ButtonAction {
  id: string;
  itemId?: string;
  disabled: boolean;
}

function extractButtons(html: string): ButtonAction[] {
  const actions: ButtonAction[] = [];
  const re = /<button\b([^>]*?)data-action="([^"]+)"([^>]*)>/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(html)) !== null) {
    const attrs = `${m[1]} ${m[3]}`;
    const itemMatch = /data-item="([^"]+)"/.exec(attrs);
    actions.push({
      id: m[2],
      itemId: itemMatch ? itemMatch[1] : undefined,
      disabled: /\bdisabled\b/.test(attrs),
    });
  }
  return actions;
}

function toSimPage(html: string): string {
  const noCsp = html.replace(
    /<meta http-equiv="Content-Security-Policy"[^>]*>\s*/i,
    "",
  );
  return noCsp.replace(/<\/head>/i, `${BRIDGE_STUB}\n</head>`);
}

function main(): void {
  fs.rmSync(OUT_DIR, { recursive: true, force: true });
  fs.mkdirSync(OUT_DIR, { recursive: true });

  const manifest: {
    commandMap: Record<string, string>;
    fixtures: Array<{
      name: string;
      title: string;
      file: string;
      actions: ButtonAction[];
    }>;
  } = { commandMap: ACTION_COMMANDS, fixtures: [] };

  for (const fixture of FIXTURES) {
    const html = renderPanelHtml(fixture.context);
    const page = toSimPage(html);
    const file = `${fixture.name}.html`;
    fs.writeFileSync(path.join(OUT_DIR, file), page, "utf8");
    manifest.fixtures.push({
      name: fixture.name,
      title: fixture.title,
      file,
      actions: extractButtons(html),
    });
  }

  fs.writeFileSync(
    path.join(OUT_DIR, "manifest.json"),
    JSON.stringify(manifest, null, 2),
    "utf8",
  );

  const links = manifest.fixtures
    .map((f) => `<li><a href="./${f.file}">${f.title}</a> <code>(${f.name})</code></li>`)
    .join("\n      ");
  fs.writeFileSync(
    path.join(OUT_DIR, "index.html"),
    `<!DOCTYPE html><html><head><meta charset="utf-8"><title>Cognis panel simulator</title>
<style>body{font-family:system-ui;max-width:680px;margin:40px auto;padding:0 16px}li{margin:8px 0}code{color:#888}</style>
</head><body>
  <h1>Cognis panel simulator</h1>
  <p>Each link renders the real webview panel for one state. Buttons are live;
     clicks are recorded to <code>window.__cognisMessages</code>.</p>
  <ul>
      ${links}
  </ul>
</body></html>`,
    "utf8",
  );

  console.log(`Built ${manifest.fixtures.length} panel pages → ${OUT_DIR}`);
}

main();
