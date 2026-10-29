/**
 * Unit tests for the ``cognis.startCognis`` one-click flow and every
 * Destructive_Action handler in ``extension.ts`` (task 6.5).
 *
 * The handlers under test (``runStartCognis`` and the destructive
 * ``runClearAndReindex`` / ``runReinstallEngine`` / ``runColdRestart`` /
 * ``runRemoveFromWorkspace`` / ``runUninstallEngine``) are module-private, so
 * we can't import them directly. Instead we drive the *real* wiring: this file
 * installs a ``Module._load`` hook that returns a ``vscode`` stub plus inert
 * stubs for every ``extension.ts`` collaborator, then calls the real
 * ``activate()``. The ``vscode`` stub records each ``registerCommand`` handler,
 * so the tests invoke the genuine command handlers (``cognis.startCognis``,
 * ``cognis.clearAndReindex``, …) exactly as the Command Palette / panel would.
 *
 * Collaborator routines (``installManagedBinary``, ``setupWorkspace``,
 * ``startLive``, ``connectMcp``, ``clearIndexAndReindex``,
 * ``removeFromWorkspace``, ``uninstallManagedBinary``, …) are spies that record
 * their call order, so we can assert the one-click sequence, early-stop on
 * failure, and that destructive routines only run behind a modal confirmation.
 *
 * Validates: Requirements 1.5, 5.4, 6.2, 6.3, 6.4, 6.5, 9.2, 21.6.
 */
import Module from "node:module";
import * as path from "node:path";
import assert from "node:assert/strict";
import test, { before, after } from "node:test";
import fc from "fast-check";
import { FIRST_PROBE_COMPATIBILITY_SNAPSHOT } from "../compatibility";

// ---------------------------------------------------------------------------
// Shared mutable test state + call recorder
// ---------------------------------------------------------------------------

interface WarningRecord {
  message: string;
  modal: boolean;
  items: string[];
}

const st = {
  folder: { uri: { fsPath: "D:/fake/repo" }, name: "repo", index: 0 } as
    | { uri: { fsPath: string }; name: string; index: number }
    | undefined,
  configured: false,
  liveIndexing: false,
  engineInstalled: false,
  /** When true, installManagedBinary marks the engine as installed. */
  installSucceeds: true,
  /** When true, setupWorkspace marks the workspace as configured. */
  setupSucceeds: true,
};

/** Ordered log of the collaborator routines invoked by the handlers. */
let calls: string[] = [];
/** Queued responses for window.showWarningMessage, consumed in order. */
let warnResponses: (string | undefined)[] = [];
/** Every warning surfaced to the user (message + whether it was modal). */
let warnings: WarningRecord[] = [];
/** Every informational message surfaced to the user. */
let infos: string[] = [];

function spy(name: string, impl?: (...args: any[]) => any): (...args: any[]) => any {
  return (...args: any[]): any => {
    calls.push(name);
    return impl ? impl(...args) : undefined;
  };
}

// ---------------------------------------------------------------------------
// vscode stub (records registered commands + surfaced messages)
// ---------------------------------------------------------------------------

const registeredCommands = new Map<string, (...args: any[]) => any>();

function parseWarningArgs(rest: any[]): { modal: boolean; items: string[] } {
  let modal = false;
  const items: string[] = [];
  for (const arg of rest) {
    if (arg && typeof arg === "object") {
      modal = arg.modal === true;
    } else if (typeof arg === "string") {
      items.push(arg);
    }
  }
  return { modal, items };
}

