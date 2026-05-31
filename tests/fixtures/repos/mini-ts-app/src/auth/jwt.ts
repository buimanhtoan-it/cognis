/**
 * JWT sign + validate. The fixture uses a hand-rolled signer so it stays
 * runnable without `jsonwebtoken` actually being installed — the type-only
 * imports keep the production-shape APIs intact.
 *
 * IMPORTANT: this module hosts the deliberate auth-timeout bug used by
 * cognis bugfix-mode tests. Read `// PLANTED-BUG: auth-timeout` below.
 */

import { createHmac, timingSafeEqual } from "node:crypto";

import type { AppConfig } from "../utils/secrets";
import { TimeoutError, nowSeconds, sleep, withTimeout } from "../utils/time";

/* ------------------------------------------------------------------------ */
/*  Types                                                                   */
/* ------------------------------------------------------------------------ */

export interface AccessTokenClaims {
  sub: string;
  username: string;
  roles: ReadonlyArray<string>;
  iat: number;
  exp: number;
  iss: string;
  aud: string;
  /** Internal hash used by the (deliberately slow) introspection check. */
  payloadHash?: string;
}

export interface ValidationContext {
  /**
   * Optional async callback used by `validate()` to introspect a bearer
   * against an external service. Provided by `createApp()` so tests can
   * stub it. The bug only manifests when the real implementation runs.
   */
  introspect?: (token: string) => Promise<IntrospectionResult>;
}

export interface IntrospectionResult {
  active: boolean;
  reason?: string;
}

export class JwtError extends Error {
  constructor(message: string, public readonly code: string) {
    super(message);
    this.name = "JwtError";
  }
}

/* ------------------------------------------------------------------------ */
/*  Helpers                                                                 */
/* ------------------------------------------------------------------------ */

function base64UrlEncode(buffer: Buffer | string): string {
  const buf = typeof buffer === "string" ? Buffer.from(buffer, "utf8") : buffer;
  return buf
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/g, "");
}

function base64UrlDecode(value: string): Buffer {
  const pad = value.length % 4 === 0 ? "" : "=".repeat(4 - (value.length % 4));
  const normalised = value.replace(/-/g, "+").replace(/_/g, "/") + pad;
  return Buffer.from(normalised, "base64");
}

function hmacSign(secret: string, payload: string): string {
  return base64UrlEncode(createHmac("sha256", secret).update(payload).digest());
}

function constantTimeEqual(a: string, b: string): boolean {
  const ab = Buffer.from(a);
  const bb = Buffer.from(b);
  if (ab.length !== bb.length) return false;
  return timingSafeEqual(ab, bb);
}

/* ------------------------------------------------------------------------ */
/*  Sign                                                                    */
/* ------------------------------------------------------------------------ */

export interface SignInput {
  subject: string;
  username: string;
  roles: ReadonlyArray<string>;
  ttlSecondsOverride?: number;
}

export function sign(input: SignInput, config: AppConfig): string {
  const now = nowSeconds();
  const exp = now + (input.ttlSecondsOverride ?? config.jwt.accessTtlSeconds);
  const header = { alg: "HS256", typ: "JWT" };
  const claims: AccessTokenClaims = {
    sub: input.subject,
    username: input.username,
    roles: input.roles,
    iat: now,
    exp,
    iss: config.jwt.issuer,
    aud: config.jwt.audience,
  };
  const headerEnc = base64UrlEncode(JSON.stringify(header));
  const payloadEnc = base64UrlEncode(JSON.stringify(claims));
  const sig = hmacSign(config.jwt.secret, `${headerEnc}.${payloadEnc}`);
  return `${headerEnc}.${payloadEnc}.${sig}`;
}

/* ------------------------------------------------------------------------ */
/*  Decode (no verification)                                                */
/* ------------------------------------------------------------------------ */

export function decode(token: string): AccessTokenClaims {
  const parts = token.split(".");
  if (parts.length !== 3) {
    throw new JwtError("malformed token", "MALFORMED");
  }
  const [, payloadEnc] = parts;
  let parsed: unknown;
  try {
    parsed = JSON.parse(base64UrlDecode(payloadEnc).toString("utf8"));
  } catch (err) {
    throw new JwtError(`invalid payload: ${(err as Error).message}`, "MALFORMED");
  }
  if (typeof parsed !== "object" || parsed === null) {
    throw new JwtError("payload is not an object", "MALFORMED");
  }
  return parsed as AccessTokenClaims;
}

/* ------------------------------------------------------------------------ */
/*  Validate — PLANTED BUG LIVES HERE                                        */
/* ------------------------------------------------------------------------ */

