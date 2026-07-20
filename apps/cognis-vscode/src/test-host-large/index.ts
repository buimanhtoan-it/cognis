/** Mocha entrypoint for the opt-in large-codebase real-host suite. */
import * as path from "node:path";

import Mocha from "mocha";

export function run(): Promise<void> {
  const mocha = new Mocha({
    ui: "tdd",
    color: true,
    timeout: 900_000,
  });
  mocha.addFile(path.join(__dirname, "largeCodebase.hosttest.js"));

  return new Promise<void>((resolve, reject) => {
    try {
      mocha.run((failures) => {
        if (failures > 0) {
          reject(new Error(`${failures} large host test(s) failed`));
        } else {
          resolve();
        }
      });
    } catch (err) {
      reject(err as Error);
    }
  });
}