const vscodeStub: any = {
  workspace: {
    workspaceFolders: undefined,
    getConfiguration() {
      return {
        get<T>(_key: string, defaultValue?: T): T | undefined {
          return defaultValue;
        },
      };
    },
    getWorkspaceFolder() {
      return st.folder;
    },
    openTextDocument: async () => ({}),
    onDidSaveTextDocument: () => ({ dispose() {} }),
    onDidCreateFiles: () => ({ dispose() {} }),
    onDidDeleteFiles: () => ({ dispose() {} }),
    onDidRenameFiles: () => ({ dispose() {} }),
    onDidChangeConfiguration: () => ({ dispose() {} }),
    onDidChangeWorkspaceFolders: () => ({ dispose() {} }),
  },
  window: {
    createStatusBarItem() {
      return {
        text: "",
        tooltip: "",
        command: "",
        show() {},
        hide() {},
        dispose() {},
      };
    },
    createOutputChannel(name: string) {
      return {
        name,
        append() {},
        appendLine() {},
        show() {},
        clear() {},
        hide() {},
        replace() {},
        dispose() {},
      };
    },
    showInformationMessage(message: string) {
      infos.push(message);
      return Promise.resolve(undefined);
    },
    showWarningMessage(message: string, ...rest: any[]) {
      const { modal, items } = parseWarningArgs(rest);
      warnings.push({ message, modal, items });
      return Promise.resolve(warnResponses.shift());
    },
    showErrorMessage() {
      return Promise.resolve(undefined);
    },
    showTextDocument: async () => undefined,
    withProgress<T>(
      _options: unknown,
      task: (progress: unknown, token: unknown) => Promise<T>
    ) {
      const progress = { report() {} };
      const token = {
        isCancellationRequested: false,
        onCancellationRequested: () => ({ dispose() {} }),
      };
      return task(progress, token);
    },
    registerWebviewViewProvider: () => ({ dispose() {} }),
  },
  commands: {
    registerCommand(command: string, handler: (...args: any[]) => any) {
      registeredCommands.set(command, handler);
      return { dispose() {} };
    },
    executeCommand() {
      return Promise.resolve(undefined);
    },
  },
  Uri: {
    file(fsPath: string) {
      return { fsPath, scheme: "file", path: fsPath };
    },
  },
  ProgressLocation: { Notification: 15 },
  StatusBarAlignment: { Left: 1, Right: 2 },
};

// ---------------------------------------------------------------------------
// Collaborator stubs (only the exports extension.ts touches at runtime)
// ---------------------------------------------------------------------------

class FakeBinaryInstallError extends Error {
  userMessage: string;
  constructor(message: string) {
    super(message);
    this.userMessage = message;
  }
}

const traceStub = {
  init() {},
  setMinLevel() {},
  info() {},
  warn() {},
  error() {},
  debug() {},
  logFilePath: () => undefined,
  span: (_scope: string, _title: string, fn: () => any) => fn(),
};

const outputChannelStub = {
  name: "cognis",
  append() {},
  appendLine() {},
  show() {},
  clear() {},
  hide() {},
  replace() {},
  dispose() {},
};

