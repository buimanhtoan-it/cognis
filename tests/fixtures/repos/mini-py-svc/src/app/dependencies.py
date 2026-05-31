"""FastAPI dependency wiring.

Exposes `get_settings`, `get_engine_dep`, `get_session`, `get_current_user`,
and `require_role`. Routes consume these via FastAPI's `Depends()`.
"""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from fastapi import Depends, Header, HTTPException, Request, status

from app.config import Settings

if TYPE_CHECKING:
    from collections.abc import Iterator


@dataclass(slots=True)
class RequestScope:
    """Per-request state passed through middlewares."""

    settings: Settings
    request_id: str
    user_id: str | None = None
    extra: dict[str, Any] | None = None

    def close(self) -> None:
        if not self.extra:
            return
        for value in list(self.extra.values()):
            close_fn = getattr(value, "close", None)
            if callable(close_fn):
                try:
                    close_fn()
                except Exception:  # noqa: BLE001
                    pass
        self.extra.clear()


def build_request_scope(request: Request, settings: Settings) -> RequestScope:
    """Assemble a `RequestScope` for the inbound request."""
    import secrets as _secrets
    import time as _time

    request_id = (
        request.headers.get(settings.request_id_header, "")
        or f"req-{int(_time.time() * 1000)}-{_secrets.token_hex(4)}"
    )
    return RequestScope(settings=settings, request_id=request_id, extra={})


def get_settings(request: Request) -> Settings:
    """Yield the typed app settings off `app.state`."""
    settings: Settings | None = getattr(request.app.state, "settings", None)
    if settings is None:
        raise HTTPException(status_code=status.HTTP_503_SERVICE_UNAVAILABLE, detail="no settings")
    return settings


def get_engine_dep(request: Request) -> Any:
    """Yield the SQLAlchemy engine off `app.state`."""
    engine = getattr(request.app.state, "engine", None)
    if engine is None:
        raise HTTPException(status_code=status.HTTP_503_SERVICE_UNAVAILABLE, detail="no engine")
    return engine


def get_session(engine: Any = Depends(get_engine_dep)) -> "Iterator[Any]":
    """Yield a connection-scoped DB session for the request lifetime."""
    @contextmanager
    def _scope() -> "Iterator[Any]":
        connection = engine.connect()
        try:
            tx = connection.begin()
            try:
                yield connection
                tx.commit()
            except Exception:
                tx.rollback()
                raise
        finally:
            connection.close()

    with _scope() as session:
        yield session


def get_bearer_token(authorization: str | None = Header(default=None)) -> str:
    """Pull the bearer token off the `Authorization` header."""
    if not authorization:
        raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="missing auth")
    parts = authorization.split(" ", 1)
    if len(parts) != 2 or parts[0].lower() != "bearer" or not parts[1]:
        raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="malformed auth")
    return parts[1]


async def get_current_user(
    token: str = Depends(get_bearer_token), settings: Settings = Depends(get_settings)
) -> dict[str, Any]:
    """Decode the bearer JWT and return its claims."""
    from app.security import JwtError, decode_jwt
    try:
        return decode_jwt(token, settings)
    except JwtError as exc:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail={"code": exc.code, "message": exc.message},
        ) from exc


def require_role(role: str) -> Any:
    """Dependency factory — require `role` in the JWT's roles claim."""
    async def checker(user: dict[str, Any] = Depends(get_current_user)) -> dict[str, Any]:
        if role not in (user.get("roles") or []):
            raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail="forbidden")
        return user
    return checker
