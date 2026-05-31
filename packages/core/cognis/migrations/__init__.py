"""SQL migration files for the cognis UCKG store.

Migrations are plain ``.sql`` files named ``NNN_<slug>.sql``. The runner in
:mod:`cognis.db` reads them in lexicographic order and applies any whose
prefix is greater than the current ``meta.schema_version`` inside a single
transaction. See :func:`cognis.db.run_migrations`.
"""
