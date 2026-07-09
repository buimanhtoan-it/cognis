/**
 * Unit tests for command-registration resilience, the ``cognis.advancedMode``
 * configuration listener, and lifecycle cleanup on deactivate — the host-side
 * wiring in ``extension.ts`` that a pure webview/render test can't reach
 * (task 9.1).
 *
 * These assertions are the parts of task 9.1 that do NOT need a real VS Code
 * host, so they run in the fast ``node --test`` harness. They complement the
 * full VS Code host test (``src/test-host/configAndCommands.hosttest.ts``),
 * which exercises the *real* Command Palette / config surface. The mapping is
 * documented at the bottom of this file.
 *
 * As with ``startCognisDestructive.test.ts``, the wiring under test is
 * module-private, so we drive the real ``activate()`` / ``deactivate()`` behind
 * a ``Module._load`` hook that swaps in a ``vscode`` stub plus inert stubs for
 * every ``extension.ts`` collaborator. The stub records registered commands,
 * the config-change listener, and the panel contexts pushed on re-render, so we
 * can observe the extension's behaviour without a webview.
 *
 * Validates: Requirements 3.3, 3.4, 7.2, 7.4, 7.5, 9.3.
 */
import Module from "node:module";
import assert from "node:assert/strict";
import test from "node:test";

// ---------------------------------------------------------------------------
// Mutable test state
// ---------------------------------------------------------------------------

const st = {
  folder: { uri: { fsPath: "D:/fake/repo" }, name: "repo", index: 0 } as
    | { uri: { fsPath: string }; name: string; index: number }
    | undefined,
  advancedMode: false as boolean,
};

/** When set, registerCommand throws for this exact id (R7.5 injection). */
let throwForCommand: string | undefined;
/** Ids handed to registerCommand that actually registered (didn't throw). */
let registeredIds: string[] = [];
const registeredCommands = new Map<string, (...args: any[]) => any>();
/** Captured `onDidChangeConfiguration` listeners. */
let configListeners: Array<(e: any) => void> = [];
/** setContext / executeCommand ids observed (to prove "no window reload"). */
let executed: string[] = [];
/** PanelContexts pushed to the panel via updateContext (re-render evidence). */
let panelContexts: any[] = [];
/** Spy flags for the deactivate cleanup routines (R9.3). */
const cleanup = { stopAllIndexing: 0, stopAllMcpServers: 0 };

// ---------------------------------------------------------------------------
// vscode stub
// ---------------------------------------------------------------------------

