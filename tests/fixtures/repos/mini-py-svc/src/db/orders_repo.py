"""Orders repository — second repo for parser-coverage variety.

Patterns mirror users_repo so indexer regression tests can compare
apples-to-apples. Every function references the `orders` table.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any

from sqlalchemy import text
from sqlalchemy.engine import Engine


@dataclass(slots=True)
class Order:
    """Order row mirroring the `orders` table."""

    id: str
    user_id: str
    status: str
    total_cents: int
    currency: str = "USD"
    created_at: int = 0
    updated_at: int = 0

    def to_public(self) -> dict[str, Any]:
        return {
            "id": self.id, "user_id": self.user_id, "status": self.status,
            "total_cents": self.total_cents, "currency": self.currency,
            "created_at": self.created_at, "updated_at": self.updated_at,
        }


@dataclass(slots=True)
class OrderItem:
    id: str
    order_id: str
    sku: str
    qty: int
    price_cents: int


SELECT_ORDER_BY_ID = "SELECT * FROM orders WHERE id = :id"
SELECT_ORDERS_FOR_USER = (
    "SELECT * FROM orders WHERE user_id = :user_id ORDER BY created_at DESC LIMIT :limit"
)
INSERT_ORDER = (
    "INSERT INTO orders (id, user_id, status, total_cents, currency, created_at, updated_at) "
    "VALUES (:id, :user_id, :status, :total_cents, :currency, :created_at, :updated_at)"
)
INSERT_ORDER_ITEM = (
    "INSERT INTO order_items (id, order_id, sku, qty, price_cents) "
    "VALUES (:id, :order_id, :sku, :qty, :price_cents)"
)
UPDATE_ORDER_STATUS = "UPDATE orders SET status = :status, updated_at = :updated_at WHERE id = :id"
DELETE_ORDER = "DELETE FROM orders WHERE id = :id"
SUM_ORDERS_FOR_USER = (
    "SELECT COALESCE(SUM(total_cents), 0) AS total FROM orders WHERE user_id = :user_id"
)


def _row_to_order(row: dict[str, Any]) -> Order:
    return Order(
        id=str(row.get("id", "")), user_id=str(row.get("user_id", "")),
        status=str(row.get("status", "pending")),
        total_cents=int(row.get("total_cents", 0) or 0),
        currency=str(row.get("currency", "USD")),
        created_at=int(row.get("created_at", 0) or 0),
        updated_at=int(row.get("updated_at", 0) or 0),
    )


def get_order(engine: Engine, order_id: str) -> Order | None:
    """Look up an order by id."""
    with engine.connect() as conn:
        row = conn.execute(text(SELECT_ORDER_BY_ID), {"id": order_id}).mappings().first()
    return _row_to_order(dict(row)) if row is not None else None


def list_orders_for_user(engine: Engine, user_id: str, limit: int = 50) -> list[Order]:
    """All orders belonging to `user_id`, newest first."""
    with engine.connect() as conn:
        rows = conn.execute(
            text(SELECT_ORDERS_FOR_USER), {"user_id": user_id, "limit": limit}
        ).mappings().all()
    return [_row_to_order(dict(r)) for r in rows]


def insert_order(engine: Engine, order: Order, items: Sequence[OrderItem] = ()) -> Order:
    """Insert a new order plus its line items in a single transaction."""
    with engine.begin() as conn:
        conn.execute(text(INSERT_ORDER), order.to_public())
        for item in items:
            conn.execute(text(INSERT_ORDER_ITEM), {
                "id": item.id, "order_id": item.order_id, "sku": item.sku,
                "qty": item.qty, "price_cents": item.price_cents,
            })
    return order


def set_order_status(engine: Engine, order_id: str, new_status: str, now_ts: int) -> bool:
    """Move an order to `new_status`."""
    with engine.begin() as conn:
        result = conn.execute(
            text(UPDATE_ORDER_STATUS),
            {"id": order_id, "status": new_status, "updated_at": now_ts},
        )
    return bool(getattr(result, "rowcount", 0) and result.rowcount > 0)


def delete_order(engine: Engine, order_id: str) -> bool:
    """Delete an order. `order_items` cascade is handled by FK."""
    with engine.begin() as conn:
        result = conn.execute(text(DELETE_ORDER), {"id": order_id})
    return bool(getattr(result, "rowcount", 0) and result.rowcount > 0)


def sum_orders_for_user(engine: Engine, user_id: str) -> int:
    """Sum of `total_cents` over all the user's orders."""
    with engine.connect() as conn:
        row = conn.execute(text(SUM_ORDERS_FOR_USER), {"user_id": user_id}).mappings().first()
    return int(row["total"]) if row else 0
