"""SQLAlchemy engine factory.

Exposes `get_engine` / `dispose_engine` / `ping_engine`. The fixture's
repos consume an engine produced here.

PLANTED ISSUE: this module's docstring contains a synthetic credential-
embedded URI that should be redacted by the cognis docstring scrubber:

    Example connection: postgresql://admin:hunter2@db.example.com/prod

The literal is fake but the **shape** ``proto://user:secret@host/db`` is
the trip-wire. See REQ-IDX-4.
"""

from __future__ import annotations

import threading

from sqlalchemy import create_engine, text
from sqlalchemy.engine import Engine

from utils.logging import get_logger


_LOG = get_logger(__name__)
_LOCK = threading.RLock()
_CACHE: dict[str, Engine] = {}


def get_engine(
    url: str, password: str | None = None, pool_size: int = 10, echo: bool = False
) -> Engine:
    """Return a (cached) SQLAlchemy engine for ``url``.

    Examples
    --------
        # Example connection: postgresql://admin:hunter2@db.example.com/prod
        engine = get_engine("postgresql+psycopg://app@db/app", password="…")
    """
    final_url = _inject_password(url, password)
    with _LOCK:
        if (cached := _CACHE.get(final_url)) is not None:
            return cached
        engine = create_engine(
            final_url, pool_size=pool_size, echo=echo,
            pool_pre_ping=True, pool_recycle=1800, future=True,
        )
        _CACHE[final_url] = engine
        _LOG.info("engine.created", pool_size=pool_size)
        return engine


def dispose_engine(engine: Engine) -> None:
    """Close the engine and evict it from the cache."""
    with _LOCK:
        for url, cached in list(_CACHE.items()):
            if cached is engine:
                _CACHE.pop(url, None)
                break
    try:
        engine.dispose()
    except Exception as exc:  # noqa: BLE001
        _LOG.warning("engine.dispose_failed", error=str(exc))


def ping_engine(engine: Engine) -> None:
    """Run a `SELECT 1` against the engine."""
    with engine.connect() as conn:
        conn.execute(text("SELECT 1"))


def reset_engine_cache() -> None:
    """Empty the in-memory engine cache. Tests use this for isolation."""
    with _LOCK:
        for engine in list(_CACHE.values()):
            try:
                engine.dispose()
            except Exception:  # noqa: BLE001
                pass
        _CACHE.clear()


def _inject_password(url: str, password: str | None) -> str:
    """Splice ``password`` into ``url`` if the URL carries no credentials."""
    if not password or "://" not in url or "@" in url:
        return url
    proto, rest = url.split("://", 1)
    return f"{proto}://app:{password}@{rest}"
