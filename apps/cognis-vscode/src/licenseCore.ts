import * as crypto from "crypto";

/**
 * Pure, dependency-free offline license verification.
 *
 * Kept separate from ``license.ts`` (which pulls in the ``vscode`` API and
 * state plumbing) so the cryptographic core can be unit tested in plain Node
 * without a VS Code harness. See ``license.ts`` for the full design notes.
 */

// Embedded public key — REPLACE before shipping a paid build.
//   openssl genpkey -algorithm ed25519 -out cognis_license_private.pem
//   openssl pkey -in cognis_license_private.pem -pubout -out cognis_license_public.pem
export const LICENSE_PUBLIC_KEY_PEM = `-----BEGIN PUBLIC KEY-----
REPLACE_WITH_YOUR_ED25519_PUBLIC_KEY
-----END PUBLIC KEY-----`;

export interface LicensePayload {
  email?: string;
  seats?: number;
  plan?: string;
  issued?: string;
  /** ISO date string, or null/absent for a perpetual license. */
  expires?: string | null;
  /**
   * Highest ``major.minor`` this key unlocks (e.g. ``"0.5"`` covers all 0.5.x
   * patches but not 0.6). Absent/null = unlocks any version. Used for the
   * "paid per minor version, free patches" model: a 0.5 key stops unlocking
   * 0.6, so the next paid minor is a new purchase.
   */
  max_version?: string | null;
}

export interface LicenseStatus {
  licensed: boolean;
  payload?: LicensePayload;
  /** Human-readable reason when not licensed (for UI/logs). */
  reason?: string;
}

/** Parse a version string ("v0.5.3", "0.5", "0.5.0") to ``[major, minor]``. */
function parseMinor(version: string): [number, number] | null {
  const m = /^v?(\d+)\.(\d+)/.exec(version.trim());
  if (!m) {
    return null;
  }
  return [Number(m[1]), Number(m[2])];
}

/**
 * Return true when *running* is within the band allowed by *maxVersion*
 * (running major.minor <= maxVersion major.minor). Patch level is ignored, so a
 * "0.5" key covers every 0.5.x. Unparseable inputs fail open (no version gate).
 */
function withinVersionBand(running: string, maxVersion: string): boolean {
  const r = parseMinor(running);
  const cap = parseMinor(maxVersion);
  if (r === null || cap === null) {
    return true; // can't evaluate → don't gate on version
  }
  if (r[0] !== cap[0]) {
    return r[0] < cap[0];
  }
  return r[1] <= cap[1];
}

export function isPublicKeyConfigured(
  publicKeyPem: string = LICENSE_PUBLIC_KEY_PEM
): boolean {
  return !publicKeyPem.includes("REPLACE_WITH_YOUR_ED25519_PUBLIC_KEY");
}

function b64urlDecode(input: string): Buffer {
  const pad = input.length % 4 === 0 ? "" : "=".repeat(4 - (input.length % 4));
  return Buffer.from(input.replace(/-/g, "+").replace(/_/g, "/") + pad, "base64");
}

/**
 * Verify a license key string offline. Never throws; always returns a status.
 *
 * Format: ``"<base64url(payloadJSON)>.<base64url(ed25519Signature)>"``.
 */
export function verifyLicenseKey(
  key: string,
  publicKeyPem: string = LICENSE_PUBLIC_KEY_PEM,
  now: Date = new Date(),
  runningVersion?: string
): LicenseStatus {
  if (!key || !key.trim()) {
    return { licensed: false, reason: "No license key provided." };
  }
  if (!isPublicKeyConfigured(publicKeyPem)) {
    return { licensed: false, reason: "License public key is not configured in this build." };
  }
  const parts = key.trim().split(".");
  if (parts.length !== 2) {
    return { licensed: false, reason: "Malformed license key (expected <payload>.<signature>)." };
  }
  const [payloadB64, sigB64] = parts;
  let payloadBuf: Buffer;
  let sigBuf: Buffer;
  try {
    payloadBuf = b64urlDecode(payloadB64);
    sigBuf = b64urlDecode(sigB64);
  } catch {
    return { licensed: false, reason: "License key is not valid base64url." };
  }
  if (payloadBuf.length === 0 || sigBuf.length === 0) {
    return { licensed: false, reason: "License key is empty after decoding." };
  }

  let verified = false;
  try {
    const publicKey = crypto.createPublicKey(publicKeyPem);
    verified = crypto.verify(null, payloadBuf, publicKey, sigBuf);
  } catch {
    return { licensed: false, reason: "Signature verification failed." };
  }
  if (!verified) {
    return { licensed: false, reason: "License signature does not match." };
  }

  let payload: LicensePayload;
  try {
    payload = JSON.parse(payloadBuf.toString("utf8")) as LicensePayload;
  } catch {
    return { licensed: false, reason: "License payload is not valid JSON." };
  }

  if (payload.expires) {
    const exp = new Date(payload.expires);
    if (!Number.isNaN(exp.getTime()) && exp.getTime() < now.getTime()) {
      return { licensed: false, payload, reason: `License expired on ${payload.expires}.` };
    }
  }
  if (payload.max_version && runningVersion && !withinVersionBand(runningVersion, payload.max_version)) {
    return {
      licensed: false,
      payload,
      reason:
        `This license covers Cognis up to v${payload.max_version}; ` +
        `you are running v${runningVersion}. Upgrade requires a new purchase.`,
    };
  }
  return { licensed: true, payload };
}
