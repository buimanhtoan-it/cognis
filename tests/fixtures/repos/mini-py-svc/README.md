# mini-py-svc — cognis test fixture

A deliberately small FastAPI + SQLAlchemy service used by the `cognis` test
suite. It is **not** intended to run; it exists so the indexer, enricher,
secret detector, and capsule composer have a realistic-shaped Python repo to
parse and probe.

## Layout

```
mini-py-svc/
├── pyproject.toml          declared deps (NOT installed in CI)
├── README.md               you are here
└── src/
    ├── app/
    │   ├── __init__.py
    │   ├── main.py         create_app() — assembles FastAPI instance
    │   ├── startup.py      on_startup() — lifecycle hook (PLANTED env-var leak)
    │   ├── config.py       load_settings() + load_secret() helpers
    │   ├── dependencies.py FastAPI Depends wiring
    │   └── security.py     JWT decode helper (placeholder strings only)
    ├── db/
    │   ├── __init__.py
    │   ├── connection.py   get_engine() — SQLAlchemy engine factory
    │   ├── users_repo.py   get_user / list_users / upsert_user
    │   └── orders_repo.py  second repo for parser-coverage variety
    ├── api/
    │   ├── __init__.py
    │   ├── users.py        APIRouter — /users
    │   ├── health.py       APIRouter — /health
    │   └── auth.py         APIRouter — /auth/login (uses security.py)
    └── utils/
        ├── __init__.py
        ├── logging.py      structlog-style logger setup
        └── secrets.py      env-var loader + planted secret-shaped strings
```

## Planted issues (and what each is for)

This fixture intentionally embeds patterns that exercise specific cognis
subsystems. Every "leak" or "secret" is a **synthetic shape**, never a real
credential. They are checked in deliberately so the redaction PBT and
enricher coverage tests have inputs.

### 1. Env-var leak in a comment block (`src/app/startup.py`)

`on_startup()` carries a TODO comment of the form

```python
# TODO: rotate AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE — leaked in 2024 incident
```

This exercises the secret detector's comment scanning (CP-7 in design.md):
the AWS access-key shape `AKIA[0-9A-Z]{16}` matches even though the value is
the AWS-published *example* key. The test asserts the redactor scrubs it
from any `body_excerpt` or attribute payload.

### 2. Secret-shaped string literals (`src/utils/secrets.py`)

`utils/secrets.py` collects, **as Python string literals**, examples of:

- AWS access keys: `AKIA[0-9A-Z]{16}`
- OpenAI keys: `sk-[A-Za-z0-9]{20,}`
- JWT bearer tokens: `eyJ[A-Za-z0-9_=-]+\.[A-Za-z0-9_=-]+\.?[A-Za-z0-9_.+/=-]*`
- Inline `password = "..."` assignments
- PEM block headers: `-----BEGIN RSA PRIVATE KEY-----` / `-----BEGIN OPENSSH PRIVATE KEY-----`

Every one is annotated with a `# fake — fixture only` neighbour comment so
human readers can confirm they're synthetic. The lexical *shape* is what
the cognis secret detector trips on, regardless of the disclaimers.

### 3. Secret-shaped DB URI in a docstring (`src/db/connection.py`)

The module docstring contains the string

```
# Example connection: postgresql://admin:hunter2@db.example.com/prod
```

which exercises detection of credentials embedded in connection strings
(also used by docstring-redaction in REQ-IDX-4).

### 4. Placeholder JWT secret (`src/app/security.py`)

A module-level constant `JWT_SECRET = "REDACT_ME_PLACEHOLDER_aaaa1111bbbb2222"`
is **not** a real secret but matches a high-entropy alphanumeric pattern, so
it should round-trip through the entropy detector and end up redacted in
any capsule.

### 5. Raw SQL string literals for `db_table` extraction

`src/db/users_repo.py` and `src/db/orders_repo.py` call
`connection.execute(text("SELECT * FROM users WHERE id = :id"))` and
similar. The enricher's sqlglot-lite parser should pull `users` / `orders`
out as `db_table` attributes attached to those functions.

## Required exported symbols

Future eval queries pin against these qualified names. They must exist; if
you rename anything below, also update `expected_symbols.json` (task 5.4):

- `py:src/app/startup.py:on_startup`
- `py:src/app/config.py:load_settings`
- `py:src/app/config.py:load_secret`
- `py:src/db/connection.py:get_engine`
- `py:src/db/users_repo.py:get_user`
- `py:src/db/users_repo.py:list_users`
- `py:src/db/users_repo.py:upsert_user`

## Why this isn't installed

`pyproject.toml` lists fastapi/sqlalchemy/pydantic but CI never runs
`pip install` on this directory. The cognis test suite parses the source
with tree-sitter-python; runtime imports never resolve. Keeping the
fixture parse-clean is sufficient — runtime behaviour isn't asserted.

The directory is excluded from `ruff` (via `extend-exclude` in the cognis
top-level `pyproject.toml`), `mypy` (via `[tool.mypy].exclude`), and is not
on `pytest`'s `testpaths`.
