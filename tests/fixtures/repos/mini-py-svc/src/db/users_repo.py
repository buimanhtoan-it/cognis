"""Users repository.

Functions in this module use raw SQL string literals so the cognis
enricher can extract `db_table=users` attributes from each callable.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from sqlalchemy import text
from sqlalchemy.engine import Engine


@dataclass(slots=True)
class User:
    """Lightweight user record mirroring the `users` table."""

    id: str
    username: str
    email: str
    password_hash: str
    role: str = "user"
    created_at: int = 0
    updated_at: int = 0

    def to_public(self) -> dict[str, Any]:
        return {
            "id": self.id, "username": self.username, "email": self.email,
            "role": self.role, "created_at": self.created_at, "updated_at": self.updated_at,
        }


SELECT_USER_BY_ID = "SELECT * FROM users WHERE id = :id"
SELECT_USER_BY_USERNAME = "SELECT * FROM users WHERE username = :username"
SELECT_ALL_USERS = "SELECT * FROM users ORDER BY created_at DESC LIMIT :limit OFFSET :offset"
SELECT_USERS_BY_ROLE = "SELECT * FROM users WHERE role = :role ORDER BY username"
INSERT_USER = (
    "INSERT INTO users (id, username, email, password_hash, role, created_at, updated_at) "
    "VALUES (:id, :username, :email, :password_hash, :role, :created_at, :updated_at)"
)
UPDATE_USER = (
    "UPDATE users SET username = :username, email = :email, role = :role, "
    "updated_at = :updated_at WHERE id = :id"
)
UPDATE_USER_PASSWORD = (
    "UPDATE users SET password_hash = :password_hash, updated_at = :updated_at WHERE id = :id"
)
DELETE_USER = "DELETE FROM users WHERE id = :id"
COUNT_USERS = "SELECT COUNT(*) AS n FROM users"
COUNT_USERS_BY_ROLE = "SELECT role, COUNT(*) AS n FROM users GROUP BY role"


def _row_to_user(row: dict[str, Any]) -> User:
    return User(
        id=str(row.get("id", "")), username=str(row.get("username", "")),
        email=str(row.get("email", "")), password_hash=str(row.get("password_hash", "")),
        role=str(row.get("role", "user")),
        created_at=int(row.get("created_at", 0) or 0),
        updated_at=int(row.get("updated_at", 0) or 0),
    )


def get_user(engine: Engine, user_id: str) -> User | None:
    """Look up a user by id (`SELECT * FROM users WHERE id = :id`)."""
    with engine.connect() as conn:
        row = conn.execute(text(SELECT_USER_BY_ID), {"id": user_id}).mappings().first()
    return _row_to_user(dict(row)) if row is not None else None


def get_user_by_username(engine: Engine, username: str) -> User | None:
    """Look up a user by username — used by `auth.login`."""
    with engine.connect() as conn:
        row = conn.execute(text(SELECT_USER_BY_USERNAME), {"username": username}).mappings().first()
    return _row_to_user(dict(row)) if row is not None else None


def list_users(engine: Engine, limit: int = 50, offset: int = 0, role: str | None = None) -> list[User]:
    """List users, optionally filtered by role."""
    if limit <= 0:
        return []
    with engine.connect() as conn:
        if role is not None:
            rows = conn.execute(text(SELECT_USERS_BY_ROLE), {"role": role}).mappings().all()
            return [_row_to_user(dict(r)) for r in rows[offset : offset + limit]]
        rows = conn.execute(text(SELECT_ALL_USERS), {"limit": limit, "offset": offset}).mappings().all()
    return [_row_to_user(dict(r)) for r in rows]


def upsert_user(engine: Engine, user: User) -> User:
    """Insert or update `user` keyed on `user.id`."""
    payload = {
        "id": user.id, "username": user.username, "email": user.email,
        "password_hash": user.password_hash, "role": user.role,
        "created_at": user.created_at, "updated_at": user.updated_at,
    }
    is_insert = get_user(engine, user.id) is None
    with engine.begin() as conn:
        if is_insert:
            conn.execute(text(INSERT_USER), payload)
        else:
            conn.execute(text(UPDATE_USER), {k: payload[k] for k in ("id", "username", "email", "role", "updated_at")})
    return user


def delete_user(engine: Engine, user_id: str) -> bool:
    """Delete a user. Returns True when a row was removed."""
    with engine.begin() as conn:
        result = conn.execute(text(DELETE_USER), {"id": user_id})
    return bool(getattr(result, "rowcount", 0) and result.rowcount > 0)


def update_user_password(engine: Engine, user_id: str, password_hash: str, now_ts: int) -> bool:
    """Rotate the password hash for `user_id`."""
    with engine.begin() as conn:
        result = conn.execute(
            text(UPDATE_USER_PASSWORD),
            {"id": user_id, "password_hash": password_hash, "updated_at": now_ts},
        )
    return bool(getattr(result, "rowcount", 0) and result.rowcount > 0)


def count_users(engine: Engine) -> int:
    """Total user count."""
    with engine.connect() as conn:
        row = conn.execute(text(COUNT_USERS)).mappings().first()
    return int(row["n"]) if row else 0


def count_users_by_role(engine: Engine) -> dict[str, int]:
    """Per-role counts."""
    with engine.connect() as conn:
        rows = conn.execute(text(COUNT_USERS_BY_ROLE)).mappings().all()
    return {str(r["role"]): int(r["n"]) for r in rows}
