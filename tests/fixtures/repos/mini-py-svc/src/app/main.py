"""FastAPI app factory.

`create_app()` assembles a FastAPI instance from the routers in `src.api`
and wires the lifecycle hook in `src.app.startup`.

This file is part of the cognis test fixture mini-py-svc and is not
intended to run.
"""

from __future__ import annotations

from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any

from fastapi import FastAPI

from app.config import Settings, load_settings
from app.dependencies import build_request_scope
from app.startup import on_shutdown, on_startup
from utils.logging import bind_request_logger, configure_logging

if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from app.dependencies import RequestScope


@asynccontextmanager
async def lifespan(app: FastAPI) -> "AsyncIterator[None]":
    """Run startup hooks, then hand control back to FastAPI's runtime."""
    settings: Settings = app.state.settings
    await on_startup(app, settings)
    try:
        yield
    finally:
        await on_shutdown(app, settings)


def create_app(settings: Settings | None = None) -> FastAPI:
    """Construct and return a FastAPI app."""
    cfg: Settings = settings if settings is not None else load_settings()
    configure_logging(level=cfg.log_level, service=cfg.service_name)
    app = FastAPI(
        title=cfg.service_name,
        version=cfg.version,
        description="Cognis fixture: minimal FastAPI service with planted secret patterns.",
        lifespan=lifespan,
        docs_url="/docs" if cfg.expose_docs else None,
        redoc_url=None,
        openapi_url="/openapi.json" if cfg.expose_docs else None,
    )
    app.state.settings = cfg
    _register_middlewares(app, cfg)
    _register_routers(app)
    _register_exception_handlers(app)
    return app


def _register_middlewares(app: FastAPI, cfg: Settings) -> None:
    """Attach the request logger + per-request scope middlewares."""

    @app.middleware("http")
    async def request_logger_middleware(request: Any, call_next: Any) -> Any:
        with bind_request_logger(request, service=cfg.service_name) as request_log:
            request.state.log = request_log
            return await call_next(request)

    @app.middleware("http")
    async def request_scope_middleware(request: Any, call_next: Any) -> Any:
        scope: RequestScope = build_request_scope(request, cfg)
        request.state.scope = scope
        try:
            return await call_next(request)
        finally:
            scope.close()


def _register_routers(app: FastAPI) -> None:
    """Mount API routers under their respective prefixes."""
    from api.auth import router as auth_router
    from api.health import router as health_router
    from api.users import router as users_router

    app.include_router(health_router, prefix="", tags=["health"])
    app.include_router(auth_router, prefix="/auth", tags=["auth"])
    app.include_router(users_router, prefix="/users", tags=["users"])


def _register_exception_handlers(app: FastAPI) -> None:
    """Translate domain errors into HTTP envelopes, scrubbing secrets."""
    from utils.secrets import scrub_secrets

    @app.exception_handler(Exception)
    async def fallback_handler(request: Any, exc: Exception) -> Any:
        from fastapi.responses import JSONResponse

        request_log = getattr(request.state, "log", None)
        message = scrub_secrets(str(exc))
        if request_log is not None:
            request_log.error("unhandled.exception", message=message, kind=type(exc).__name__)
        return JSONResponse(
            status_code=500,
            content={"error": {"code": "INTERNAL_ERROR", "message": message}},
        )


def run() -> None:  # pragma: no cover
    """Stub runner; the fixture isn't actually launched in CI."""
    _ = load_settings()


if __name__ == "__main__":  # pragma: no cover
    run()
