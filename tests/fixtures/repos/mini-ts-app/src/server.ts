/**
 * Process entrypoint. Loads config, builds the Express app via
 * `createApp`, binds the HTTP listener, and wires SIGINT/SIGTERM to a
 * graceful shutdown. Kept deliberately small — `createApp` does the
 * actual wiring so tests can spin the app up without binding a port.
 */

import { createApp } from "./app";
import { createLogger } from "./utils/logger";
import { loadConfig, redactConfig } from "./utils/secrets";

async function main(): Promise<void> {
  const config = loadConfig();
  const logger = createLogger({ level: config.logLevel, bindings: { svc: "mini-ts-app" } });
  logger.info({ config: redactConfig(config) }, "boot-config");

  const { app } = createApp({ config, logger });

  const server = app.listen(config.port, () => {
    logger.info({ port: config.port, env: config.nodeEnv }, "server-listening");
  });

  const shutdown = (signal: string): void => {
    logger.info({ signal }, "shutdown-requested");
    server.close((err) => {
      if (err) {
        logger.error({ err: err.message }, "shutdown-error");
        process.exit(1);
      }
      logger.info({}, "shutdown-complete");
      process.exit(0);
    });
    // Force exit after 10s if connections refuse to drain.
    setTimeout(() => {
      logger.warn({}, "shutdown-forced");
      process.exit(1);
    }, 10_000).unref();
  };

  process.on("SIGINT", () => shutdown("SIGINT"));
  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("unhandledRejection", (reason) => {
    logger.error({ reason: reason instanceof Error ? reason.message : String(reason) }, "unhandled-rejection");
  });
  process.on("uncaughtException", (err) => {
    logger.error({ err: err.message, stack: err.stack }, "uncaught-exception");
    process.exit(1);
  });
}

// Only auto-start when invoked directly. Tests import `createApp` instead.
if (require.main === module) {
  main().catch((err) => {
    // eslint-disable-next-line no-console
    console.error("boot-failed", err);
    process.exit(1);
  });
}
