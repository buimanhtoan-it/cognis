"""Configuration loader for mini-py-svc.

`load_settings()` returns a Pydantic-Settings-style `Settings` object
built from environment variables. `load_secret()` is a thin helper for
reading sensitive values; in real use it would consult a vault.

No real secrets are embedded. Placeholder strings are clearly marked
fake and exist so cognis' detector has shapes to match.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field


@dataclass(slots=True)
class Settings:
    """Typed runtime configuration."""

    service_name: str = "mini-py-svc"
    version: str = "0.1.0"
    log_level: str = "info"
    host: str = "0.0.0.0"
    port: int = 8000
    expose_docs: bool = True
    db_url: str = "postgresql+psycopg://app@localhost:5432/app"
    db_password_default: str = "REDACT_ME_PLACEHOLDER_pwfixture0001"  # fake — fixture only
    db_pool_size: int = 10
    db_echo: bool = False
    db_ping_timeout_seconds: float = 3.0
    jwt_issuer: str = "mini-py-svc"
    jwt_audience: str = "mini-py-svc-clients"
    jwt_algorithm: str = "HS256"
    jwt_access_ttl_seconds: int = 900
    jwt_refresh_ttl_seconds: int = 604_800
    introspector_url: str | None = None
    introspector_timeout_seconds: float = 1.5
    broker_url: str | None = None
    broker_timeout_seconds: float = 1.5
    rate_limit_per_minute: int = 120
    request_id_header: str = "x-request-id"
    flags: dict[str, bool] = field(default_factory=dict)


def load_settings(env: dict[str, str] | None = None) -> Settings:
    """Build a `Settings` instance from `env` (defaults to `os.environ`)."""
    e: dict[str, str] = dict(env if env is not None else os.environ)
    flags = {
        k.removeprefix("FEATURE_").lower(): v.lower() in {"1", "true", "yes", "on"}
        for k, v in e.items() if k.startswith("FEATURE_")
    }
    return Settings(
        service_name=e.get("SERVICE_NAME", "mini-py-svc"),
        version=e.get("SERVICE_VERSION", "0.1.0"),
        log_level=e.get("LOG_LEVEL", "info"),
        host=e.get("HOST", "0.0.0.0"),
        port=_int(e.get("PORT"), 8000),
        expose_docs=_bool(e.get("EXPOSE_DOCS"), True),
        db_url=e.get("DATABASE_URL", "postgresql+psycopg://app@localhost:5432/app"),
        db_password_default=e.get(
            "DB_PASSWORD_DEFAULT",
            "REDACT_ME_PLACEHOLDER_pwfixture0001",  # fake — fixture only
        ),
        db_pool_size=_int(e.get("DB_POOL_SIZE"), 10),
        db_echo=_bool(e.get("DB_ECHO"), False),
        db_ping_timeout_seconds=_float(e.get("DB_PING_TIMEOUT_SECONDS"), 3.0),
        jwt_issuer=e.get("JWT_ISSUER", "mini-py-svc"),
        jwt_audience=e.get("JWT_AUDIENCE", "mini-py-svc-clients"),
        jwt_algorithm=e.get("JWT_ALGORITHM", "HS256"),
        jwt_access_ttl_seconds=_int(e.get("JWT_ACCESS_TTL_SECONDS"), 900),
        jwt_refresh_ttl_seconds=_int(e.get("JWT_REFRESH_TTL_SECONDS"), 604_800),
        introspector_url=e.get("TOKEN_INTROSPECTOR_URL") or None,
        introspector_timeout_seconds=_float(e.get("TOKEN_INTROSPECTOR_TIMEOUT_SECONDS"), 1.5),
        broker_url=e.get("BROKER_URL") or None,
        broker_timeout_seconds=_float(e.get("BROKER_TIMEOUT_SECONDS"), 1.5),
        rate_limit_per_minute=_int(e.get("RATE_LIMIT_PER_MINUTE"), 120),
        request_id_header=e.get("REQUEST_ID_HEADER", "x-request-id"),
        flags=flags,
    )


def load_secret(name: str, default: str | None = None, env: dict[str, str] | None = None) -> str:
    """Read a sensitive value with the same shape as a vault lookup.

    Resolution order: ``env[name]`` → ``env[name + "_FILE"]`` → ``default``.
    Raises ``KeyError`` when nothing resolves.
    """
    e: dict[str, str] = dict(env if env is not None else os.environ)
    if direct := e.get(name):
        return direct
    if file_path := e.get(f"{name}_FILE"):
        try:
            with open(file_path, encoding="utf-8") as fh:
                return fh.read().strip()
        except OSError as exc:
            raise KeyError(f"secret {name} unreadable: {exc}") from exc
    if default is not None:
        return default
    raise KeyError(f"secret {name} not configured")


def _int(raw: str | None, fallback: int) -> int:
    try:
        return int(raw) if raw else fallback
    except ValueError:
        return fallback


def _float(raw: str | None, fallback: float) -> float:
    try:
        return float(raw) if raw else fallback
    except ValueError:
        return fallback


def _bool(raw: str | None, fallback: bool) -> bool:
    return fallback if not raw else raw.lower() in {"1", "true", "yes", "on"}
