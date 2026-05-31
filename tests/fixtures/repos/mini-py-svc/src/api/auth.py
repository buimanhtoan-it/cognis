"""`/auth` endpoints — login, refresh, logout, whoami."""

from __future__ import annotations

import time
import uuid
from typing import TYPE_CHECKING, Any

from fastapi import APIRouter, Depends, HTTPException, Request, status
from pydantic import BaseModel, Field

from app.config import Settings
from app.dependencies import get_current_user, get_engine_dep, get_settings
from app.security import JwtClaims, encode_jwt, verify_password
from db.users_repo import get_user_by_username
from utils.logging import get_logger

if TYPE_CHECKING:
    from sqlalchemy.engine import Engine


_LOG = get_logger(__name__)
router = APIRouter()


class LoginRequest(BaseModel):
    username: str = Field(min_length=1, max_length=80)
    password: str = Field(min_length=1, max_length=256)


class TokenResponse(BaseModel):
    access_token: str
    refresh_token: str
    token_type: str = "bearer"
    expires_in: int


class RefreshRequest(BaseModel):
    refresh_token: str = Field(min_length=1, max_length=4096)


def _token_pair(sub: str, username: str, roles: list[str], settings: Settings) -> TokenResponse:
    """Mint an access + refresh token pair for the given identity."""
    access = encode_jwt(_build_claims(sub, username, roles, settings), settings)
    refresh_tok = encode_jwt(
        _build_claims(sub, username, roles, settings, ttl=settings.jwt_refresh_ttl_seconds),
        settings,
    )
    return TokenResponse(
        access_token=access, refresh_token=refresh_tok, expires_in=settings.jwt_access_ttl_seconds
    )


@router.post("/login", response_model=TokenResponse)
async def login(
    payload: LoginRequest,
    request: Request,
    settings: Settings = Depends(get_settings),
    engine: "Engine" = Depends(get_engine_dep),
) -> TokenResponse:
    """Authenticate by username/password and return a token pair."""
    user = get_user_by_username(engine, payload.username)
    if user is None or not verify_password(payload.password, user.password_hash):
        _LOG.info("auth.login.bad_credentials", username=payload.username)
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail={"code": "BAD_CREDENTIALS", "message": "invalid credentials"},
        )
    _LOG.info("auth.login.ok", user_id=user.id, request_id=getattr(request.state, "scope", None))
    return _token_pair(user.id, user.username, [user.role], settings)


@router.post("/refresh", response_model=TokenResponse)
async def refresh(
    payload: RefreshRequest,
    settings: Settings = Depends(get_settings),
    user: dict[str, Any] = Depends(get_current_user),
) -> TokenResponse:
    """Rotate the access token. The bearer header is also required."""
    if not payload.refresh_token:
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail="refresh required")
    return _token_pair(
        str(user.get("sub", "")), str(user.get("username", "")),
        list(user.get("roles") or []), settings,
    )


@router.post("/logout", status_code=status.HTTP_204_NO_CONTENT)
async def logout(user: dict[str, Any] = Depends(get_current_user)) -> None:
    """Best-effort logout — JWTs are stateless."""
    _LOG.info("auth.logout", user_id=user.get("sub"))
    return None


@router.get("/whoami")
async def whoami(user: dict[str, Any] = Depends(get_current_user)) -> dict[str, Any]:
    """Return the decoded claims for the current bearer."""
    return {"sub": user.get("sub"), "username": user.get("username"), "roles": user.get("roles") or []}


def _build_claims(
    sub: str, username: str, roles: list[str], settings: Settings, ttl: int | None = None
) -> JwtClaims:
    """Assemble a `JwtClaims` for the given user."""
    iat = int(time.time())
    return JwtClaims(
        sub=sub or str(uuid.uuid4()),
        username=username,
        roles=list(roles),
        iat=iat,
        exp=iat + (ttl if ttl is not None else settings.jwt_access_ttl_seconds),
        iss=settings.jwt_issuer,
        aud=settings.jwt_audience,
    )