const vscodeStub: any = {
  workspace: {
    workspaceFolders: undefined,
    getConfiguration() {
      return {
        get<T>(key: string, defaultValue?: T): T | undefined {
          if (key === "advancedMode") {
            return st.advancedMode as unknown as T;
          }
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
    onDidChangeConfiguration(listener: (e: any) => void) {
      configListeners.push(listener);
      return { dispose() {} };
    },
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
    showInformationMessage: () => Promise.resolve(undefined),
    showWarningMessage: () => Promise.resolve(undefined),
    showErrorMessage: () => Promise.resolve(undefined),
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
      if (command === throwForCommand) {
        throw new Error(`simulated registration failure for ${command}`);
      }
      registeredIds.push(command);
      registeredCommands.set(command, handler);
      return { dispose() {} };
    },
    executeCommand(command: string) {
      executed.push(command);
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

class FakeBinaryInstallError extends Error {
  userMessage: string;
  constructor(message: string) {
    super(message);
    this.userMessage = message;
  }
}

const stubModules: Record<string, any> = {
  "./cli": { getOutputChannel: () => outputChannelStub },
  "./diagnostics": { trace: traceStub },
  "./binary": {
    initManagedBinary: () => {},
    installManagedBinary: async () => ({ triple: "x86_64-test", timings: [] }),
    uninstallManagedBinary: async () => ({ removed: true }),
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
    uninstallManagedModel: () => true,
  },
  "./guidance": {
    presentGuidance: async () => {},
    setupResultGuidance: () => undefined,
    showErrorGuidance: async () => {},
  },
  "./indexd": {
    isLiveIndexing: () => false,
    onDidChangeIndexStatus: () => ({ dispose() {} }),
    stopAllIndexing: () => {
      cleanup.stopAllIndexing += 1;
    },
  },
  "./mcpServer": {
    onDidChangeMcpServerState: () => ({ dispose() {} }),
    startMcpServer: async () => ({}),
    stopAllMcpServers: async () => {
      cleanup.stopAllMcpServers += 1;
    },
    stopMcpServer: async () => {},
  },
  "./panel": {
    CognisPanelProvider: class {
      static viewType = "cognis.controlPanel";
      constructor(..._args: any[]) {}
      updateContext(ctx: any) {
        panelContexts.push(ctx);
      }
      reveal() {}
    },
    outcomeLabelForContext: () => "Cognis",
  },
  "./reconcile": { reconcileWorkspaceOnActivate: async () => {} },
  "./handshake": { performHandshake: async () => undefined },
  "./contract": { handshakeWarning: () => undefined },
  "./license": {
    enterLicenseKey: async () => {},
    requireLicense: async () => true,
  },
  "./gitignore": {
    addCognisToGitignore: () => undefined,
    shouldRemindGitignore: () => false,
  },
  "./prerequisites": {
    fetchPrerequisites: async () => ({
      ready: true,
      combined_install_target: "",
      items: [],
    }),
    installAllMissing: () => {},
    installPrerequisite: () => {},
  },
  "./state": {
    deriveStatus: () => "idle",
    getState: () => ({
      liveIndexing: false,
      mcpEnabled: false,
      syncPaused: false,
      indexStatus: undefined,
      lastHealth: { overall: "ok", checks: {} },
      autoManaged: false,
    }),
    initStateStorage: () => {},
    isIndexStatusBusy: () => false,
    setIndexStatus: () => {},
    setLiveIndexing: () => {},
    setMcpEnabled: () => {},
  },
  "./types": {},
  "./mcpConfig": {
    enableMcpForWorkspace: async () => {},
    writeHttpMcpConfig: () => ({ configPath: "mcp.json" }),
  },
  "./workspace": {
    getWorkspaceFolder: () => st.folder,
    isWorkspaceConfigured: () => true,
    isWorkspaceSyncPaused: () => false,
    refreshPanelContext: async () => ({
      status: "idle",
      liveIndexing: false,
      mcpEnabled: false,
      syncPaused: false,
    }),
    rehydrateWorkspaceState: async () => {},
    clearIndexAndReindex: async () => ({}),
    connectMcp: async () => {},
    disableMcp: async () => {},
    pauseSync: async () => {},
    removeFromWorkspace: async () => ({
      cognisDirRemoved: true,
      purgedConfigPaths: [],
      mcpRemoved: true,
      configPath: "mcp.json",
    }),
    repairSetup: async () => ({}),
    resumeSync: async () => {},
    setupWorkspace: async () => ({}),
    showHealthReport: async () => {},
    startLive: async () => {},
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

// eslint-disable-next-line @typescript-eslint/no-var-requires
const extension: any = require("../extension");

// ---------------------------------------------------------------------------
// Helpers
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

async function settle(ticks = 12): Promise<void> {
  for (let i = 0; i < ticks; i++) {
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
}

/**
 * The full set of `cognis.*` command ids the extension registers in
 * `activate()` (task 6.2 group). Mirrors `contributes.commands` in
 * package.json plus the internal `cognis.installPrerequisite`.
 */
const EXPECTED_COMMANDS = [
  "cognis.showOutput",
  "cognis.showDiagnostics",
  "cognis.setupWorkspace",
  "cognis.startCognis",
  "cognis.repairSetup",
  "cognis.clearAndReindex",
  "cognis.connectMcp",
  "cognis.disconnectMcp",
  "cognis.cancelIndexing",
  "cognis.pauseSync",
  "cognis.resumeSync",
  "cognis.enterLicense",
  "cognis.installBackend",
  "cognis.removeFromWorkspace",
  "cognis.prepareUninstall",
  "cognis.reinstallEngine",
  "cognis.uninstallEngine",
  "cognis.coldRestart",
  "cognis.showHealth",
  "cognis.startMcpServer",
  "cognis.stopMcpServer",
  "cognis.openPanel",
  "cognis.refreshPrerequisites",
  "cognis.installPrerequisite",
  "cognis.installAllPrerequisites",
];

/** Reset all per-test recorders and re-activate a fresh extension instance. */
async function freshActivate(): Promise<any> {
  registeredIds = [];
  registeredCommands.clear();
  configListeners = [];
  executed = [];
  panelContexts = [];
  cleanup.stopAllIndexing = 0;
  cleanup.stopAllMcpServers = 0;
  const ctx = makeFakeContext();
  extension.activate(ctx);
  await settle();
  return ctx;
}

// ---------------------------------------------------------------------------
// R7.2 / R7.4: every command registers regardless of advancedMode
// ---------------------------------------------------------------------------

test("every cognis.* command registers when advancedMode is false", async () => {
  st.advancedMode = false;
  await freshActivate();
  for (const id of EXPECTED_COMMANDS) {
    assert.ok(
      registeredCommands.has(id),
      `command ${id} must be registered even with advancedMode=false (R7.2/R7.4)`
    );
  }
  await extension.deactivate();
});

test("every cognis.* command registers when advancedMode is true", async () => {
  st.advancedMode = true;
  await freshActivate();
  for (const id of EXPECTED_COMMANDS) {
    assert.ok(
      registeredCommands.has(id),
      `command ${id} must be registered with advancedMode=true (R7.2/R7.4)`
    );
  }
  st.advancedMode = false;
  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// R7.5: a single failing registration must not abort activation
// ---------------------------------------------------------------------------

test("activation survives one registerCommand throwing; other commands still register", async () => {
  st.advancedMode = false;
  throwForCommand = "cognis.clearAndReindex"; // force exactly one to blow up
  try {
    // activate() must return normally (safeRegister swallows the failure).
    await assert.doesNotReject(async () => {
      await freshActivate();
    });

    // The command that threw is absent…
    assert.equal(
      registeredCommands.has("cognis.clearAndReindex"),
      false,
      "the command whose registration threw should not be registered"
    );
    // …but every other command still registered — activation continued with
    // partial functionality instead of aborting (R7.5).
    for (const id of EXPECTED_COMMANDS) {
      if (id === "cognis.clearAndReindex") {
        continue;
      }
      assert.ok(
        registeredCommands.has(id),
        `command ${id} must still register after another command failed (R7.5)`
      );
    }
  } finally {
    throwForCommand = undefined;
    await extension.deactivate();
  }
});

// ---------------------------------------------------------------------------
// R3.3 / R3.4: toggling advancedMode re-renders promptly, in place, and does
// not touch command registration.
// ---------------------------------------------------------------------------

test("changing cognis.advancedMode re-renders the panel within 2s without a reload", async () => {
  st.advancedMode = false;
  await freshActivate();

  const commandsBefore = new Set(registeredCommands.keys());
  const rendersBefore = panelContexts.length;

  // Flip the setting and fire the real onDidChangeConfiguration listener the
  // extension installed, exactly as VS Code would when the user edits settings.
  st.advancedMode = true;
  assert.ok(configListeners.length >= 1, "extension must install a config listener");

  const start = Date.now();
  for (const listener of configListeners) {
    listener({ affectsConfiguration: (s: string) => s === "cognis.advancedMode" });
  }
  await settle();
  const elapsed = Date.now() - start;

  // R3.3: the panel re-rendered (a new context was pushed) …
  assert.ok(
    panelContexts.length > rendersBefore,
    "toggling advancedMode should push a fresh panel context (re-render)"
  );
  // … the freshest render reflects the new advancedMode value …
  const latest = panelContexts[panelContexts.length - 1];
  assert.equal(
    latest.advancedMode,
    true,
    "re-rendered panel context must carry the new advancedMode value"
  );
  // … promptly (≤2s) …
  assert.ok(elapsed <= 2000, `re-render took ${elapsed}ms, must be ≤2000ms (R3.3)`);
  // … and *in place*: no window reload was requested.
  assert.equal(
    executed.includes("workbench.action.reloadWindow"),
    false,
    "advancedMode toggle must not reload the window (R3.3)"
  );

  // R3.4: visibility toggle never disturbs command registration.
  const commandsAfter = new Set(registeredCommands.keys());
  assert.deepEqual(
    [...commandsAfter].sort(),
    [...commandsBefore].sort(),
    "advancedMode must not add/remove registered commands (R3.4)"
  );

  st.advancedMode = false;
  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// R9.3: deactivate terminates Indexd and the MCP server(s).
// ---------------------------------------------------------------------------

test("deactivate() stops all indexing and all MCP servers", async () => {
  st.advancedMode = false;
  await freshActivate();

  // Isolate the deactivate call from any activation-time cleanup.
  cleanup.stopAllIndexing = 0;
  cleanup.stopAllMcpServers = 0;

  await extension.deactivate();

  assert.ok(
    cleanup.stopAllIndexing >= 1,
    "deactivate must call stopAllIndexing to terminate Indexd (R9.3)"
  );
  assert.ok(
    cleanup.stopAllMcpServers >= 1,
    "deactivate must call stopAllMcpServers to terminate the MCP server (R9.3)"
  );
});
