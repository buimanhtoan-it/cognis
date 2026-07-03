// Harness first: installs the vscode stub before panel.ts (which imports vscode)
// is required.
import "./testHarness";

import assert from "node:assert/strict";
import test from "node:test";

import { renderPrerequisitesSection, type PanelContext } from "../panel";
import type { PrerequisiteReport } from "../types";

function makeReport(overrides: Partial<PrerequisiteReport> = {}): PrerequisiteReport {
  return {
    ready: true,
    combined_install_target: "",
    items: [
      {
        id: "engine",
        label: "Cognis engine",
        description: "The single self-contained cognis binary.",
        status: "ok",
        required: true,
        install_target: "",
        detail: "cognis (rust)",
      },
      {
        id: "semantic_index",
        label: "Semantic index",
        description: "Symbol embeddings for semantic search.",
        status: "ok",
        required: false,
        install_target: "",
        detail: "vectors present",
      },
    ],
    ...overrides,
  };
}

function ctx(report: PrerequisiteReport | undefined): PanelContext {
  return { status: "notInstalled", prerequisites: report };
}

test("prerequisites section is empty when there is no report", () => {
  assert.equal(renderPrerequisitesSection(ctx(undefined)), "");
});

test("checklist collapses (details closed) when everything required is ready", () => {
  const html = renderPrerequisitesSection(ctx(makeReport({ ready: true })));
  assert.ok(html.includes("<details"), "should use a collapsible <details>");
  // Collapsed: the `open` attribute must NOT be present when ready.
  assert.ok(!/<details[^>]*\sopen/.test(html), "ready checklist must be collapsed");
  assert.ok(html.includes("Ready"), "collapsed summary should say it's ready");
});

test("checklist stays expanded (details open) when a required item is missing", () => {
  const report = makeReport({
    ready: false,
    combined_install_target: ".[indexer]",
    items: [
      {
        id: "indexer",
        label: "Code parsers (tree-sitter)",
        description: "Parses code.",
        status: "missing",
        required: true,
        install_target: ".[indexer]",
        detail: "Not installed: missing tree_sitter",
      },
    ],
  });
  const html = renderPrerequisitesSection(ctx(report));
  assert.ok(/<details[^>]*\sopen/.test(html), "missing prereqs must keep the checklist open");
  assert.ok(html.includes("installAllPrerequisites"), "should offer Install all");
  assert.ok(html.includes("Install the required"),
    "should prompt to install");
});

test("ready summary surfaces available optional extras", () => {
  const report = makeReport({
    ready: true,
    combined_install_target: ".[vector]",
    items: [
      {
        id: "indexer",
        label: "Code parsers",
        description: "Parses code.",
        status: "ok",
        required: true,
        install_target: ".[indexer]",
        detail: "Installed.",
      },
      {
        id: "vector",
        label: "Vector search",
        description: "KNN.",
        status: "missing",
        required: false,
        install_target: ".[vector]",
        detail: "Not installed.",
      },
    ],
  });
  const html = renderPrerequisitesSection(ctx(report));
  assert.ok(!/<details[^>]*\sopen/.test(html), "ready (with only optional missing) stays collapsed");
  assert.ok(html.includes("optional available"), "summary should mention optional extras");
});
