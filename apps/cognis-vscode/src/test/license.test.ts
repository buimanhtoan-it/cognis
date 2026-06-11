import * as assert from "node:assert";
import { execFileSync } from "node:child_process";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { test } from "node:test";

import { verifyLicenseKey } from "../licenseCore";

/**
 * Build a signed license key the same way the seller's issuer would, so we can
 * verify the offline gate end-to-end with a throwaway Ed25519 keypair.
 */
function makeKey(
  privateKey: crypto.KeyObject,
  payload: Record<string, unknown>
): string {
  const payloadBuf = Buffer.from(JSON.stringify(payload), "utf8");
  const sig = crypto.sign(null, payloadBuf, privateKey);
  const b64url = (b: Buffer) =>
    b.toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  return `${b64url(payloadBuf)}.${b64url(sig)}`;
}

function keypair() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const publicPem = publicKey.export({ type: "spki", format: "pem" }).toString();
  return { publicPem, privateKey };
}

test("a correctly signed key validates offline", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", seats: 1, plan: "build" });
  const status = verifyLicenseKey(key, publicPem);
  assert.equal(status.licensed, true);
  assert.equal(status.payload?.email, "buyer@x.com");
});

test("a tampered payload is rejected", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", seats: 1 });
  const [, sig] = key.split(".");
  const forgedPayload = Buffer.from(JSON.stringify({ email: "buyer@x.com", seats: 999 }))
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
  const status = verifyLicenseKey(`${forgedPayload}.${sig}`, publicPem);
  assert.equal(status.licensed, false);
});

test("a key signed by a different key is rejected", () => {
  const a = keypair();
  const b = keypair();
  const key = makeKey(a.privateKey, { email: "buyer@x.com" });
  const status = verifyLicenseKey(key, b.publicPem); // verify with the WRONG public key
  assert.equal(status.licensed, false);
});

test("an expired license is rejected", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", expires: "2000-01-01" });
  const status = verifyLicenseKey(key, publicPem, new Date("2026-06-06"));
  assert.equal(status.licensed, false);
  assert.match(status.reason ?? "", /expired/i);
});

test("a perpetual license (no expiry) validates", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", expires: null });
  const status = verifyLicenseKey(key, publicPem, new Date("2099-01-01"));
  assert.equal(status.licensed, true);
});

test("malformed keys never throw and return licensed:false", () => {
  const { publicPem } = keypair();
  for (const bad of ["", "   ", "noseparator", "a.b.c", "@@@.@@@"]) {
    const status = verifyLicenseKey(bad, publicPem);
    assert.equal(status.licensed, false);
    assert.ok(status.reason);
  }
});

test("the placeholder public key validates nothing", () => {
  const { privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com" });
  // Default embedded key is the placeholder → no key should validate.
  const status = verifyLicenseKey(key);
  assert.equal(status.licensed, false);
});

// --- Version-band licensing ("paid per minor, free patches") --------------

test("max_version 0.5 unlocks a 0.5.x patch", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", max_version: "0.5" });
  const status = verifyLicenseKey(key, publicPem, new Date(), "0.5.3");
  assert.equal(status.licensed, true);
});

test("max_version 0.5 covers the 0.5.0 boundary", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", max_version: "0.5" });
  const status = verifyLicenseKey(key, publicPem, new Date(), "0.5.0");
  assert.equal(status.licensed, true);
});

test("max_version 0.5 still covers an older 0.4.x build", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", max_version: "0.5" });
  const status = verifyLicenseKey(key, publicPem, new Date(), "0.4.9");
  assert.equal(status.licensed, true);
});

test("max_version 0.5 does NOT unlock the next minor 0.6.0", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", max_version: "0.5" });
  const status = verifyLicenseKey(key, publicPem, new Date(), "0.6.0");
  assert.equal(status.licensed, false);
  assert.match(status.reason ?? "", /new purchase|covers/i);
});

test("no max_version unlocks any version (backward compat)", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com" });
  const status = verifyLicenseKey(key, publicPem, new Date(), "9.9.9");
  assert.equal(status.licensed, true);
});

test("max_version present but no runningVersion fails open (no gate)", () => {
  const { publicPem, privateKey } = keypair();
  const key = makeKey(privateKey, { email: "buyer@x.com", max_version: "0.5" });
  const status = verifyLicenseKey(key, publicPem, new Date()); // runningVersion undefined
  assert.equal(status.licensed, true);
});

// --- Signer round-trip (scripts/sign-license.mjs) --------------------------

test("sign-license.mjs issues a key the verifier accepts within its band", (t) => {
  // The signer is seller-only tooling that lives in the gitignored business/
  // dir, so it is absent in a fresh clone / CI — skip cleanly there and only
  // run the round-trip where the seller tooling exists locally.
  // out/test/ -> repo root is four levels up, then business/.
  const scriptPath = path.join(
    __dirname,
    "..",
    "..",
    "..",
    "..",
    "business",
    "sign-license.mjs"
  );
  if (!fs.existsSync(scriptPath)) {
    t.skip("seller-only signer (business/sign-license.mjs) not present");
    return;
  }

  // Bootstrap a throwaway keypair and write the private PEM to a temp file so
  // the signer subprocess can read it exactly as the seller would.
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const publicPem = publicKey.export({ type: "spki", format: "pem" }).toString();
  const privPem = privateKey.export({ type: "pkcs8", format: "pem" }).toString();

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "cognis-lic-"));
  const privPath = path.join(dir, "priv.pem");
  fs.writeFileSync(privPath, privPem);

  try {
    const key = execFileSync(
      process.execPath,
      [
        scriptPath,
        "--private-key",
        privPath,
        "--email",
        "b@x.com",
        "--max-version",
        "0.5",
      ],
      { encoding: "utf8" }
    ).trim();

    const within = verifyLicenseKey(key, publicPem, new Date(), "0.5.2");
    assert.equal(within.licensed, true);
    assert.equal(within.payload?.email, "b@x.com");
    assert.equal(within.payload?.max_version, "0.5");

    const beyond = verifyLicenseKey(key, publicPem, new Date(), "0.6.0");
    assert.equal(beyond.licensed, false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