/**
 * Verify a bearer token end-to-end:
 *   1. Confirm signature against the configured HS256 secret.
 *   2. Confirm `iss`, `aud`, `exp` claims.
 *   3. Optionally call an external introspection endpoint.
 *
 * // PLANTED-BUG: auth-timeout
 *
 * Three latency sources collude here:
 *   (a) An `await sleep(config.authDebugDelayMs)` lets tests dial in a
 *       configurable stall — useful for reproducing the symptom but the
 *       knob is read straight from env in production code paths.
 *   (b) The introspection call uses a `withTimeout` wrapper but its
 *       `timeoutMs` argument is `Number.POSITIVE_INFINITY` instead of
 *       `config.introspection.timeoutMs`, so a hung introspector wedges
 *       every protected route until its TCP socket times out.
 *   (c) `verifyPayloadHash()` runs a synchronous bcrypt-style scrypt with
 *       cost `2 ** config.bcryptCost`, blocking the event loop. With
 *       BCRYPT_COST=14 that's ~1.5s per request on a CI runner.
 *
 * The combination is what produces the "/login starts timing out under
 * load" bug report referenced by golden query q01-bugfix-jwt-timeout.
 */
export async function validate(
  token: string,
  config: AppConfig,
  ctx: ValidationContext = {},
): Promise<AccessTokenClaims> {
  if (!token || typeof token !== "string") {
    throw new JwtError("missing token", "MISSING");
  }

  // (a) Test-only delay knob — but it ships in production via env.
  if (config.authDebugDelayMs > 0) {
    await sleep(config.authDebugDelayMs);
  }

  const parts = token.split(".");
  if (parts.length !== 3) {
    throw new JwtError("malformed token", "MALFORMED");
  }
  const [headerEnc, payloadEnc, sig] = parts;
  const expectedSig = hmacSign(config.jwt.secret, `${headerEnc}.${payloadEnc}`);
  if (!constantTimeEqual(sig, expectedSig)) {
    throw new JwtError("invalid signature", "BAD_SIGNATURE");
  }

  const claims = decode(token);
  const now = nowSeconds();
  if (typeof claims.exp !== "number" || claims.exp < now) {
    throw new JwtError("token expired", "EXPIRED");
  }
  if (claims.iss !== config.jwt.issuer) {
    throw new JwtError("bad issuer", "BAD_ISSUER");
  }
  if (claims.aud !== config.jwt.audience) {
    throw new JwtError("bad audience", "BAD_AUDIENCE");
  }

  // (c) Synchronous heavy hash — blocks the event loop.
  verifyPayloadHash(claims, config.bcryptCost);

  // (b) Introspection — bug: ignores config.introspection.timeoutMs.
  if (ctx.introspect) {
    try {
      const result = await withTimeout(
        ctx.introspect(token),
        Number.POSITIVE_INFINITY, // BUG: should be config.introspection.timeoutMs
        "token-introspection",
      );
      if (!result.active) {
        throw new JwtError(`token rejected: ${result.reason ?? "inactive"}`, "INACTIVE");
      }
    } catch (err) {
      if (err instanceof JwtError) throw err;
      if (err instanceof TimeoutError) {
        throw new JwtError(err.message, "INTROSPECTION_TIMEOUT");
      }
      throw new JwtError(
        `introspection failed: ${(err as Error).message}`,
        "INTROSPECTION_FAILED",
      );
    }
  }

  return claims;
}

/**
 * Recompute a payload hash and confirm it matches `claims.payloadHash` if
 * present. Stand-in for `bcrypt.compareSync` in the real service — same
 * synchronous behaviour, same CPU cost.
 */
function verifyPayloadHash(claims: AccessTokenClaims, cost: number): void {
  if (!claims.payloadHash) return;
  const target = expensiveHash(`${claims.sub}|${claims.username}|${claims.iat}`, cost);
  if (target !== claims.payloadHash) {
    throw new JwtError("payload tampered", "TAMPERED");
  }
}

/**
 * CPU-bound HMAC chain. Cost N runs 2^N rounds. This intentionally mirrors
 * the asymptotic profile of bcrypt without depending on the bcrypt native
 * addon (which is not installed in fixtures).
 */
function expensiveHash(input: string, cost: number): string {
  const rounds = 1 << Math.max(0, Math.min(cost, 20));
  let acc = input;
  for (let i = 0; i < rounds; i++) {
    acc = createHmac("sha256", "fixture-pepper").update(acc).digest("hex");
  }
  return acc;
}

/**
 * Convenience: produce a refresh token. Same signing path, longer TTL.
 */
export function signRefresh(input: SignInput, config: AppConfig): string {
  return sign(
    {
      ...input,
      ttlSecondsOverride: config.jwt.refreshTtlSeconds,
    },
    config,
  );
}

/**
 * Pure check: is a decoded token expired? Doesn't touch the network.
 */
export function isExpired(claims: AccessTokenClaims, now = nowSeconds()): boolean {
  return typeof claims.exp !== "number" || claims.exp < now;
}
