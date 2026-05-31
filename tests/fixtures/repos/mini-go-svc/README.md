# mini-go-svc — cognis test fixture

A deliberately small Gin microservice used by the `cognis` test suite. It
is **not** intended to run; it exists so the indexer, edge resolver,
review-mode classifier, and capsule composer have a realistic-shaped Go
repo to parse and probe.

## Layout

```
mini-go-svc/
├── go.mod                              declared deps (NOT installed in CI)
├── README.md                           you are here
├── cmd/
│   └── server/
│       └── main.go                     setupRouter() + main()  (PLANTED dead-import)
└── internal/
    ├── auth/
    │   └── jwt.go                      ValidateJWT, claims helpers
    ├── config/
    │   └── config.go                   Load() — env-var loader
    ├── db/
    │   └── repo.go                     in-memory repo, raw SQL string literals
    ├── handlers/
    │   ├── orders.go                   OrdersHandler — Create/Update/Cancel (goroutine-based audit)
    │   └── legacy.go                   LegacyHandler — unused, kept for review-mode tests
    ├── middleware/
    │   ├── logging.go                  request log + access log
    │   └── ratelimit.go                NewRateLimiter — token-bucket with refill goroutine
    └── validation/
        └── orders.go                   ValidateOrder, OrderError
```

## Planted issues (and what each is for)

This fixture intentionally embeds patterns that exercise specific cognis
subsystems. None of the "secrets" or "leaks" are real credentials — they
exist as input shapes for the parsers and classifiers.

### 1. Dead import in `cmd/server/main.go`

`main.go` imports `"fmt"` but never references it. The line is annotated:

```go
import (
    "fmt" // PLANTED-ISSUE: dead-import — kept so review-mode classifier can pin it
    ...
)
```

This is the canonical "unused import" review finding. tree-sitter-go parses
it cleanly; `go build` would reject it, but this fixture is never compiled.
The cognis review-mode classifier should surface this as a `review_finding`
with `rationale="unused import"` and `symbol_id` pointing at
`go:cmd/server/main.go:main`.

### 2. Unreferenced `LegacyHandler` in `internal/handlers/legacy.go`

`LegacyHandler` is fully exported, has methods (`HandlePing`,
`HandleHealthCheck`, `Deprecated`), and is **never wired to any route** in
`setupRouter()`. The structural-edge resolver should produce zero inbound
edges for `go:internal/handlers/legacy.go:LegacyHandler` — the
review-mode capsule composer uses this signal to flag the type as
"orphaned export" candidate for deletion.

### 3. Goroutine-based audit dispatch in `OrdersHandler.CreateOrder`

`CreateOrder` spawns an unbounded goroutine to write an audit-log entry:

```go
go func() {
    h.audit.Record(ctx, "order.created", order.ID)
}()
```

This is the planted concurrency smell. The cognis enricher should attach a
`spawns_goroutine=true` attribute to the symbol; review-mode capsules
should surface "no error handling on async path" as a candidate finding.

### 4. Goroutine-based token-bucket refill in `NewRateLimiter`

`NewRateLimiter` returns gin middleware backed by a buffered channel acting
as a token bucket; a long-lived goroutine refills the channel at a fixed
interval. This exercises edge resolution across concurrency boundaries
(`go func() { ... }()` referencing closure-captured state) and gives the
indexer a function symbol whose `kind=function` co-exists with a
`spawns_goroutine=true` attribute.

### 5. Raw SQL string literals in `internal/db/repo.go`

`OrderRepo.findByID`, `listByCustomer`, `insert`, `update`, and
`cancelByID` all carry inline SQL of the form
`"SELECT * FROM orders WHERE id = $1"`,
`"INSERT INTO orders (id, customer_id, total_cents, status) VALUES ($1, $2, $3, $4)"`,
etc. The enricher's sqlglot-lite parser should pull `orders` out as a
`db_table` attribute attached to those methods.

### 6. Env-var reads in `internal/config/config.go`

`Load()` calls `os.Getenv("HTTP_PORT")`, `os.Getenv("JWT_SECRET")`,
`os.Getenv("DATABASE_URL")`, etc. Each is a known input shape for the
`env_var` enricher.

## Required exported symbols

Future eval queries pin against these qualified names. They must exist; if
you rename anything below, also update `expected_symbols.json` (task 5.4):

- `go:cmd/server/main.go:setupRouter`
- `go:cmd/server/main.go:main`
- `go:internal/handlers/orders.go:OrdersHandler`
- `go:internal/handlers/orders.go:OrdersHandler.CreateOrder`
- `go:internal/handlers/orders.go:OrdersHandler.UpdateOrder`
- `go:internal/handlers/orders.go:OrdersHandler.CancelOrder`
- `go:internal/handlers/orders.go:OrdersHandler.GetOrder`
- `go:internal/handlers/legacy.go:LegacyHandler`
- `go:internal/middleware/ratelimit.go:NewRateLimiter`
- `go:internal/middleware/ratelimit.go:NewJWTGuard`
- `go:internal/middleware/logging.go:NewRequestLogger`
- `go:internal/middleware/logging.go:NewAccessLog`
- `go:internal/validation/orders.go:ValidateOrder`
- `go:internal/db/repo.go:OrderRepo`
- `go:internal/db/repo.go:AuditSink`
- `go:internal/config/config.go:Load`
- `go:internal/config/config.go:Config`
- `go:internal/auth/jwt.go:ValidateJWT`
- `go:internal/auth/jwt.go:Validator`

## Why this isn't built

`go.mod` declares deps on gin / golang-jwt / google-uuid but CI never runs
`go build` or `go mod download` on this directory. The cognis test suite
parses the source with tree-sitter-go; runtime imports never resolve.
Keeping the fixture parse-clean is sufficient — runtime behaviour isn't
asserted.

The directory is excluded from `ruff` (via `extend-exclude` in the cognis
top-level `pyproject.toml`), `pytest`'s `testpaths`, and is not on any Go
toolchain path.
