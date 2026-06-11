"""Unit tests for dynamic embedding-dimension reconciliation.

When a model with a different vector size is plugged in, ``Database`` must
recreate the ``symbol_vec`` table at the new dimension and persist it in
``meta`` so every later connection agrees. These tests exercise that contract
on the sqlite-vec backend (skipped when the extension is unavailable).
"""

from __future__ import annotations

from pathlib import Path

import pytest
from cognis.db import (
    EMBEDDING_DIM,
    EMBEDDING_DIM_META_KEY,
    Database,
    _read_meta,
    _read_vec_table_dim,
)

# vec0 virtual table is only created when sqlite-vec loads.
pytest.importorskip("sqlite_vec")


def _vec_enabled_db(tmp_path: Path) -> Database:
    db = Database(tmp_path / "uckg.db")
    if not db.vec_enabled:
        pytest.skip("sqlite-vec extension not loadable in this environment")
    db.connect()  # force schema + vec table creation
    return db


def test_fresh_db_defaults_to_384(tmp_path: Path) -> None:
    db = _vec_enabled_db(tmp_path)
    assert _read_vec_table_dim(db.connect()) == EMBEDDING_DIM


def test_reconcile_to_new_dim_recreates_table(tmp_path: Path) -> None:
    db = _vec_enabled_db(tmp_path)

    changed = db.reconcile_embedding_dim(1024)
    assert changed is True

    conn = db.connect()
    assert _read_vec_table_dim(conn) == 1024
    assert _read_meta(conn, EMBEDDING_DIM_META_KEY, str(EMBEDDING_DIM)) == "1024"


def test_reconcile_to_same_dim_is_noop(tmp_path: Path) -> None:
    db = _vec_enabled_db(tmp_path)
    assert db.reconcile_embedding_dim(EMBEDDING_DIM) is False


def test_persisted_dim_survives_new_connection(tmp_path: Path) -> None:
    db_path = tmp_path / "uckg.db"
    db = Database(db_path)
    if not db.vec_enabled:
        pytest.skip("sqlite-vec extension not loadable in this environment")
    db.connect()
    db.reconcile_embedding_dim(768)
    db.close_thread_connection()

    # A brand-new handle must rebuild the vec table at the persisted dimension.
    reopened = Database(db_path)
    assert _read_vec_table_dim(reopened.connect()) == 768
