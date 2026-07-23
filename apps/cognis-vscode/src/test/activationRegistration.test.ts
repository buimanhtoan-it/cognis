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
import {
  FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
  compatibilityIdentity,
} from "../compatibility";

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
/**
 * When set, {@link vscode.commands.executeCommand} rejects for this exact id.
 * Used to simulate a failed Update Extension remediation (R4.7 / R5.5): the
 * dispatched command fails, so the extension must keep the Confirmed_Mismatch
 * and record no Dismiss skip key.
 */
let throwForExecuteCommand: string | undefined;
/**
 * Controls what {@link vscode.window.showWarningMessage} resolves to. Defaults
 * to `undefined` (user dismissed). Tests that must click through a modal (e.g.
 * the Reinstall Engine confirmation) swap in a responder that returns the
 * confirming label.
 */
let warningResponder: (message: string, ...args: any[]) => any = () => undefined;
/**
 * Every {@link vscode.window.showWarningMessage} invocation, in order, so tests
 * can inspect the exact button set the notification offered and prove the
 * destructive Repair Engine remediation went through a modal (args carry
 * `{ modal: true }`).
 */
let warningCalls: Array<{ message: string; args: any[] }> = [];
/**
 * Session-scoped `globalState` backing store. `get` returns `undefined` for
 * unset keys (matching the previous inert stub), while `update` records the
 * write so the Dismiss test can prove the per-identity skip key was set without
 * the Panel snapshot ever being cleared.
 */
let globalStateStore = new Map<string, any>();
let globalStateUpdates: Array<[string, any]> = [];
/** Ids handed to registerCommand that actually registered (didn't throw). */
let registeredIds: string[] = [];
const registeredCommands = new Map<string, (...args: any[]) => any>();
/** Captured `onDidChangeConfiguration` listeners. */
let configListeners: Array<(e: any) => void> = [];
/** setContext / executeCommand ids observed (to prove "no window reload"). */
let executed: string[] = [];
/** PanelContexts pushed to the panel via updateContext (re-render evidence). */
let panelContexts: any[] = [];
/** PanelContext references received by the status-bar outcome derivation. */
let statusBarContexts: any[] = [];
/** Event listeners captured from the real activation wiring. */
let indexStatusListeners: Array<(event: any) => void> = [];
let mcpStateListeners: Array<(event: any) => void> = [];
/** `onDidChangeWorkspaceFolders` listeners captured from activation. */
let workspaceFolderListeners: Array<(event: any) => void> = [];

function defaultWorkspaceState(): any {
  return {
    liveIndexing: false,
    mcpEnabled: false,
    syncPaused: false,
    indexStatus: undefined,
    lastHealth: { overall: "ok", checks: {} },
    autoManaged: false,
  };
}

let workspaceState = defaultWorkspaceState();
const defaultPanelContext = {
  status: "idle",
  compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
  liveIndexing: false,
  mcpEnabled: false,
  syncPaused: false,
};
let refreshPanelContextImpl: (repoRoot: string) => Promise<any> = async () => ({
  ...defaultPanelContext,
});
let performHandshakeImpl: (
  repoRoot: string,
  expectedVersion?: string
) => Promise<any> = async () => undefined;
let handshakeCalls: Array<[string, string | undefined]> = [];
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
    onDidChangeWorkspaceFolders(listener: (e: any) => void) {
      workspaceFolderListeners.push(listener);
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
    showWarningMessage: (message: string, ...args: any[]) => {
      warningCalls.push({ message, args });
      return Promise.resolve(warningResponder(message, ...args));
    },
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
      if (command === throwForExecuteCommand) {
        return Promise.reject(
          new Error(`simulated executeCommand failure for ${command}`)
        );
      }
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
    onDidChangeIndexStatus: (listener: (event: any) => void) => {
      indexStatusListeners.push(listener);
      return { dispose() {} };
    },
    stopAllIndexing: () => {
      cleanup.stopAllIndexing += 1;
    },
  },
  "./mcpServer": {
    onDidChangeMcpServerState: (listener: (event: any) => void) => {
      mcpStateListeners.push(listener);
      return { dispose() {} };
    },
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
    outcomeLabelForContext: (context: any) => {
      statusBarContexts.push(context);
      return "Cognis";
    },
    // The real webview action-id → command map. The notification resolves its
    // remediation button command through this exact table so it can never
    // diverge from the Panel's Compatibility_Primary_Action (R3.6).
    ACTION_COMMANDS: {
      startCognis: "cognis.startCognis",
      setup: "cognis.setupWorkspace",
      repair: "cognis.repairSetup",
      clearReindex: "cognis.clearAndReindex",
      connectMcp: "cognis.connectMcp",
      disconnectMcp: "cognis.disconnectMcp",
      startMcp: "cognis.startMcpServer",
      stopMcp: "cognis.stopMcpServer",
      pauseSync: "cognis.pauseSync",
      resumeSync: "cognis.resumeSync",
      cancelIndexing: "cognis.cancelIndexing",
      health: "cognis.showHealth",
      output: "cognis.showOutput",
      refreshPrerequisites: "cognis.refreshPrerequisites",
      installAllPrerequisites: "cognis.installAllPrerequisites",
      installBackend: "cognis.installBackend",
      reinstallEngine: "cognis.reinstallEngine",
      updateExtension: "cognis.updateExtension",
      coldRestart: "cognis.coldRestart",
      remove: "cognis.removeFromWorkspace",
      prepareUninstall: "cognis.prepareUninstall",
      forceCleanup: "cognis.forceCleanup",
    },
    // Backend-free caption; the real derivation is unit-tested in panel tests.
    // Here it only needs to be a non-empty user string so the notification is
    // shown (the caption never contains "Backend").
    deriveCompatibilityHint: () =>
      "An update is needed to keep the Engine and Extension in sync.",
  },
  "./reconcile": { reconcileWorkspaceOnActivate: async () => {} },
  "./handshake": {
    performHandshake: (repoRoot: string, expectedVersion?: string) => {
      handshakeCalls.push([repoRoot, expectedVersion]);
      return performHandshakeImpl(repoRoot, expectedVersion);
    },
  },
  "./contract": { handshakeWarning: () => undefined },
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
    getState: () => workspaceState,
    initStateStorage: () => {},
    isIndexStatusBusy: () => false,
    setIndexStatus: (_repoRoot: string, status: any) => {
      workspaceState.indexStatus = status;
    },
    setLiveIndexing: (_repoRoot: string, active: boolean) => {
      workspaceState.liveIndexing = active;
    },
    setMcpEnabled: (_repoRoot: string, enabled: boolean) => {
      workspaceState.mcpEnabled = enabled;
    },
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
    refreshPanelContext: (repoRoot: string) => refreshPanelContextImpl(repoRoot),
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
    forceRemoveFromWorkspace: async () => ({
      cognisDirRemoved: true,
      purgedConfigPaths: [],
      mcpRemoved: true,
      configPath: "mcp.json",
      killedPids: [],
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
    globalState: {
      get: (key: string) => globalStateStore.get(key),
      update: async (key: string, value: any) => {
        globalStateStore.set(key, value);
        globalStateUpdates.push([key, value]);
      },
    },
    workspaceState: { get: () => undefined, update: async () => {} },
  };
}

