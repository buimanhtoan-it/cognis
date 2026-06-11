import * as vscode from "vscode";
import { getOutputChannel } from "./cli";
import {
  isPublicKeyConfigured,
  verifyLicenseKey,
  type LicenseStatus,
} from "./licenseCore";

export {
  verifyLicenseKey,
  type LicensePayload,
  type LicenseStatus,
} from "./licenseCore";

/**
 * Offline license gate for the paid (prebuilt) build — VS Code integration.
 *
 * The cryptographic verification lives in ``licenseCore.ts`` (pure, testable in
 * plain Node). This module adds editor plumbing: storing the key in
 * ``globalState``, prompting the user, and gating paid features.
 *
 * Verification is fully offline (Ed25519 signature against an embedded public
 * key) so there is no license server and zero ops after a sale. A
 * Merchant-of-Record issues the signed keys and handles billing + tax.
 *
 * In the open-source/source build there is no embedded public key, so the gate
 * is OPEN — paid gating only applies to the prebuilt commercial build.
 */

const LICENSE_STATE_KEY = "cognis.licenseKey.v1";

let cachedStatus: LicenseStatus | undefined;

/** The running extension version (major.minor[.patch]) for version-band gating. */
function runningVersion(context: vscode.ExtensionContext): string | undefined {
  const pkg = context.extension?.packageJSON as { version?: string } | undefined;
  return pkg?.version;
}

/** Read the stored key and compute current status (cached per session). */
export function getLicenseStatus(context: vscode.ExtensionContext): LicenseStatus {
  if (cachedStatus) {
    return cachedStatus;
  }
  const stored = context.globalState.get<string>(LICENSE_STATE_KEY) ?? "";
  cachedStatus = verifyLicenseKey(stored, undefined, undefined, runningVersion(context));
  return cachedStatus;
}

export function isLicensed(context: vscode.ExtensionContext): boolean {
  return getLicenseStatus(context).licensed;
}

/**
 * Prompt the user for a license key, validate it offline, and persist it on
 * success. Returns the resulting status.
 */
export async function enterLicenseKey(
  context: vscode.ExtensionContext
): Promise<LicenseStatus> {
  const key = await vscode.window.showInputBox({
    title: "Activate Cognis",
    prompt: "Paste your license key (from your purchase email).",
    ignoreFocusOut: true,
    placeHolder: "<payload>.<signature>",
  });
  if (key === undefined) {
    return getLicenseStatus(context);
  }
  const status = verifyLicenseKey(key, undefined, undefined, runningVersion(context));
  if (status.licensed) {
    await context.globalState.update(LICENSE_STATE_KEY, key.trim());
    cachedStatus = status;
    const who = status.payload?.email ? ` (${status.payload.email})` : "";
    void vscode.window.showInformationMessage(`Cognis activated${who}. Thank you!`);
  } else {
    getOutputChannel().appendLine(`[license] activation failed: ${status.reason}`);
    void vscode.window.showErrorMessage(
      `License key not accepted: ${status.reason ?? "invalid key."}`
    );
  }
  return status;
}

/** Clear the stored license (for testing / transferring machines). */
export async function clearLicense(context: vscode.ExtensionContext): Promise<void> {
  await context.globalState.update(LICENSE_STATE_KEY, undefined);
  cachedStatus = undefined;
}

/**
 * Gate a paid feature. When unlicensed, shows an actionable prompt (Activate /
 * Buy) and returns false so the caller aborts. When the build has no public key
 * configured (the open-source/source build), the gate is OPEN — paid gating
 * only applies to the prebuilt commercial build that ships a real key.
 *
 * @returns true if the caller may proceed with the feature.
 */
export async function requireLicense(
  context: vscode.ExtensionContext,
  featureLabel: string,
  buyUrl?: string
): Promise<boolean> {
  if (!isPublicKeyConfigured()) {
    return true; // source build: fully open
  }
  if (isLicensed(context)) {
    return true;
  }
  // Prefer the operator-configured checkout link; fall back to the official
  // Polar checkout so the button is never a dead link.
  const target =
    buyUrl?.trim() ||
    vscode.workspace.getConfiguration("cognis").get<string>("buyUrl")?.trim() ||
    "https://buy.polar.sh/polar_cl_tbpNy7AHIlPtsDR4PwB3KkGVDQrnoaqM4uZew1dRSRW";
  const choice = await vscode.window.showInformationMessage(
    `${featureLabel} is part of the paid Cognis build. Activate your license to continue.`,
    "Enter license key",
    "Buy Cognis"
  );
  if (choice === "Enter license key") {
    const status = await enterLicenseKey(context);
    return status.licensed;
  }
  if (choice === "Buy Cognis") {
    void vscode.env.openExternal(vscode.Uri.parse(target));
  }
  return false;
}
