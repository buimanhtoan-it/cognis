import { defineConfig } from "@playwright/test";

/**
 * Playwright drives the panel *simulator* — standalone HTML pages built by
 * `npm run sim:build` from the real webview render path (see src/sim/). No VS
 * Code instance is needed: the pages run in a normal Chromium with a stubbed
 * `acquireVsCodeApi`, so we can click the actual buttons and assert the command
 * intents they post.
 */
export default defineConfig({
  testDir: "./tests-e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? "github" : "list",
  use: {
    headless: true,
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { browserName: "chromium" } }],
});