async function settle(ticks = 12): Promise<void> {
  for (let i = 0; i < ticks; i++) {
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function handshakeResult(
  engineVersion = "0.8.4",
  compatibility = "ok"
): any {
  return {
    compatibility,
    backendContractVersion: 1,
    expectedContractVersion: 1,
    engineVersion,
    expectedEngineVersion: "0.8.4",
    missingCommands: [],
    missingTools: [],
    usable: true,
  };
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
  "cognis.installBackend",
  "cognis.updateExtension",
  "cognis.removeFromWorkspace",
  "cognis.prepareUninstall",
  "cognis.forceCleanup",
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
async function freshActivate(options?: {
  refresh?: (repoRoot: string) => Promise<any>;
  handshake?: (repoRoot: string, expectedVersion?: string) => Promise<any>;
  /**
   * Notification responder to install *before* activation runs. The reconcile
   * notification fires during ``activate()`` (``void reconcileCompatibility()``),
   * so a test that must click a notification button has to hand its responder
   * in here rather than assigning ``warningResponder`` after the fact.
   */
  responder?: (message: string, ...args: any[]) => any;
  /** Seed the session-scoped globalState store before activation. */
  globalState?: Record<string, any>;
}): Promise<any> {
  registeredIds = [];
  registeredCommands.clear();
  configListeners = [];
  executed = [];
  panelContexts = [];
  statusBarContexts = [];
  indexStatusListeners = [];
  mcpStateListeners = [];
  workspaceFolderListeners = [];
  workspaceState = defaultWorkspaceState();
  refreshPanelContextImpl = options?.refresh ?? (async () => ({ ...defaultPanelContext }));
  performHandshakeImpl = options?.handshake ?? (async () => undefined);
  handshakeCalls = [];
  warningResponder = options?.responder ?? (() => undefined);
  warningCalls = [];
  globalStateStore = new Map<string, any>(
    Object.entries(options?.globalState ?? {})
  );
  globalStateUpdates = [];
  cleanup.stopAllIndexing = 0;
  cleanup.stopAllMcpServers = 0;
  throwForExecuteCommand = undefined;
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
// State publication regressions: latest health poll wins, and an index-status
// event preserves fields owned by the last full panel snapshot.
// ---------------------------------------------------------------------------

test("an older health poll cannot overwrite a newer completed poll", async () => {
  st.advancedMode = false;
  await freshActivate();

  const first = deferred<any>();
  const second = deferred<any>();
  const pending = [first, second];
  refreshPanelContextImpl = async () => {
    const next = pending.shift();
    assert.ok(next, "each MCP event should start exactly one health poll");
    return next.promise;
  };

  assert.equal(mcpStateListeners.length, 1, "extension must install one MCP state listener");
  const event = { repoRoot: st.folder!.uri.fsPath };
  mcpStateListeners[0](event);
  mcpStateListeners[0](event);
  await settle(1);

  const fresh = {
    status: "mcpEnabled",
    compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
    liveIndexing: true,
    mcpEnabled: true,
    syncPaused: false,
    version: "new",
  };
  second.resolve(fresh);
  await settle();
  const rendersAfterFreshPoll = panelContexts.length;
  assert.equal(panelContexts.at(-1)?.version, "new");

  first.resolve({ ...fresh, version: "stale", mcpEnabled: false });
  await settle();
  assert.equal(
    panelContexts.length,
    rendersAfterFreshPoll,
    "the stale poll must not publish another panel context"
  );
  assert.equal(panelContexts.at(-1)?.version, "new");

  await extension.deactivate();
});

test("index status updates preserve the last full panel snapshot", async () => {
  st.advancedMode = false;
  const richContext = {
    status: "mcpEnabled",
    compatibility: FIRST_PROBE_COMPATIBILITY_SNAPSHOT,
    health: { overall: "ok", runtime_version: "0.8.4", checks: {} },
    liveIndexing: true,
    mcpEnabled: true,
    syncPaused: false,
    mcpServerPhase: "error",
    mcpServerUrl: "http://127.0.0.1:50001/mcp",
    mcpServerName: "cognis-repo-ab12cd",
    mcpServerError: "retained diagnostic",
    mcpConfigPath: "D:/fake/repo/.vscode/mcp.json",
    mcpRuntimeCount: 1,
    mcpRuntimeRepoScoped: true,
    version: "0.8.4",
  };
  await freshActivate();

  // Publish one authoritative rich snapshot through the real MCP-event
  // health-poll path before emitting the narrower index-status event.
  refreshPanelContextImpl = async () => ({ ...richContext });
  assert.equal(mcpStateListeners.length, 1, "extension must install one MCP state listener");
  mcpStateListeners[0]({ repoRoot: st.folder!.uri.fsPath });
  await settle();

  const preserved = panelContexts.at(-1);
  assert.equal(preserved?.mcpServerError, "retained diagnostic");
  const status = {
    active: false,
    phase: "idle",
    message: "Watching for changes",
    pendingCount: 0,
    pendingFiles: [],
    inflightCount: 0,
    inflightFiles: [],
    recentFiles: ["src/updated.ts"],
    updatedAt: 42,
  };
  assert.equal(indexStatusListeners.length, 1, "extension must install one index listener");
  indexStatusListeners[0]({ repoRoot: st.folder!.uri.fsPath, status });
  await settle();

  const latest = panelContexts.at(-1);
  for (const field of [
    "compatibility",
    "health",
    "mcpEnabled",
    "mcpServerPhase",
    "mcpServerUrl",
    "mcpServerName",
    "mcpServerError",
    "mcpConfigPath",
    "mcpRuntimeCount",
    "mcpRuntimeRepoScoped",
    "configured",
    "backendAvailable",
    "prerequisites",
    "version",
  ]) {
    assert.deepEqual(latest?.[field], preserved?.[field], `${field} must be preserved`);
  }
  assert.equal(latest?.liveIndexing, false);
  assert.deepEqual(latest?.indexStatus, status);

  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// Compatibility publication regressions: one coordinator-backed immutable
// context is shared by Status Bar + Panel, and stale work never publishes.
// ---------------------------------------------------------------------------

test("health poll publishes one frozen context reference with coordinator compatibility", async () => {
  st.advancedMode = false;
  const healthContext = {
    ...defaultPanelContext,
    status: "mcpEnabled",
    health: { overall: "ok", runtime_version: "0.8.4", checks: {} },
    liveIndexing: true,
    mcpEnabled: true,
    version: "health-snapshot",
  };
  const mismatch = handshakeResult("0.8.3", "engine-outdated");
  await freshActivate({
    refresh: async () => healthContext,
    handshake: async () => mismatch,
  });

  const rendersBefore = panelContexts.length;
  const outcomesBefore = statusBarContexts.length;
  mcpStateListeners[0]({ repoRoot: st.folder!.uri.fsPath });
  await settle();

  assert.equal(handshakeCalls.length, 1, "polling must use one coordinator probe");
  assert.deepEqual(handshakeCalls[0], [st.folder!.uri.fsPath, "0.8.4"]);
  assert.equal(panelContexts.length, rendersBefore + 1);
  assert.equal(statusBarContexts.length, outcomesBefore + 1);
  const published = panelContexts.at(-1);
  assert.strictEqual(
    statusBarContexts.at(-1),
    published,
    "Status Bar and Panel must receive the exact same context object"
  );
  assert.ok(Object.isFrozen(published), "published PanelContext must be immutable");
  assert.equal(published.health, healthContext.health);
  assert.equal(published.compatibility.phase, "confirmed");
  assert.strictEqual(published.compatibility.result, mismatch);

  await extension.deactivate();
});

test("workspace change while health and compatibility are pending cannot publish stale context", async () => {
  st.advancedMode = false;
  const health = deferred<any>();
  const handshake = deferred<any>();
  await freshActivate({
    refresh: async (repoRoot) => ({
      ...(await health.promise),
      version: repoRoot,
    }),
    handshake: async () => handshake.promise,
  });

  const oldRoot = st.folder!.uri.fsPath;
  mcpStateListeners[0]({ repoRoot: oldRoot });
  await settle(1);
  st.folder = { uri: { fsPath: "D:/fake/other-repo" }, name: "other", index: 0 };
  health.resolve({ ...defaultPanelContext });
  handshake.resolve(handshakeResult());
  await settle();

  assert.equal(
    panelContexts.some((context) => context.version === oldRoot),
    false,
    "a poll for a workspace that is no longer current must not publish its old-root snapshot"
  );

  await extension.deactivate();
  st.folder = { uri: { fsPath: "D:/fake/repo" }, name: "repo", index: 0 };
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

// ---------------------------------------------------------------------------
// R2.5 / R5.5: a successful install forces a fresh compatibility probe so the
// panel reflects the just-installed engine version instead of the cached
// pre-install verdict.
// ---------------------------------------------------------------------------

test("a successful backend install forces a fresh (TTL-bypassing) compatibility probe", async () => {
  st.advancedMode = false;
  // First probe reports an outdated engine; the second (post-install) reports ok.
  const verdicts = [
    handshakeResult("0.8.3", "engine-outdated"),
    handshakeResult("0.8.4", "ok"),
  ];
  let probeIndex = 0;
  await freshActivate({
    handshake: async () => verdicts[Math.min(probeIndex++, verdicts.length - 1)],
  });

  const probesAfterActivation = handshakeCalls.length;
  assert.ok(
    probesAfterActivation >= 1,
    "activation must run at least one compatibility probe"
  );

  const runInstall = registeredCommands.get("cognis.installBackend");
  assert.ok(runInstall, "cognis.installBackend must be registered");
  await runInstall();
  await settle();

  // The install path must trigger a *new* probe rather than serve the cached
  // (≤30s) snapshot claimed during activation — proving the forced re-probe.
  assert.ok(
    handshakeCalls.length > probesAfterActivation,
    "install must force a fresh compatibility probe (R2.5/R5.5)"
  );
  // The freshest published verdict reflects the post-install engine (ok), not
  // the stale pre-install mismatch.
  const published = panelContexts.at(-1);
  assert.equal(published?.compatibility.phase, "confirmed");
  assert.equal(published?.compatibility.result.compatibility, "ok");

  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// R2.5 / R5.5: Reinstall Engine flows through the same install routine, so it
// too must end in a forced re-probe once the modal is confirmed.
// ---------------------------------------------------------------------------

test("a confirmed engine reinstall forces a fresh compatibility probe", async () => {
  st.advancedMode = false;
  const verdicts = [
    handshakeResult("0.8.3", "engine-outdated"),
    handshakeResult("0.8.4", "ok"),
  ];
  let probeIndex = 0;
  await freshActivate({
    handshake: async () => verdicts[Math.min(probeIndex++, verdicts.length - 1)],
  });
  // Confirm the destructive reinstall modal by clicking its label.
  warningResponder = (_message: string, ...labels: any[]) =>
    labels.includes("Reinstall Engine") ? "Reinstall Engine" : undefined;

  const probesAfterActivation = handshakeCalls.length;
  const runReinstall = registeredCommands.get("cognis.reinstallEngine");
  assert.ok(runReinstall, "cognis.reinstallEngine must be registered");
  await runReinstall();
  await settle();

  assert.ok(
    handshakeCalls.length > probesAfterActivation,
    "reinstall must force a fresh compatibility probe (R2.5/R5.5)"
  );
  const published = panelContexts.at(-1);
  assert.equal(published?.compatibility.phase, "confirmed");
  assert.equal(published?.compatibility.result.compatibility, "ok");

  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// R2.7 / R5.5: a workspace switch evicts the closed root from the coordinator
// (so its snapshot can never be reused) and forces a fresh probe for the new
// root.
// ---------------------------------------------------------------------------

test("workspace switch evicts the removed root and force-reprobes the new root", async () => {
  st.advancedMode = false;
  await freshActivate({ handshake: async () => handshakeResult() });
  assert.equal(
    workspaceFolderListeners.length,
    1,
    "extension must install one onDidChangeWorkspaceFolders listener"
  );

  const oldRoot = st.folder!.uri.fsPath;
  const probesBefore = handshakeCalls.length;

  // Simulate closing the old root and opening a new one as the current folder.
  const newRoot = "D:/fake/switched-repo";
  st.folder = { uri: { fsPath: newRoot }, name: "switched", index: 0 };
  workspaceFolderListeners[0]({
    added: [{ uri: { fsPath: newRoot }, name: "switched", index: 0 }],
    removed: [{ uri: { fsPath: oldRoot }, name: "repo", index: 0 }],
  });
  await settle();

  // A forced probe ran for the *new* root (the eviction of the old root leaves
  // no cached snapshot to reuse).
  assert.ok(
    handshakeCalls.length > probesBefore,
    "a workspace switch must force a fresh probe for the new root (R2.7/R5.5)"
  );
  assert.equal(
    handshakeCalls.at(-1)?.[0],
    newRoot,
    "the forced probe must target the new workspace root, not the closed one"
  );

  await extension.deactivate();
  st.folder = { uri: { fsPath: "D:/fake/repo" }, name: "repo", index: 0 };
});

// ---------------------------------------------------------------------------
// R2.8 / R5.5: a config/visibility-only re-render is pure — it must not spawn
// a new compatibility probe (only the coordinator triggers CLI handshakes).
// ---------------------------------------------------------------------------

test("a config/visibility-only re-render does not start a new compatibility probe", async () => {
  st.advancedMode = false;
  await freshActivate({ handshake: async () => handshakeResult() });

  const probesBefore = handshakeCalls.length;
  const rendersBefore = panelContexts.length;

  // Flip advancedMode and fire the real config listener — a pure re-render.
  st.advancedMode = true;
  assert.ok(configListeners.length >= 1, "extension must install a config listener");
  for (const listener of configListeners) {
    listener({ affectsConfiguration: (s: string) => s === "cognis.advancedMode" });
  }
  await settle();

  // The panel re-rendered in place …
  assert.ok(
    panelContexts.length > rendersBefore,
    "advancedMode toggle should re-render the panel"
  );
  // … but NO new handshake/probe was spawned by the pure render path (R2.8).
  assert.equal(
    handshakeCalls.length,
    probesBefore,
    "a pure config/visibility re-render must not spawn a compatibility probe (R2.8)"
  );

  st.advancedMode = false;
  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// Task 3.4 (R1.2–1.5, R2.1–2.8, R5.5): the Panel and Status Bar must never
// receive compatibility from two independent probes or from a different
// workspace root. These lock the single-publish-funnel invariant: in every
// publish cycle both surfaces receive the *exact same* committed context
// object, whose compatibility comes from the one coordinator probe for the
// current canonical root only.
// ---------------------------------------------------------------------------

/**
 * Assert that Status Bar and Panel have observed identical context references
 * in lockstep across the whole run — i.e. neither surface ever saw a context
 * the other did not, and both always agreed on `compatibility`. This is the
 * core "never split across two surfaces" invariant.
 */
function assertSurfacesNeverSplit(): void {
  assert.equal(
    statusBarContexts.length,
    panelContexts.length,
    "Status Bar and Panel must publish the same number of contexts (no surface is ever fed independently)"
  );
  for (let i = 0; i < panelContexts.length; i++) {
    assert.strictEqual(
      statusBarContexts[i],
      panelContexts[i],
      `publish #${i}: Status Bar and Panel must receive the exact same context object reference`
    );
    assert.strictEqual(
      statusBarContexts[i].compatibility,
      panelContexts[i].compatibility,
      `publish #${i}: both surfaces must share one committed compatibility snapshot`
    );
  }
}

test("one publish cycle feeds Status Bar and Panel the same coordinator snapshot from a single probe", async () => {
  st.advancedMode = false;
  const healthContext = {
    ...defaultPanelContext,
    status: "mcpEnabled",
    health: { overall: "ok", runtime_version: "0.8.4", checks: {} },
    liveIndexing: true,
    mcpEnabled: true,
    version: "cycle-health",
  };
  const mismatch = handshakeResult("0.8.3", "engine-outdated");
  await freshActivate({
    refresh: async () => healthContext,
    handshake: async () => mismatch,
  });

  const rendersBefore = panelContexts.length;
  const outcomesBefore = statusBarContexts.length;
  // Activation already warmed the coordinator with exactly one probe; that one
  // committed snapshot is the single source both surfaces draw from.
  const probesBefore = handshakeCalls.length;
  assert.ok(
    probesBefore >= 1,
    "activation must have run at least one coordinator probe"
  );

  mcpStateListeners[0]({ repoRoot: st.folder!.uri.fsPath });
  await settle();

  // No SECOND independent probe backed this cycle: the coordinator serves its
  // one committed (≤30s) snapshot to both surfaces rather than spawning a
  // parallel performHandshake for Status Bar vs Panel — R2.1/R2.3/R2.4.
  assert.equal(
    handshakeCalls.length,
    probesBefore,
    "the publish cycle must reuse the one coordinator snapshot, never a second independent probe"
  );
  // One publish each — the single funnel published once for both surfaces.
  assert.equal(panelContexts.length, rendersBefore + 1);
  assert.equal(statusBarContexts.length, outcomesBefore + 1);

  const published = panelContexts.at(-1);
  // Same object reference to both surfaces (R1.3) …
  assert.strictEqual(
    statusBarContexts.at(-1),
    published,
    "Status Bar and Panel must receive the exact same context object"
  );
  assert.ok(Object.isFrozen(published), "the published context must be immutable");
  // … and its compatibility is the coordinator's committed snapshot, carrying
  // the same HandshakeResult the probe returned by reference (R1.2).
  assert.equal(published.compatibility.phase, "confirmed");
  assert.strictEqual(published.compatibility.result, mismatch);

  assertSurfacesNeverSplit();

  await extension.deactivate();
});

test("a probe for a root that is no longer current reaches neither Status Bar nor Panel", async () => {
  st.advancedMode = false;
  const health = deferred<any>();
  const handshake = deferred<any>();
  await freshActivate({
    refresh: async (repoRoot) => ({
      ...(await health.promise),
      version: repoRoot,
    }),
    handshake: async () => handshake.promise,
  });

  const oldRoot = st.folder!.uri.fsPath;
  // Start a poll cycle for the old root, then switch the current workspace root
  // out from under it before its health + compatibility work resolves.
  mcpStateListeners[0]({ repoRoot: oldRoot });
  await settle(1);
  const newRoot = "D:/fake/other-repo";
  st.folder = { uri: { fsPath: newRoot }, name: "other", index: 0 };
  health.resolve({ ...defaultPanelContext });
  handshake.resolve(handshakeResult("0.8.3", "engine-outdated"));
  await settle();

  // The stale-root snapshot reached NEITHER surface (R2.6/R2.7): the canonical
  // root re-check in pollHealth drops the publish for the no-longer-current
  // root, so its old-root context never lands on Status Bar or Panel.
  assert.equal(
    panelContexts.some((context) => context.version === oldRoot),
    false,
    "Panel must not receive a snapshot published for a root that is no longer current"
  );
  assert.equal(
    statusBarContexts.some((context) => context.version === oldRoot),
    false,
    "Status Bar must not receive a snapshot published for a root that is no longer current"
  );
  // Whatever did publish, the two surfaces still agreed in every cycle.
  assertSurfacesNeverSplit();

  await extension.deactivate();
  st.folder = { uri: { fsPath: "D:/fake/repo" }, name: "repo", index: 0 };
});

test("two overlapping probes never split compatibility across the two surfaces", async () => {
  st.advancedMode = false;
  // Warm the coordinator during activation with one committed mismatch probe.
  const mismatch = handshakeResult("0.8.3", "engine-outdated");
  await freshActivate({ handshake: async () => mismatch });

  // Now drive two overlapping poll cycles for the same root, each awaiting its
  // own health context. Assigning the queue *after* activation avoids draining
  // it with activation's own polls (mirrors the older-poll regression above).
  const first = deferred<any>();
  const second = deferred<any>();
  const pending = [first, second];
  refreshPanelContextImpl = async () => {
    const next = pending.shift();
    assert.ok(next, "each overlapping poll should start exactly one health fetch");
    return next.promise;
  };

  const probesBefore = handshakeCalls.length;

  // Fire two overlapping poll cycles for the current root.
  const root = st.folder!.uri.fsPath;
  mcpStateListeners[0]({ repoRoot: root });
  mcpStateListeners[0]({ repoRoot: root });
  await settle(1);

  // Resolve the newer poll first, then the older one — the older result must
  // not publish after the newer (latest-wins), and must never split a surface.
  second.resolve({
    ...defaultPanelContext,
    status: "mcpEnabled",
    version: "newer",
  });
  await settle();
  const rendersAfterNewer = panelContexts.length;
  first.resolve({
    ...defaultPanelContext,
    status: "idle",
    version: "older",
  });
  await settle();

  // No SECOND independent probe: within the ≤30s TTL both overlapping cycles
  // reuse the one committed coordinator snapshot instead of spawning a parallel
  // performHandshake per surface (R2.1/R2.3/R2.4).
  assert.equal(
    handshakeCalls.length,
    probesBefore,
    "overlapping polls for one root must reuse the single committed coordinator snapshot"
  );
  // The stale (older) poll did not publish after the newer one.
  assert.equal(
    panelContexts.length,
    rendersAfterNewer,
    "the older overlapping poll must not publish after the newer one"
  );
  assert.equal(panelContexts.at(-1)?.version, "newer");
  // Both surfaces agreed on the same object/compatibility in every cycle.
  assertSurfacesNeverSplit();
  assert.strictEqual(
    statusBarContexts.at(-1),
    panelContexts.at(-1),
    "the surviving cycle fed both surfaces the same context object"
  );
  assert.equal(panelContexts.at(-1)?.compatibility.phase, "confirmed");
  assert.strictEqual(panelContexts.at(-1)?.compatibility.result, mismatch);

  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// Task 5.2 (R3.6, R4.3, R4.4, R6.1): the reconcile notification is a mirror of
// the Panel's Compatibility_Primary_Action. Its single remediation button MUST
// match the remediation derived for the current Compatibility_Kind (label +
// command), it MUST also offer Show Diagnostics and Dismiss, Dismiss MUST only
// record the per-identity skip (never clearing the committed verdict), and the
// destructive Repair Engine remediation MUST route through the modal.
// ---------------------------------------------------------------------------

/**
 * The reconcile-mismatch notification is the one `showWarningMessage` call that
 * offers the "Show Diagnostics" action. The destructive Reinstall Engine modal
 * (`{ modal: true }` + "Reinstall Engine") is deliberately excluded so tests
 * can isolate the notification from the confirmation dialog it may open.
 */
function notificationCalls(): Array<{ message: string; args: any[] }> {
  return warningCalls.filter((c) => c.args.includes("Show Diagnostics"));
}

/** Every modal `showWarningMessage` call (args carry `{ modal: true }`). */
function modalWarningCalls(): Array<{ message: string; args: any[] }> {
  return warningCalls.filter((c) =>
    c.args.some((a) => a && typeof a === "object" && a.modal === true)
  );
}

/**
 * The notification's remediation button (label + the command it dispatches to)
 * must match the Panel's Compatibility_Primary_Action for the current kind
 * (R3.6). The button set is always {remediation label, Show Diagnostics,
 * Dismiss}, in that order, and no visible text says "Backend" (R6.1).
 */
const NOTIFICATION_BUTTON_CASES: Array<{
  kind: string;
  engineVersion: string;
  expectedLabel: string;
}> = [
  { kind: "engine-outdated", engineVersion: "0.8.3", expectedLabel: "Update Engine" },
  { kind: "backend-older", engineVersion: "0.8.4", expectedLabel: "Update Engine" },
  { kind: "capabilities-missing", engineVersion: "0.8.4", expectedLabel: "Update Engine" },
  { kind: "engine-newer", engineVersion: "0.8.5", expectedLabel: "Update Extension" },
  { kind: "backend-newer", engineVersion: "0.8.4", expectedLabel: "Update Extension" },
  { kind: "unreadable", engineVersion: "0.8.4", expectedLabel: "Repair Engine" },
];

for (const { kind, engineVersion, expectedLabel } of NOTIFICATION_BUTTON_CASES) {
  test(`notification for ${kind} offers [${expectedLabel}, Show Diagnostics, Dismiss] and never says "Backend"`, async () => {
    st.advancedMode = false;
    await freshActivate({
      handshake: async () => handshakeResult(engineVersion, kind),
    });

    const notes = notificationCalls();
    assert.equal(
      notes.length,
      1,
      `exactly one reconcile notification must be shown for ${kind}`
    );
    const [note] = notes;
    // The remediation button is the first action and matches the Panel's
    // Compatibility_Primary_Action label for this kind (R3.6).
    assert.equal(
      note.args[0],
      expectedLabel,
      `${kind} remediation button label must be "${expectedLabel}"`
    );
    // …followed by Show Diagnostics and Dismiss (R4.3).
    assert.ok(note.args.includes("Show Diagnostics"), "must offer Show Diagnostics");
    assert.ok(note.args.includes("Dismiss"), "must offer Dismiss");
    // No user-visible string (message or any button) contains "Backend" (R6.1).
    const visible = [note.message, ...note.args.filter((a) => typeof a === "string")];
    for (const text of visible) {
      assert.ok(
        !/backend/i.test(text),
        `visible notification text must not contain "Backend": ${JSON.stringify(text)}`
      );
    }

    await extension.deactivate();
  });
}

test("clicking Update Extension dispatches the cognis.updateExtension command (R3.6)", async () => {
  st.advancedMode = false;
  // Install the responder BEFORE activation, since the reconcile notification
  // fires during activate() — the responder clicks the Update Extension button.
  await freshActivate({
    handshake: async () => handshakeResult("0.8.5", "engine-newer"),
    responder: (_message: string, ...labels: any[]) =>
      labels.includes("Update Extension") ? "Update Extension" : undefined,
  });

  // Update Extension is dispatched through executeCommand using the id from the
  // shared ACTION_COMMANDS table — the notification never invents its own id.
  assert.ok(
    executed.includes("cognis.updateExtension"),
    "clicking Update Extension must execute cognis.updateExtension (R3.6)"
  );
  // A non-destructive update never opens the destructive Reinstall modal.
  assert.equal(
    modalWarningCalls().length,
    0,
    "Update Extension must not open the destructive Reinstall Engine modal"
  );

  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// Task 5.3 (R3.6, R4.6): a SUCCESSFUL remediation of any kind forces a fresh
// re-probe within ≤5s; when it returns `ok`, the warning clears and the Panel
// drops back to the operational control. Update Extension has no in-process
// install routine to piggy-back on, so its re-probe wiring is the new path
// under test here (Update Engine / Repair Engine already prove their re-probe
// via runInstallBinaryBackend above).
// ---------------------------------------------------------------------------

test("a successful Update Extension forces a fresh re-probe and returns the Panel to operational control (R4.6)", async () => {
  st.advancedMode = false;
  // First probe reports a version skew (engine newer than the extension); the
  // second (post-update) probe reports ok, modelling the editor having applied
  // the extension update so the versions now match.
  const verdicts = [
    handshakeResult("0.8.5", "engine-newer"),
    handshakeResult("0.8.5", "ok"),
  ];
  let probeIndex = 0;
  await freshActivate({
    handshake: async () => verdicts[Math.min(probeIndex++, verdicts.length - 1)],
    // Click the notification's Update Extension remediation button.
    responder: (_message: string, ...labels: any[]) =>
      labels.includes("Update Extension") ? "Update Extension" : undefined,
  });
  // Let the fire-and-forget reconcile notification click, dispatch, forced
  // re-probe, and republish settle fully before asserting.
  await settle();

  // The dispatched command ran…
  assert.ok(
    executed.includes("cognis.updateExtension"),
    "Update Extension must dispatch cognis.updateExtension"
  );
  // …and it forced a *new* probe. Activation warms the coordinator with the
  // first (engine-newer) verdict; the cache stays fresh for 30s, so the ONLY
  // way the second (ok) verdict is ever obtained is a forced, TTL-bypassing
  // re-probe. Two distinct probes therefore prove the re-probe fired (R4.6).
  assert.ok(
    handshakeCalls.length >= 2,
    "a successful Update Extension must force a fresh compatibility re-probe (R4.6)"
  );

  // The freshest published verdict is ok, so the warning cleared and the Panel
  // returns to the operational control (no Confirmed_Mismatch remains). This
  // could not happen without the forced re-probe: the cached verdict is still
  // engine-newer within its TTL.
  const published = panelContexts.at(-1);
  assert.equal(published?.compatibility.phase, "confirmed");
  assert.equal(
    published?.compatibility.result.compatibility,
    "ok",
    "a return to ok must clear the mismatch on both surfaces (R4.6)"
  );
  // Both surfaces drew from the same committed snapshot in the final publish.
  assert.equal(
    statusBarContexts.at(-1),
    published,
    "Panel and Status Bar must publish the same ok verdict context"
  );

  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// Task 5.3 (R4.7 / R5.5): a FAILED/CANCELLED remediation keeps the mismatch and
// records NO Dismiss skip key, so a later attempt still prompts. A failed
// Update Extension surfaces an error but never marks the identity dismissed.
// ---------------------------------------------------------------------------

test("a failed Update Extension keeps the mismatch and records no Dismiss skip (R4.7/R5.5)", async () => {
  st.advancedMode = false;
  // Make the dispatched update command fail.
  throwForExecuteCommand = "cognis.updateExtension";
  try {
    await freshActivate({
      handshake: async () => handshakeResult("0.8.5", "engine-newer"),
      responder: (_message: string, ...labels: any[]) =>
        labels.includes("Update Extension") ? "Update Extension" : undefined,
    });

    // The command was attempted (and rejected).
    assert.ok(
      executed.includes("cognis.updateExtension"),
      "the update command must be attempted before it fails"
    );

    // A failed remediation records NO per-identity Dismiss skip key — only the
    // explicit Dismiss button writes one — so a later attempt still prompts.
    const skips = globalStateUpdates.filter(
      ([key]) => key.startsWith("cognis.skipHandshakeWarning.")
    );
    assert.equal(
      skips.length,
      0,
      "a failed Update Extension must not record a Dismiss skip key (R4.7)"
    );

    // The committed verdict is retained: no probe returned ok, so the Panel's
    // last published compatibility is still the confirmed mismatch.
    const confirmedContexts = panelContexts.filter(
      (c) => c.compatibility?.phase === "confirmed"
    );
    const lastConfirmed = confirmedContexts.at(-1);
    assert.ok(lastConfirmed, "a confirmed mismatch must have been published");
    assert.equal(
      lastConfirmed.compatibility.result.compatibility,
      "engine-newer",
      "the mismatch must be retained after a failed remediation (R5.5)"
    );

    await extension.deactivate();
  } finally {
    throwForExecuteCommand = undefined;
  }
});

test("clicking Repair Engine routes through the destructive modal (R3.6, R5.2)", async () => {
  st.advancedMode = false;
  // Click Repair Engine on the notification, then CANCEL the confirmation modal
  // so nothing destructive actually runs — we only prove the modal was opened.
  await freshActivate({
    handshake: async () => handshakeResult("0.8.4", "unreadable"),
    responder: (_message: string, ...args: any[]) => {
      if (args.includes("Repair Engine")) {
        return "Repair Engine"; // click the notification's remediation button
      }
      // The Reinstall Engine confirmation is modal (args carry { modal: true }).
      return undefined; // cancel the modal → no destructive action
    },
  });

  // The notification opened the modal-confirmed Reinstall Engine dialog.
  const modals = modalWarningCalls();
  assert.equal(
    modals.length,
    1,
    "Repair Engine must open exactly one modal confirmation (R5.2)"
  );
  assert.ok(
    modals[0].args.includes("Reinstall Engine"),
    "the modal must be the Reinstall Engine confirmation"
  );

  await extension.deactivate();
});

test("Dismiss records the identity skip and never clears the Panel/Status Bar verdict (R4.4)", async () => {
  st.advancedMode = false;
  const mismatch = handshakeResult("0.8.3", "engine-outdated");
  await freshActivate({
    handshake: async () => mismatch,
    responder: (_message: string, ...args: any[]) =>
      args.includes("Dismiss") ? "Dismiss" : undefined,
  });

  // Exactly one notification was shown and the user dismissed it.
  assert.equal(notificationCalls().length, 1, "one notification must be shown");

  // Dismiss recorded a per-identity skip key set to true (so the SAME identity
  // stays silent next session) — and nothing else.
  const skips = globalStateUpdates.filter(
    ([key, value]) => key.startsWith("cognis.skipHandshakeWarning.") && value === true
  );
  assert.equal(skips.length, 1, "Dismiss must record exactly one per-identity skip key");
  assert.ok(
    skips[0][0].includes("engine-outdated"),
    "the skip key must be scoped to this Compatibility_Identity"
  );

  // Dismiss is notification-only: it must NOT clear the committed verdict. Drive
  // a fresh health poll — the coordinator still holds the confirmed mismatch (it
  // was never cleared), so both surfaces re-publish the SAME mismatch verdict.
  const healthContext = {
    ...defaultPanelContext,
    status: "mcpEnabled",
    health: { overall: "ok", runtime_version: "0.8.4", checks: {} },
    version: "post-dismiss",
  };
  refreshPanelContextImpl = async () => ({ ...healthContext });
  mcpStateListeners[0]({ repoRoot: st.folder!.uri.fsPath });
  await settle();

  const published = panelContexts.at(-1);
  assert.equal(
    published?.compatibility.phase,
    "confirmed",
    "the Panel verdict must survive Dismiss (Dismiss hides only the notification, R4.4)"
  );
  assert.equal(
    published?.compatibility.result.compatibility,
    "engine-outdated",
    "the committed mismatch verdict is unchanged after Dismiss"
  );
  // Status Bar saw the same committed snapshot — Dismiss split neither surface.
  assert.strictEqual(
    statusBarContexts.at(-1),
    published,
    "Status Bar and Panel share the same post-Dismiss verdict context"
  );

  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// Task 5.5 (R4.2): DEDUPE — at most one notification per Compatibility_Identity
// per activation session. A re-entrant reconcile for the SAME identity (same
// kind + version pair) must NOT show a second notification. The identity-key
// stability is unit-tested in compatibility.test.ts; this proves the seen-set
// is actually wired into the activation-level notification path so a repeated
// reconcile within one session stays silent.
// ---------------------------------------------------------------------------

test("a re-entrant reconcile for the same identity shows only one notification per session (R4.2)", async () => {
  st.advancedMode = false;
  // Every probe reports the SAME confirmed mismatch, so the identity is stable.
  const mismatch = handshakeResult("0.8.3", "engine-outdated");
  const ctx = await freshActivate({ handshake: async () => mismatch });

  // Activation already fired exactly one reconcile → one notification.
  assert.equal(
    notificationCalls().length,
    1,
    "activation must show exactly one notification for the first identity"
  );

  // Drive a SECOND reconcile within the same activation session. The coordinator
  // still holds the same committed mismatch (same identity), so the session
  // seen-set must suppress a duplicate notification.
  await extension.__test__.reconcileCompatibility();
  await settle();

  assert.equal(
    notificationCalls().length,
    1,
    "a re-entrant reconcile for the same identity must not show a second notification (R4.2)"
  );

  // Deduping the NOTIFICATION never touched the committed verdict: the Panel
  // still holds the confirmed mismatch.
  const published = panelContexts.at(-1);
  assert.equal(
    published?.compatibility.phase,
    "confirmed",
    "dedupe suppresses only the notification, not the committed verdict"
  );
  assert.equal(
    published?.compatibility.result.compatibility,
    "engine-outdated"
  );

  void ctx;
  await extension.deactivate();
});

// ---------------------------------------------------------------------------
// Task 5.5 (R4.5): a NEW Compatibility_Identity re-prompts even after a prior
// Dismiss of a different identity. Dismiss writes a per-identity skip key; a
// different actionable skew (different version pair here) has a different key,
// so it is not silenced and shows its own notification. This is the
// activation-level companion to the identity-key-difference unit tests.
// ---------------------------------------------------------------------------

test("a new identity re-prompts within a session even after a prior Dismiss of another identity (R4.5)", async () => {
  st.advancedMode = false;
  // First identity: engine-outdated 0.8.3 -> 0.8.4. The responder Dismisses it.
  const first = handshakeResult("0.8.3", "engine-outdated");
  // Second identity: a DIFFERENT engine version pair (0.8.2), so a distinct
  // Compatibility_Identity and a distinct skip key — Dismiss of the first must
  // not silence it.
  const second = handshakeResult("0.8.2", "engine-outdated");
  assert.notEqual(
    JSON.stringify(compatibilityIdentity(first)),
    JSON.stringify(compatibilityIdentity(second)),
    "test setup: the two verdicts must be distinct identities"
  );

  let probeIndex = 0;
  const verdicts = [first, second];
  // Dismiss ONLY the first notification; leave any later notification
  // offered-but-not-clicked so a re-prompt for a new identity is observable
  // without also writing a skip key for it.
  let dismissedOnce = false;
  await freshActivate({
    handshake: async () =>
      verdicts[Math.min(probeIndex++, verdicts.length - 1)],
    responder: (_message: string, ...args: any[]) => {
      if (args.includes("Dismiss") && !dismissedOnce) {
        dismissedOnce = true;
        return "Dismiss";
      }
      return undefined;
    },
  });

  // Activation shows + dismisses the first identity's notification.
  assert.equal(
    notificationCalls().length,
    1,
    "the first identity must show one notification"
  );
  const firstSkips = globalStateUpdates.filter(
    ([key, value]) =>
      key.startsWith("cognis.skipHandshakeWarning.") && value === true
  );
  assert.equal(firstSkips.length, 1, "the first identity's Dismiss records one skip key");
  assert.ok(
    firstSkips[0][0].includes("0.8.3"),
    "the first Dismiss must be scoped to the first identity's version pair"
  );

  // Commit the SECOND (different) identity. The coordinator keeps the first
  // verdict warm for its ≤30s TTL, so a plain reconcile would just re-read the
  // cached first verdict. Force a fresh, TTL-bypassing probe through the same
  // install path production uses (cognis.installBackend → forceCompatibilityReprobe),
  // which returns the second verdict and commits it as the current snapshot.
  const runInstall = registeredCommands.get("cognis.installBackend");
  assert.ok(runInstall, "cognis.installBackend must be registered");
  await runInstall();
  await settle();
  assert.ok(
    handshakeCalls.length >= 2,
    "the forced re-probe must run a second handshake to commit the new identity"
  );

  // Reconcile again within the SAME activation session. The committed snapshot
  // is now the second identity, which the first identity's Dismiss skip key
  // does not cover, so it shows its own (second) notification.
  await extension.__test__.reconcileCompatibility();
  await settle();

  // A distinct identity is NOT covered by the first identity's skip key, so it
  // shows its own notification: exactly two now (one per distinct identity),
  // proving the new identity re-prompted rather than being silenced by the
  // prior Dismiss of the first identity (R4.5).
  assert.equal(
    notificationCalls().length,
    2,
    "a new Compatibility_Identity must re-prompt even after a prior Dismiss (R4.5)"
  );
  // The second reconcile did NOT record a new skip key on its own — only an
  // explicit Dismiss writes one, and the second identity was never dismissed —
  // so the re-prompt is driven purely by the identity change.
  const skipsAfter = globalStateUpdates.filter(
    ([key, value]) =>
      key.startsWith("cognis.skipHandshakeWarning.") && value === true
  );
  assert.equal(
    skipsAfter.length,
    1,
    "only the first identity's Dismiss recorded a skip key; the new identity re-prompts unsilenced (R4.5)"
  );

  await extension.deactivate();
});
