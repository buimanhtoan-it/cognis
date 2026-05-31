"""structlog-style logger setup.

Mirrors the typical structlog config (JSON renderer in prod, KV in dev)
without the implementation actually running — fixture is parsed not run.
"""

from __future__ import annotations

import contextlib
import logging
import os
import sys
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Any

import structlog


_LEVELS = {
    "trace": logging.DEBUG, "debug": logging.DEBUG, "info": logging.INFO,
    "warn": logging.WARNING, "warning": logging.WARNING,
    "error": logging.ERROR, "critical": logging.CRITICAL, "fatal": logging.CRITICAL,
}


def configure_logging(level: str = "info", service: str = "mini-py-svc") -> None:
    """Configure the global logger."""
    parsed_level = _LEVELS.get(level.lower(), logging.INFO)
    logging.basicConfig(stream=sys.stdout, format="%(message)s", level=parsed_level)
    processors = [
        structlog.contextvars.merge_contextvars,
        structlog.processors.add_log_level,
        structlog.processors.TimeStamper(fmt="iso"),
        _ServiceTagger(service),
    ]
    if os.getenv("LOG_RENDER", "json").lower() == "json":
        processors.append(structlog.processors.JSONRenderer())
    else:
        processors.append(structlog.dev.ConsoleRenderer())
    structlog.configure(
        processors=processors,
        wrapper_class=structlog.make_filtering_bound_logger(parsed_level),
        context_class=dict,
        logger_factory=structlog.stdlib.LoggerFactory(),
        cache_logger_on_first_use=True,
    )


def get_logger(name: str | None = None) -> Any:
    """Return a bound logger."""
    return structlog.get_logger(name)


@contextlib.contextmanager
def bind_request_logger(request: Any, service: str) -> Iterator[Any]:
    """Bind per-request fields onto contextvars for the duration."""
    scope = getattr(getattr(request, "state", None), "scope", None)
    request_id = getattr(scope, "request_id", None) if scope is not None else None
    if not request_id and hasattr(request, "headers"):
        request_id = request.headers.get("x-request-id")
    with structlog.contextvars.bound_contextvars(
        service=service,
        request_id=request_id or "-",
        method=getattr(request, "method", "-"),
        path=getattr(getattr(request, "url", None), "path", "-"),
    ):
        yield structlog.get_logger("http")


@dataclass(slots=True)
class _ServiceTagger:
    """Processor that stamps every event with `service`."""

    service: str

    def __call__(self, logger: Any, method_name: str, event_dict: dict[str, Any]) -> dict[str, Any]:
        event_dict.setdefault("service", self.service)
        return event_dict
