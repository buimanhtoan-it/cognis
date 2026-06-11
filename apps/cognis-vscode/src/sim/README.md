# Panel simulator (Playwright-drivable webview harness)

The extension's UI is a **webview** (`src/panel.ts`). VS Code webviews can't be
clicked by Playwright while running inside the Electron host, but the panel's
HTML is produced by a pure function and its buttons talk to the extension over a
simple `postMessage` bridge. So we render that real HTML **standalone**, stub the
bridge, and let Playwright (or a human, in any browser) click the actual buttons.

## What it does

`npm run sim:build` renders every state in [`fixtures.ts`](./fixtures.ts) through
the production `renderPanelHtml`, then rewrites each page so it runs in a normal
browser:

- strips the VS Code Content-Security-Policy meta;
- injects an `acquireVsCodeApi()` stub that records each posted message to
  `window.__cognisMessages` — the exact `{type:'action', id, itemId?}` the real
  extension receives.

Output lands in `sim-dist/`:

- `index.html` — a launcher linking every state (open it in any browser to click
  around manually);
- `<state>.html` — one standalone page per panel state;
- `manifest.json` — the buttons each page renders + the real action→command map
  (`ACTION_COMMANDS`), used by the test as the contract.

## Run

```bash
npm run sim:build     # build the standalone pages (open sim-dist/index.html)
npm run test:e2e      # build + drive every page with Playwright (headless)
```

First time only: `npx playwright install chromium`.

## What the Playwright spec asserts ([`../../tests-e2e/panel.spec.ts`](../../tests-e2e/panel.spec.ts))

For every fixture state:

1. the page renders exactly the buttons the contract expects (no missing/extra);
2. clicking every **enabled** button posts a `{type:'action'}` message whose id
   maps to a real VS Code command (or `installPrerequisite` carrying an itemId) —
   i.e. **no dead buttons**;
3. **disabled** buttons (e.g. "Set Up for AI" while prerequisites are missing)
   post nothing.

## Extending

Add a state to `fixtures.ts` (any real `PanelContext`) and it is automatically
built into a page and exercised by all three checks — no per-state test code.

## Scope and boundary

This drives the webview **markup + button→command wiring** faithfully. It does
not exercise what the commands *do* on the extension side (that lives in the
`node --test` suite under `src/test/`, which stubs the VS Code API and child
processes), nor native VS Code chrome (status bar, command palette). For driving
the real Electron UI end-to-end, `@vscode/test-electron` (already a dependency)
or `vscode-extension-tester` is the right tool; this simulator is the fast,
CI-friendly layer for the panel UI itself.
