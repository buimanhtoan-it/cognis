"""Append-only JSONL audit log for MCP tool calls.

Design reference: design.md §Error Handling → Audit log::

    Append-only JSONL at ``.cognis/audit.log``.
    Each entry: {"ts": "...", "tool": "...", "args_hash": "<sha256_of_args>", "ok": true/false}
    NEVER log the actual args (could contain secrets).

The audit log is written synchronously on every tool call. The file is opened
in append mode per entry so crashes between calls never corrupt existing
records.
"""

from __future__ import annotations

import hashlib
import json
import logging
import time
from pathlib import Path
from typing import Any

__all__ = ["DEFAULT_AUDIT_PATH", "audit_log_entry"]

logger = logging.getLogger(__name__)

DEFAULT_AUDIT_PATH = Path(".cognis") / "audit.log"
"""Default audit log path (relative to cwd / config-resolved at runtime)."""

_MAX_AUDIT_BYTES: int = 100 * 1024 * 1024  # 100 MiB — rotate when exceeded (docs/observability.md)


def audit_log_entry(
    tool: str,
    args: dict[str, Any],
    ok: bool,
    audit_path: Path = DEFAULT_AUDIT_PATH,
) -> None:
    """Append one audit entry to the JSONL audit log.

    Computes a SHA-256 hash of the canonical JSON serialization of *args*
    so calls can be identified without exposing argument values (which may
    contain secrets or sensitive query strings).

    Args:
        tool: Tool name (e.g. ``"symbol_lookup"``).
        args: The raw arguments dict passed to the tool.  NEVER logged directly.
        ok: ``True`` if the tool returned a valid result, ``False`` on error.
        audit_path: Path to the JSONL audit log file.  Parent directory is
            created automatically if it does not exist.

    Note:
        This function swallows all exceptions (``except Exception``) so that
        an audit log failure never prevents the tool from returning a result
        to the MCP client.  Failures are logged to the ``cognis_mcpd.audit``
        logger at WARNING level.
    """
    try:
        args_hash = hashlib.sha256(
            json.dumps(args, sort_keys=True, default=str).encode()
        ).hexdigest()
        entry = {
            "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "tool": tool,
            "args_hash": args_hash,
            "ok": ok,
        }
        # Ensure parent directory exists.
        audit_path.parent.mkdir(parents=True, exist_ok=True)
        if audit_path.exists() and audit_path.stat().st_size >= _MAX_AUDIT_BYTES:
            rotated = audit_path.with_suffix(audit_path.suffix + ".1")
            if rotated.exists():
                rotated.unlink()
            audit_path.rename(rotated)
        with open(audit_path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(entry) + "\n")
    except Exception:
        logger.warning("audit_log_entry failed for tool=%s ok=%s", tool, ok, exc_info=True)
