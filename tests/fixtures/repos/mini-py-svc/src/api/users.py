"""`/users` endpoints — read/write the `users` table via `db.users_repo`."""

from __future__ import annotations

import time
import uuid
from typing import TYPE_CHECKING, Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request, status
from pydantic import BaseModel, EmailStr, Field

from app.config import Settings
from app.dependencies import get_current_user, get_engine_dep, get_settings, require_role
from app.security import hash_password
from db.users_repo import (
    User, count_users, count_users_by_role, delete_user, get_user,
    list_users, update_user_password, upsert_user,
)
from utils.logging import get_logger

if TYPE_CHECKING:
    from sqlalchemy.engine import Engine


_LOG = get_logger(__name__)
router = APIRouter()


class UserPublic(BaseModel):
    id: str
    username: str
    email: str
    role: str
    created_at: int
    updated_at: int


class CreateUserRequest(BaseModel):
    username: str = Field(min_length=1, max_length=80)
    email: EmailStr
    password: str = Field(min_length=8, max_length=256)
    role: str = Field(default="user", max_length=32)


class UpdateUserRequest(BaseModel):
    username: str | None = Field(default=None, min_length=1, max_length=80)
    email: EmailStr | None = None
    role: str | None = Field(default=None, max_length=32)


class ChangePasswordRequest(BaseModel):
    new_password: str = Field(min_length=8, max_length=256)


class UserListResponse(BaseModel):
    items: list[UserPublic]
    total: int
    limit: int
    offset: int


def _not_found() -> HTTPException:
    return HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="not found")


@router.get("", response_model=UserListResponse)
async def list_users_route(
    request: Request,
    limit: int = Query(default=50, ge=1, le=200),
    offset: int = Query(default=0, ge=0),
    role: str | None = Query(default=None, max_length=32),
    engine: "Engine" = Depends(get_engine_dep),
    _admin: dict[str, Any] = Depends(require_role("admin")),
) -> UserListResponse:
    """List users — admin-only."""
    rows = list_users(engine, limit=limit, offset=offset, role=role)
    return UserListResponse(
        items=[UserPublic(**u.to_public()) for u in rows],
        total=count_users(engine), limit=limit, offset=offset,
    )


@router.get("/me", response_model=UserPublic)
async def me(
    user: dict[str, Any] = Depends(get_current_user),
    engine: "Engine" = Depends(get_engine_dep),
) -> UserPublic:
    """Return the bearer's own user record."""
    record = get_user(engine, str(user.get("sub", "")))
    if record is None:
        raise _not_found()
    return UserPublic(**record.to_public())


@router.get("/{user_id}", response_model=UserPublic)
async def get_user_route(
    user_id: str,
    engine: "Engine" = Depends(get_engine_dep),
    _admin: dict[str, Any] = Depends(require_role("admin")),
) -> UserPublic:
    """Look up a single user by id."""
    record = get_user(engine, user_id)
    if record is None:
        raise _not_found()
    return UserPublic(**record.to_public())


@router.post("", response_model=UserPublic, status_code=status.HTTP_201_CREATED)
async def create_user_route(
    payload: CreateUserRequest,
    settings: Settings = Depends(get_settings),
    engine: "Engine" = Depends(get_engine_dep),
    _admin: dict[str, Any] = Depends(require_role("admin")),
) -> UserPublic:
    """Create a user — admin-only."""
    now_ts = int(time.time())
    saved = upsert_user(engine, User(
        id=str(uuid.uuid4()), username=payload.username, email=str(payload.email),
        password_hash=hash_password(payload.password), role=payload.role,
        created_at=now_ts, updated_at=now_ts,
    ))
    _LOG.info("users.create", user_id=saved.id, role=saved.role)
    _ = settings  # surface dependency edge.
    return UserPublic(**saved.to_public())


@router.patch("/{user_id}", response_model=UserPublic)
async def update_user_route(
    user_id: str,
    payload: UpdateUserRequest,
    engine: "Engine" = Depends(get_engine_dep),
    _admin: dict[str, Any] = Depends(require_role("admin")),
) -> UserPublic:
    """Patch user fields — admin-only."""
    record = get_user(engine, user_id)
    if record is None:
        raise _not_found()
    if payload.username is not None:
        record.username = payload.username
    if payload.email is not None:
        record.email = str(payload.email)
    if payload.role is not None:
        record.role = payload.role
    record.updated_at = int(time.time())
    upsert_user(engine, record)
    return UserPublic(**record.to_public())


@router.put("/{user_id}/password", status_code=status.HTTP_204_NO_CONTENT)
async def change_password_route(
    user_id: str,
    payload: ChangePasswordRequest,
    user: dict[str, Any] = Depends(get_current_user),
    engine: "Engine" = Depends(get_engine_dep),
) -> None:
    """Change the password for `user_id` (self-service or admin)."""
    if str(user.get("sub", "")) != user_id and "admin" not in (user.get("roles") or []):
        raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail="forbidden")
    if not update_user_password(engine, user_id, hash_password(payload.new_password), int(time.time())):
        raise _not_found()
    return None


@router.delete("/{user_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_user_route(
    user_id: str,
    engine: "Engine" = Depends(get_engine_dep),
    _admin: dict[str, Any] = Depends(require_role("admin")),
) -> None:
    """Delete a user — admin-only."""
    if not delete_user(engine, user_id):
        raise _not_found()
    _LOG.info("users.delete", user_id=user_id)
    return None


@router.get("/stats/by-role")
async def stats_by_role(
    engine: "Engine" = Depends(get_engine_dep),
    _admin: dict[str, Any] = Depends(require_role("admin")),
) -> dict[str, int]:
    """Histogram of users by role — admin-only."""
    return count_users_by_role(engine)