const stubModules: Record<string, any> = {
  "./cli": { getOutputChannel: () => outputChannelStub },
  "./diagnostics": { trace: traceStub },
  "./binary": {
    initManagedBinary: () => {},
    installManagedBinary: spy("install", async () => {
      if (st.installSucceeds) {
        st.engineInstalled = true;
      }
      return { triple: "x86_64-test", timings: [] as { ms: number }[] };
    }),
    managedBinDir: () => "D:/fake/storage/bin",
    uninstallManagedBinary: spy("uninstallManagedBinary", async () => ({
      removed: true,
    })),
    checkManagedBinaryDrift: () => ({
      outdated: false,
      installed: false,
      expected: undefined,
    }),
    BinaryInstallError: FakeBinaryInstallError,
    formatElapsed: () => "0s",
  },
  "./model": {
    initManagedModel: () => {},
    installManagedModel: async () => {},
    isModelInstalled: () => true,
    managedModelDir: () => "D:/fake/storage/models/bge-small",
    uninstallManagedModel: () => true,
  },
  "./guidance": {
    presentGuidance: async () => {},
    setupResultGuidance: () => undefined,
    showErrorGuidance: spy("showErrorGuidance", async () => {}),
  },
  "./indexd": {
    isLiveIndexing: () => st.liveIndexing,
    onDidChangeIndexStatus: () => ({ dispose() {} }),
    stopAllIndexing: () => {},
  },
  "./mcpServer": {
    onDidChangeMcpServerState: () => ({ dispose() {} }),
    startMcpServer: async () => ({}),
    stopAllMcpServers: async () => {},
    stopMcpServer: async () => {},
  },
  "./panel": {
    CognisPanelProvider: class {
      static viewType = "cognis.controlPanel";
      constructor(..._args: any[]) {}
      updateContext() {}
      reveal() {}
    },
    outcomeLabelForContext: () => "Cognis",
    // The three remediation action ids the notification can dispatch, mirroring
    // the real panel's ACTION_COMMANDS map (Requirement 3.6).
    ACTION_COMMANDS: {
      installBackend: "cognis.installBackend",
      updateExtension: "cognis.updateExtension",
      reinstallEngine: "cognis.reinstallEngine",
    },
    deriveCompatibilityHint: () =>
      "Update needed to keep the Engine and Extension in sync.",
  },
  "./reconcile": { reconcileWorkspaceOnActivate: async () => {} },
  "./handshake": { performHandshake: async () => undefined },
  "./contract": { handshakeWarning: () => undefined },
  "./gitignore": {
    addCognisToGitignore: () => undefined,
    shouldRemindGitignore: () => false,
  },
  "./prerequisites": {
    fetchPrerequisites: async () =>
      st.engineInstalled
        ? { ready: true, combined_install_target: "", items: [] }
        : undefined,
    installAllMissing: () => {},
    installPrerequisite: () => {},
  },
  "./state": {
    getState: () => ({
      liveIndexing: st.liveIndexing,
      mcpEnabled: false,
      syncPaused: false,
      indexStatus: undefined,
      lastHealth: undefined,
      autoManaged: false,
    }),
    initStateStorage: () => {},
    isIndexStatusBusy: () => false,
    setIndexStatus: () => {},
    setLiveIndexing: () => {},
    setMcpEnabled: () => {},
  },
  "./status": {
    deriveStatusSnapshot: () => ({ workspaceStatus: "unknown" }),
    startStatusSurfaceRefresh: () => ({ dispose() {} }),
  },
  "./activation": {
    ActivationManager: class {
      currentPhase = undefined;
      async turnOn() {
        return [];
      }
      beginTransition() {}
      endTransition() {}
      watchCoupling() {
        return { dispose() {} };
      }
      dispose() {}
    },
  },
  "./deactivation": {
    DeactivationManager: class {
      currentPhase = undefined;
      async turnOff() {
        return [];
      }
    },
  },
  "./types": {},
  "./mcpConfig": {
    enableMcpForWorkspace: async () => {},
    writeHttpMcpConfig: () => ({ configPath: "mcp.json" }),
  },
  "./workspace": {
    getWorkspaceFolder: () => st.folder,
    isWorkspaceConfigured: () => st.configured,
    isWorkspaceSyncPaused: () => false,
    refreshPanelContext: async () => ({
      status: "idle",
      compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
      liveIndexing: st.liveIndexing,
      mcpEnabled: false,
      syncPaused: false,
    }),
    rehydrateWorkspaceState: async () => {},
    clearIndexAndReindex: spy("clearIndexAndReindex", async () => ({})),
    connectMcp: spy("connect", async () => {}),
    disableMcp: async () => {},
    pauseSync: async () => {},
    removeFromWorkspace: spy("removeFromWorkspace", async () => ({
      cognisDirRemoved: true,
      purgedConfigPaths: [] as string[],
      mcpRemoved: true,
      configPath: "mcp.json",
    })),
    forceRemoveFromWorkspace: spy("forceRemoveFromWorkspace", async () => ({
      cognisDirRemoved: true,
      purgedConfigPaths: [] as string[],
      mcpRemoved: true,
      configPath: "mcp.json",
      killedPids: [] as number[],
    })),
    repairSetup: async () => ({}),
    resumeSync: async () => {},
    setupWorkspace: spy("setup", async () => {
      if (st.setupSucceeds) {
        st.configured = true;
      }
      return {};
    }),
    showHealthReport: async () => {},
    startLive: spy("index", async () => {
      st.liveIndexing = true;
    }),
    stopLive: async () => {},
  },
};

