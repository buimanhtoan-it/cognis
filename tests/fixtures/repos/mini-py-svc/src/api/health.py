"""`/health` endpoints — liveness, readiness, version."""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Request

from app.startup import liveness_probe, readiness_probe

router = APIRouter()


@router.get("/health")
async def health(request: Request) -> dict[str, Any]:
    """Combined liveness + readiness summary."""
    live = liveness_probe()
    ready = await readiness_probe(request.app)
    overall = "ok" if live.get("status") == "alive" and ready.get("status") == "ready" else "degraded"
    return {"status": overall, "liveness": live, "readiness": ready}


@router.get("/health/live")
async def live() -> dict[str, str]:
    """Liveness probe — always returns ``alive`` while the process runs."""
    return liveness_probe()


@router.get("/health/ready")
async def ready(request: Request) -> dict[str, Any]:
    """Readiness probe — confirms downstreams reachable."""
    return await readiness_probe(request.app)


@router.get("/health/version")
async def version(request: Request) -> dict[str, str]:
    """Service identity."""
    settings = request.app.state.settings
    return {"service": settings.service_name, "version": settings.version}
