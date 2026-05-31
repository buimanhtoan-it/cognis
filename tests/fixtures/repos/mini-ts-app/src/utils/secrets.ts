/**
 * Configuration loader. Pulls runtime parameters out of `process.env`,
 * validates them, and exposes a typed `AppConfig`. NOTHING in this file
 * embeds a real credential — every secret is sourced at runtime.
 */

import { levelFromString, type LogLevel } from "./logger";

export interface JwtConfig {
  secret: string;
  accessTtlSeconds: number;
  refreshTtlSeconds: number;
  issuer: string;
  audience: string;
}

export interface IntrospectionConfig {
  url: string;
  timeoutMs: number;
}

export interface AppConfig {
  port: number;
  nodeEnv: "development" | "test" | "production";
  jwt: JwtConfig;
  introspection: IntrospectionConfig;
  bcryptCost: number;
  logLevel: LogLevel;
  authDebugDelayMs: number;
}

/**
 * Read a required string from env. Throws when missing so misconfiguration
 * fails fast at boot rather than producing a runtime mystery later.
 */
function requireEnv(name: string, env: NodeJS.ProcessEnv = process.env): string {
  const value = env[name];
  if (value === undefined || value === "") {
    throw new Error(`missing required env var: ${name}`);
  }
  return value;
}

function readNumber(name: string, fallback: number, env: NodeJS.ProcessEnv = process.env): number {
  const raw = env[name];
  if (raw === undefined || raw === "") {
    return fallback;
  }
  const parsed = Number(raw);
  if (!Number.isFinite(parsed)) {
    throw new Error(`env var ${name} is not a valid number: ${raw}`);
  }
  return parsed;
}

function readNodeEnv(env: NodeJS.ProcessEnv): AppConfig["nodeEnv"] {
  const raw = (env.NODE_ENV ?? "development").toLowerCase();
  if (raw === "production" || raw === "test" || raw === "development") {
    return raw;
  }
  return "development";
}

/**
 * Build an `AppConfig` from the current environment. Tests pass a custom
 * `env` map; production uses `process.env`.
 */
export function loadConfig(env: NodeJS.ProcessEnv = process.env): AppConfig {
  const jwtSecret = requireEnv("JWT_SECRET", env);
  return {
    port: readNumber("PORT", 3000, env),
    nodeEnv: readNodeEnv(env),
    jwt: {
      secret: jwtSecret,
      accessTtlSeconds: readNumber("JWT_ACCESS_TTL_SECONDS", 900, env),
      refreshTtlSeconds: readNumber("JWT_REFRESH_TTL_SECONDS", 604_800, env),
      issuer: env.JWT_ISSUER ?? "mini-ts-app",
      audience: env.JWT_AUDIENCE ?? "mini-ts-app-clients",
    },
    introspection: {
      url: env.TOKEN_INTROSPECTION_URL ?? "http://localhost:9999/introspect",
      timeoutMs: readNumber("TOKEN_INTROSPECTION_TIMEOUT_MS", 2500, env),
    },
    bcryptCost: readNumber("BCRYPT_COST", 12, env),
    logLevel: levelFromString(env.LOG_LEVEL, "info"),
    authDebugDelayMs: readNumber("AUTH_DEBUG_DELAY_MS", 0, env),
  };
}

/**
 * Redact a string for logging. Used by the request logger so authorization
 * headers never end up in plain log output. Real secret detection is the
 * responsibility of cognis itself; this is just defence-in-depth.
 */
export function redactBearer(value: string | undefined): string {
  if (!value) return "";
  if (value.toLowerCase().startsWith("bearer ")) {
    return "Bearer [REDACTED]";
  }
  if (value.length > 12) {
    return `${value.slice(0, 4)}…${value.slice(-2)}`;
  }
  return "[REDACTED]";
}

/**
 * Redact a generic object so we can stamp partial config into logs without
 * leaking the JWT secret. Returns a copy.
 */
export function redactConfig(cfg: AppConfig): Record<string, unknown> {
  return {
    port: cfg.port,
    nodeEnv: cfg.nodeEnv,
    jwt: {
      secret: "[REDACTED]",
      accessTtlSeconds: cfg.jwt.accessTtlSeconds,
      refreshTtlSeconds: cfg.jwt.refreshTtlSeconds,
      issuer: cfg.jwt.issuer,
      audience: cfg.jwt.audience,
    },
    introspection: {
      url: cfg.introspection.url,
      timeoutMs: cfg.introspection.timeoutMs,
    },
    bcryptCost: cfg.bcryptCost,
    logLevel: cfg.logLevel,
    authDebugDelayMs: cfg.authDebugDelayMs,
  };
}

/**
 * Detect strings that look like credentials. Used as a tripwire in the
 * request logger; cognis has its own redactor that supersedes this one.
 */
const SECRET_REGEXES: RegExp[] = [
  /AKIA[0-9A-Z]{16}/g,
  /AIza[0-9A-Za-z_-]{35}/g,
  /ghp_[A-Za-z0-9]{36}/g,
  /xox[baprs]-[A-Za-z0-9-]{10,}/g,
  /-----BEGIN [A-Z ]+PRIVATE KEY-----/g,
  /eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}/g,
];

export function looksLikeSecret(value: string): boolean {
  for (const re of SECRET_REGEXES) {
    if (re.test(value)) {
      return true;
    }
  }
  return false;
}

export function scrubSecrets(text: string): string {
  let result = text;
  for (const re of SECRET_REGEXES) {
    result = result.replace(re, "[REDACTED:secret]");
  }
  return result;
}