// ---------------------------------------------------------------------------
// Install the module hook BEFORE requiring the extension under test.
// ---------------------------------------------------------------------------

const moduleApi = Module as unknown as {
  _load: (request: string, parent: unknown, isMain: boolean) => unknown;
};
const originalLoad = moduleApi._load;
moduleApi._load = function (request: string, parent: unknown, isMain: boolean): unknown {
  if (request === "vscode") {
    return vscodeStub;
  }
  if (Object.prototype.hasOwnProperty.call(stubModules, request)) {
    return stubModules[request];
  }
  return originalLoad.call(this, request, parent, isMain);
};

// Now the extension's own `require("./…")` calls resolve to our stubs.
// eslint-disable-next-line @typescript-eslint/no-var-requires
const extension: any = require("../extension");

// ---------------------------------------------------------------------------
// Fake ExtensionContext + activation
// ---------------------------------------------------------------------------

function makeFakeContext(): any {
  return {
    subscriptions: [] as { dispose(): void }[],
    extension: { packageJSON: { version: "0.8.4" } },
    extensionUri: { fsPath: "/fake", scheme: "file", path: "/fake" },
    globalState: { get: () => undefined, update: async () => {} },
    workspaceState: { get: () => undefined, update: async () => {} },
  };
}

async function settle(ticks = 10): Promise<void> {
  for (let i = 0; i < ticks; i++) {
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
}

function getCommand(id: string): (...args: any[]) => any {
  const handler = registeredCommands.get(id);
  assert.ok(handler, `command ${id} was not registered by activate()`);
  return handler;
}

/** Reset per-test state and drive backendAvailable back to "not installed". */
async function resetState(): Promise<void> {
  calls = [];
  warnResponses = [];
  warnings = [];
  infos = [];
  st.configured = false;
  st.liveIndexing = false;
  st.engineInstalled = false;
  st.installSucceeds = true;
  st.setupSucceeds = true;
  // Re-probe prerequisites so the module-private `backendAvailable` flips back
  // to `false` (engine not installed) before each test — the real command
  // handler is the only path that sets it.
  await getCommand("cognis.refreshPrerequisites")();
}

before(async () => {
  extension.activate(makeFakeContext());
  await settle();
});

after(async () => {
  // Clear the health-poll interval so `node --test` can exit cleanly.
  await extension.deactivate();
});

const SEQUENCE = ["install", "setup", "index", "connect"];
const sequenceCalls = (): string[] => calls.filter((c) => SEQUENCE.includes(c));

// ---------------------------------------------------------------------------
// runStartCognis (R1.5, R6.5, R9.2)
// ---------------------------------------------------------------------------

test("runStartCognis runs install → setup → live indexing → connect MCP in order", async () => {
  await resetState();

  await getCommand("cognis.startCognis")();

  // The one-click flow provisions Cognis in the exact order the design mandates
  // (R1.5): ensure engine installed, set up the workspace, start live indexing,
  // then connect MCP.
  assert.deepEqual(sequenceCalls(), ["install", "setup", "index", "connect"]);
  // A fully successful run never surfaces a "not finished" warning.
  assert.equal(warnings.length, 0);
});

test("runStartCognis stops early when the engine install does not complete", async () => {
  await resetState();
  st.installSucceeds = false; // engine never becomes runnable

  await getCommand("cognis.startCognis")();

  // Install was attempted, but the flow halts before touching the workspace:
  // no setup, no indexing, no MCP connect (early-stop on failure — R6.5).
  assert.deepEqual(sequenceCalls(), ["install"]);
  assert.equal(calls.includes("setup"), false);
  assert.equal(calls.includes("index"), false);
  assert.equal(calls.includes("connect"), false);
  // A user-visible "not finished" message is shown (R9.2). No destructive
  // routine ran, so the local .cognis index is untouched.
  assert.ok(warnings.length >= 1, "expected a user-visible 'not finished' warning");
  assert.equal(calls.includes("clearIndexAndReindex"), false);
  assert.equal(calls.includes("removeFromWorkspace"), false);
});

test("runStartCognis stops early when workspace setup does not complete", async () => {
  await resetState();
  st.setupSucceeds = false; // setup runs but never marks the workspace configured

  await getCommand("cognis.startCognis")();

  // Install + setup were attempted; the flow halts before indexing / MCP.
  assert.deepEqual(sequenceCalls(), ["install", "setup"]);
  assert.equal(calls.includes("index"), false);
  assert.equal(calls.includes("connect"), false);
  // User-visible "not finished" notice; nothing destructive happened (R9.2).
  assert.ok(warnings.length >= 1, "expected a user-visible 'not finished' warning");
  assert.equal(calls.includes("removeFromWorkspace"), false);
});

// ---------------------------------------------------------------------------
// Destructive_Action modal confirmation (R6.2, R6.3, R6.4)
// ---------------------------------------------------------------------------

interface DestructiveCase {
  title: string;
  command: string;
  args: any[];
  confirmLabel: string;
  namedTargets: string[];
  /** The underlying destructive routine that must only run after confirm. */
  routine: string;
}

const WORKSPACE_DATA = path.join("D:/fake/repo", ".cognis");
const WORKSPACE_DATABASE = path.join(WORKSPACE_DATA, "uckg.db");

const destructiveCases: DestructiveCase[] = [
  {
    title: "Rebuild Index (clearAndReindex)",
    command: "cognis.clearAndReindex",
    args: [],
    confirmLabel: "Clear & Re-index",
    namedTargets: [WORKSPACE_DATABASE, "capsule cache"],
    routine: "clearIndexAndReindex",
  },
  {
    title: "Remove from Workspace",
    command: "cognis.removeFromWorkspace",
    args: [],
    confirmLabel: "Remove",
    namedTargets: ["live indexing process", WORKSPACE_DATABASE],
    routine: "removeFromWorkspace",
  },
  {
    title: "Remove Everything & Uninstall (prepareUninstall)",
    command: "cognis.prepareUninstall",
    args: [],
    confirmLabel: "Remove Everything",
    namedTargets: [
      "live indexing process",
      WORKSPACE_DATABASE,
      "EVERY Cognis MCP server entry",
      "D:/fake/storage/bin",
      "D:/fake/storage/models/bge-small",
    ],
    routine: "removeFromWorkspace",
  },
  {
    title: "Reinstall Engine",
    command: "cognis.reinstallEngine",
    args: [],
    confirmLabel: "Reinstall Engine",
    namedTargets: ["D:/fake/storage/bin", "D:/fake/storage/models/bge-small"],
    routine: "uninstallManagedBinary",
  },
  {
    title: "Uninstall Engine",
    command: "cognis.uninstallEngine",
    args: [],
    confirmLabel: "Uninstall Engine",
    namedTargets: ["D:/fake/storage/bin", "D:/fake/storage/models/bge-small"],
    routine: "uninstallManagedBinary",
  },
  {
    title: "Cold Restart",
    command: "cognis.coldRestart",
    args: [],
    confirmLabel: "Cold Restart",
    namedTargets: [
      "live indexing process",
      WORKSPACE_DATABASE,
      "ALL Cognis MCP entries",
      "D:/fake/storage/bin",
      "D:/fake/storage/models/bge-small",
    ],
    routine: "removeFromWorkspace",
  },
  {
    title: "Force Cleanup (workspace)",
    command: "cognis.forceCleanup",
    args: [],
    confirmLabel: "Force Clean",
    namedTargets: [
      "force cleanup",
      "indexd/mcpd process trees",
      WORKSPACE_DATABASE,
    ],
    routine: "forceRemoveFromWorkspace",
  },
];

for (const c of destructiveCases) {
  test(`${c.title}: cancelling the modal makes no changes`, async () => {
    await resetState();
    // No queued response → showWarningMessage resolves to undefined (cancel).

    await getCommand(c.command)(...c.args);

    // The confirmation was a modal (R6.2).
    assert.ok(warnings.length >= 1, "expected a confirmation prompt");
    assert.equal(
      warnings[0].modal,
      true,
      "destructive confirmation must be modal ({ modal: true })"
    );
    assert.ok(
      warnings[0].items.includes(c.confirmLabel),
      `confirmation should offer the "${c.confirmLabel}" action`
    );
    for (const target of c.namedTargets) {
      assert.ok(
        warnings[0].message.includes(target),
        `confirmation must name the removed data or processes: ${target}`
      );
    }
    // Cancelling performs no destructive work — the routine never runs, so no
    // source or .cognis files are written/deleted (R6.3, R6.4).
    assert.equal(
      calls.includes(c.routine),
      false,
      `${c.routine} must NOT run when the user cancels`
    );
  });
}

const confirmCases = destructiveCases.filter((c) =>
  // Confirm-path routines that are safe to drive through the stubs.
  ["cognis.clearAndReindex", "cognis.removeFromWorkspace", "cognis.prepareUninstall", "cognis.uninstallEngine", "cognis.forceCleanup"].includes(
    c.command
  )
);

for (const c of confirmCases) {
  test(`${c.title}: confirming the modal runs the destructive routine`, async () => {
    await resetState();
    warnResponses = [c.confirmLabel]; // user confirms

    await getCommand(c.command)(...c.args);

    assert.equal(warnings[0].modal, true, "confirmation must be modal");
    assert.ok(
      calls.includes(c.routine),
      `${c.routine} must run after the user confirms`
    );
  });
}

const arbDestructiveSequence = fc.array(
  fc.record({
    caseIndex: fc.integer({ min: 0, max: destructiveCases.length - 1 }),
    response: fc.oneof(
      fc.constant(undefined),
      fc.constantFrom(
        ...destructiveCases.map((c) => c.confirmLabel),
        "",
        "Cancel",
        "Later",
        "Nope",
        WORKSPACE_DATABASE
      )
    ),
  }),
  { minLength: 1, maxLength: 24 }
);

// Property 32: destructive routines require their own naming confirmation.
// **Validates: Requirements 21.6, 5.4**
test("Property 32: destruction requires a naming confirmation", async () => {
  await fc.assert(
    fc.asyncProperty(arbDestructiveSequence, async (steps) => {
      await resetState();

      for (const [stepIndex, step] of steps.entries()) {
        const c = destructiveCases[step.caseIndex]!;
        const priorCallCount = calls.length;
        const priorWarningCount = warnings.length;
        warnResponses = [step.response];

        await getCommand(c.command)(...c.args);

        const stepWarnings = warnings.slice(priorWarningCount);
        assert.equal(
          stepWarnings.length,
          1,
          `step ${stepIndex}: ${c.command} must surface exactly one destructive confirmation`
        );

        const prompt = stepWarnings[0]!;
        assert.equal(prompt.modal, true, `step ${stepIndex}: destructive confirmation must be modal`);
        assert.ok(
          prompt.items.includes(c.confirmLabel),
          `step ${stepIndex}: ${c.command} must offer its own confirmation label`
        );
        for (const target of c.namedTargets) {
          assert.ok(
            prompt.message.includes(target),
            `step ${stepIndex}: confirmation must name removed data/process ${target}`
          );
        }

        const executed = calls.slice(priorCallCount).includes(c.routine);
        const confirmedThisCommand = step.response === c.confirmLabel;
        assert.equal(
          executed,
          confirmedThisCommand,
          `step ${stepIndex}: ${c.command} must execute only after its own naming confirmation`
        );

        if (executed && c.namedTargets.includes(WORKSPACE_DATABASE)) {
          assert.ok(
            prompt.message.includes(WORKSPACE_DATABASE),
            `step ${stepIndex}: database-destructive confirmations must name ${WORKSPACE_DATABASE}`
          );
        }
      }
    }),
    { numRuns: 300 }
  );
});

test("a confirmation for another command cannot authorize workspace database deletion", async () => {
  await resetState();
  warnResponses = ["Uninstall Engine"];

  await getCommand("cognis.removeFromWorkspace")();

  assert.equal(warnings.length, 1);
  assert.ok(warnings[0].message.includes(WORKSPACE_DATABASE));
  assert.equal(calls.includes("removeFromWorkspace"), false);
});
