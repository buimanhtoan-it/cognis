/**
 * Minimal `vscode` require-hook for off-host rendering.
 *
 * `panel.ts` does `import * as vscode from "vscode"`, which only resolves
 * inside the VS Code extension host. The panel's *render* path
 * (`renderPanelHtml` / `derivePanelView` / `panelHtml`) never calls into the
 * vscode runtime — it only uses `vscode.Uri` as an erased type — so an empty
 * stub is enough to import the module and generate HTML in plain Node.
 *
 * Importing this module (for its side effect) MUST happen before importing
 * `../panel`.
 */
import Module from "node:module";

const moduleApi = Module as unknown as {
  _load: (request: string, parent: unknown, isMain: boolean) => unknown;
};
const originalLoad = moduleApi._load;
moduleApi._load = function (request: string, parent: unknown, isMain: boolean): unknown {
  if (request === "vscode") {
    return {};
  }
  return originalLoad.call(this, request, parent, isMain);
};
