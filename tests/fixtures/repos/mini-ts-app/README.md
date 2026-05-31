# mini-ts-app — cognis test fixture

This is a deliberately small Express + JWT TypeScript service used by the
`cognis` test suite. It is **not** intended to run in production and the source
is checked in only so the indexer, retrieval mesh, and capsule composer have a
realistic-shaped repo to chew on.

## Layout

```
mini-ts-app/
├── package.json          declared dependencies (NOT installed in CI)
├── tsconfig.json         strict TS config, ES2022 target
├── src/
│   ├── server.ts         entry point — boots HTTP listener
│   ├── app.ts            createApp() — wires middleware + routes
│   ├── auth/
│   │   └── jwt.ts        JWT sign / validate (PLANTED BUG lives here)
│   ├── middleware/
│   │   ├── auth.ts       requireAuth — verifies bearer tokens via jwt.validate
│   │   ├── logging.ts    pino-style request logger
│   │   └── errorHandler.ts  central error-to-response translator
│   ├── routes/
│   │   ├── index.ts      registerRoutes() — mounts every route module
│   │   ├── login.ts      POST /login — checks password, issues JWT
│   │   ├── users.ts      GET /users/me, PATCH /users/me — protected routes
│   │   └── health.ts     GET /health, GET /readiness — public probes
│   ├── db/
│   │   └── userRepo.ts   in-memory user repo, SQL-shaped string literals
│   └── utils/
│       ├── secrets.ts    env loader, redaction helpers
│       ├── logger.ts     thin pino wrapper
│       └── time.ts       monotonic clock helpers
└── README.md             you are here
```

## Planted bug — auth-timeout

`src/auth/jwt.ts` contains a deliberate latency bomb in `validate()`:

1. The function performs a blocking `bcrypt.compareSync` against the token's
   payload-hash with `BCRYPT_COST=14` (≈ 1.5 s on a typical CI runner).
2. It also calls a **token introspection** endpoint without a `Promise.race`
   timeout — the helper passes `TOKEN_INTROSPECTION_TIMEOUT_MS` from env into
   the fetch call but then ignores it, so a hung introspector wedges the route.
3. A `setTimeout` `await` keyed off `AUTH_DEBUG_DELAY_MS` lets tests amplify the
   stall without needing a real network partition.

The bug location is tagged in source with a comment block:

```ts
// PLANTED-BUG: auth-timeout
```

so cognis eval queries can verify retrieval lands on `validate` (and the call
chain `postLogin → requireAuth → validate`).

This fixture backs golden query `q01-bugfix-jwt-timeout` in
`tests/fixtures/eval/golden.jsonl`. The expected symbol ids reference
`ts:src/auth/jwt.ts:validate`, `ts:src/middleware/auth.ts:requireAuth`,
`ts:src/routes/login.ts:postLogin`, `ts:src/app.ts:createApp`, and
`ts:src/routes/index.ts:registerRoutes`. Future task 5.4 emits an
`expected_symbols.json` keyed off these symbols.

## Running

The fixture is **not** wired into CI as a runnable service — `node_modules` is
not vendored and `npm install` is not invoked. If you want to run it locally:

```bash
cd tests/fixtures/repos/mini-ts-app
npm install
npm run build
JWT_SECRET=dev-only npm start
```

Then `curl http://localhost:3000/health` should return `{"status":"ok"}`.

## Reproducing the bug

```bash
AUTH_DEBUG_DELAY_MS=4000 BCRYPT_COST=14 npm start &
curl -X POST http://localhost:3000/login \
     -H 'content-type: application/json' \
     -d '{"username":"alice","password":"correct horse"}'
# subsequent calls to /users/me with the issued bearer will hang ~5 s
```

That timing matches the symptom in the bugfix golden query.
