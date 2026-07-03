/**
 * Mocha entry that runs *inside* the VS Code extension host.
 *
 * `@vscode/test-electron` launches a real VS Code, loads this module as the
 * extension test runner, and awaits `run()`. Unlike the node:test suites under
 * `src/test/` (which stub the `vscode` API and `child_process.spawn`), these
 * tests use the *real* VS Code API against a real Rust engine binary — the only
 * layer that exercises `extension.ts` end to end through the editor host.
 */
import * as fs from "node:fs";
import * as path from "node:path";

import Mocha from "mocha";

export function run(): Promise<void> {
  const mocha = new Mocha({ ui: "tdd", color: true, timeout: 300_000 });
  const here = __dirname;
  for (const file of fs.readdirSync(here)) {
    if (file.endsWith(".hosttest.js")) {
      mocha.addFile(path.join(here, file));
    }
  }
  return new Promise<void>((resolve, reject) => {
    try {
      mocha.run((failures) => {
        if (failures > 0) {
          reject(new Error(`${failures} host test(s) failed`));
        } else {
          resolve();
        }
      });
    } catch (err) {
      reject(err as Error);
    }
  });
}
