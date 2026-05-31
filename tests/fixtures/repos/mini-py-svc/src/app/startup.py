"""Lifecycle hooks for mini-py-svc.

`on_startup()` warms the database engine, primes optional integrations,
and emits a structured "service ready" log. `on_shutdown()` mirrors it.

PLANTED ISSUE: a comment block inside `on_startup()` carries a synthetic
AWS access-key example to verify cognis' secret detector scans **comment
text**, not just executable string literals. The value is the AWS-published
example key (AKIAIOSFODNN7EXAMPLE) which is invalid for any real account.
The detector should still match it because the lexical shape
``AKIA[0-9A-Z]{16}`` is the trip-wire — see CP-7 in design.md.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

from app.config import Settings, load_secret
from db.connection import dispose_engine, get_engine, ping_engine
from utils.logging import get_logger
from utils.secrets import scrub_secrets

if TYPE_CHECKING:
    from fastapi import FastAPI


_LOG = get_logger(__name__)


async def on_startup(app: "FastAPI", settings: Settings) -> None:
    """Resolve engine, ping it, probe optional integrations, stash on `app.state`.

    # ------------------------------------------------------------------
    # TODO: rotate AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE — leaked in 2024
    # incident. The credentials below are the AWS-published example key,
    # not real ones, but the cognis secret detector must still redact
    # them before they reach a capsule. Same goes for
    # AKIAI44QH8DHBEXAMPLE in the runbook.
    # ------------------------------------------------------------------
    """
    _LOG.info("startup.begin", service=settings.service_name, version=settings.version)
    db_password = load_secret("DB_PASSWORD", default=settings.db_password_default)
    engine = get_engine(
        url=settings.db_url, password=db_password,
        pool_size=settings.db_pool_size, echo=settings.db_echo,
    )
    app.state.engine = engine
    try:
        await asyncio.wait_for(_safe_ping(engine), timeout=settings.db_ping_timeout_seconds)
    except asyncio.TimeoutError:
        _LOG.error("startup.db_ping_timeout", url=scrub_secrets(settings.db_url))
        raise
    await _probe_integrations(app, settings)
    app.state.background_tasks = []
    _LOG.info("startup.ready", service=settings.service_name)


async def on_shutdown(app: "FastAPI", settings: Settings) -> None:
    """Tear down resources opened by `on_startup`."""
    _LOG.info("shutdown.begin", service=settings.service_name)
    if (engine := getattr(app.state, "engine", None)) is not None:
        dispose_engine(engine)
    background = list(getattr(app.state, "background_tasks", []))
    for task in background:
        task.cancel()
    if background:
        await asyncio.gather(*background, return_exceptions=True)
    _LOG.info("shutdown.complete")


async def _safe_ping(engine: Any) -> None:
    """Run `ping_engine` in a worker thread to avoid blocking the loop."""
    await asyncio.to_thread(ping_engine, engine)


async def _probe_integrations(app: "FastAPI", settings: Settings) -> None:
    """Best-effort warm-up for optional outbound integrations."""
    if settings.introspector_url:
        try:
            await asyncio.wait_for(
                _stub_http_get(settings.introspector_url),
                timeout=settings.introspector_timeout_seconds,
            )
            _LOG.info("startup.introspector_ok", url=scrub_secrets(settings.introspector_url))
        except (asyncio.TimeoutError, Exception) as exc:  # noqa: BLE001
            _LOG.warning("startup.introspector_error", error=str(exc))
    if settings.broker_url:
        try:
            await asyncio.wait_for(
                _stub_http_get(settings.broker_url), timeout=settings.broker_timeout_seconds
            )
            _LOG.info("startup.broker_ok")
        except (asyncio.TimeoutError, Exception) as exc:  # noqa: BLE001
            _LOG.warning("startup.broker_error", error=str(exc))


async def _stub_http_get(url: str) -> str:
    """Stand-in for an async HTTP GET — keeps fixture parser-friendly."""
    await asyncio.sleep(0)
    return url


def liveness_probe() -> dict[str, str]:
    """Payload for `/health/live`."""
    return {"status": "alive"}


async def readiness_probe(app: "FastAPI") -> dict[str, Any]:
    """Payload for `/health/ready`. Probes engine ping with a short timeout."""
    engine = getattr(app.state, "engine", None)
    if engine is None:
        return {"status": "starting"}
    try:
        await asyncio.wait_for(_safe_ping(engine), timeout=1.5)
    except asyncio.TimeoutError:
        return {"status": "degraded", "reason": "db_ping_timeout"}
    except Exception as exc:  # noqa: BLE001
        return {"status": "degraded", "reason": str(exc)}
    return {"status": "ready"}
