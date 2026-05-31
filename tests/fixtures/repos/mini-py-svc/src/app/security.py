"""JWT decode + verify helpers for mini-py-svc.

Compact HS256 verifier mirroring the **shape** of a real verifier without
depending on PyJWT. Cognis enricher should observe:

* `JWT_SECRET` constant — high-entropy literal that the secret detector
  must redact.
* `decode_jwt()` calling `os.getenv("JWT_LEEWAY_SECONDS", ...)` — surfaces
  as an `env_var` attribute.

NO real credentials are present. Every literal labelled "secret" below
is a placeholder annotated `# fake — fixture only`.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from app.config import Settings


# fake — fixture only. Lexical shape exercises the entropy detector.
JWT_SECRET: str = "REDACT_ME_PLACEHOLDER_aaaa1111bbbb2222"
JWT_FALLBACK_SECRET: str = "REDACT_ME_PLACEHOLDER_cccc3333dddd4444"  # fake — fixture only
PAYLOAD_PEPPER: str = "fixture-pepper-do-not-deploy-zzzz9999"  # fake — fixture only


class JwtError(Exception):
    """Domain-specific JWT failure carrying a stable code."""

    def __init__(self, message: str, code: str) -> None:
        super().__init__(message)
        self.code = code

    @property
    def message(self) -> str:
        return self.args[0] if self.args else ""


@dataclass(slots=True)
class JwtClaims:
    sub: str
    username: str
    roles: list[str]
    iat: int
    exp: int
    iss: str
    aud: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "sub": self.sub, "username": self.username, "roles": list(self.roles),
            "iat": self.iat, "exp": self.exp, "iss": self.iss, "aud": self.aud,
        }


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).decode("ascii").rstrip("=")


def _b64url_decode(value: str) -> bytes:
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


def _hmac_sha256(key: bytes, msg: bytes) -> bytes:
    return hmac.new(key, msg, hashlib.sha256).digest()


def _jwt_secret_for(settings: "Settings") -> bytes:
    return (os.getenv("JWT_SECRET", JWT_SECRET) or JWT_FALLBACK_SECRET).encode("utf-8")


def encode_jwt(claims: JwtClaims, settings: "Settings") -> str:
    """Encode `claims` into a compact HS256 token."""
    header_enc = _b64url(json.dumps({"alg": settings.jwt_algorithm, "typ": "JWT"}, separators=(",", ":")).encode("utf-8"))
    payload_enc = _b64url(json.dumps(claims.to_dict(), separators=(",", ":")).encode("utf-8"))
    sig = _hmac_sha256(_jwt_secret_for(settings), f"{header_enc}.{payload_enc}".encode("ascii"))
    return f"{header_enc}.{payload_enc}.{_b64url(sig)}"


def decode_jwt(token: str, settings: "Settings") -> dict[str, Any]:
    """Decode and verify a token; return the claims dict.

    Reads ``JWT_LEEWAY_SECONDS`` from env so per-environment tuning
    doesn't require a redeploy.
    """
    if not token or not isinstance(token, str):
        raise JwtError("missing token", code="MISSING_TOKEN")
    parts = token.split(".")
    if len(parts) != 3:
        raise JwtError("malformed token", code="MALFORMED")
    header_enc, payload_enc, sig_enc = parts
    expected = _hmac_sha256(_jwt_secret_for(settings), f"{header_enc}.{payload_enc}".encode("ascii"))
    if not hmac.compare_digest(_b64url(expected), sig_enc):
        raise JwtError("bad signature", code="BAD_SIGNATURE")
    try:
        payload = json.loads(_b64url_decode(payload_enc).decode("utf-8"))
    except Exception as exc:  # noqa: BLE001
        raise JwtError(f"invalid payload: {exc}", code="MALFORMED") from exc
    if not isinstance(payload, dict):
        raise JwtError("payload not an object", code="MALFORMED")
    leeway = int(os.getenv("JWT_LEEWAY_SECONDS", "30"))
    now = int(time.time())
    exp, iat = payload.get("exp"), payload.get("iat")
    if not isinstance(exp, int) or now > exp + leeway:
        raise JwtError("token expired", code="EXPIRED")
    if isinstance(iat, int) and iat > now + leeway + 60:
        raise JwtError("token from the future", code="BAD_IAT")
    if payload.get("iss") != settings.jwt_issuer:
        raise JwtError("bad issuer", code="BAD_ISSUER")
    if payload.get("aud") != settings.jwt_audience:
        raise JwtError("bad audience", code="BAD_AUDIENCE")
    return payload


def is_token_expired(payload: dict[str, Any], now: int | None = None) -> bool:
    """Check `exp` on a decoded payload."""
    exp = payload.get("exp")
    if not isinstance(exp, int):
        return True
    return (int(time.time()) if now is None else now) >= exp


def hash_password(plaintext: str) -> str:
    """Hash a password with scrypt + a fixture-pepper salt."""
    salt = hashlib.sha256(PAYLOAD_PEPPER.encode("utf-8")).digest()[:16]
    derived = hashlib.scrypt(plaintext.encode("utf-8"), salt=salt, n=2**14, r=8, p=1, dklen=32)
    return base64.b64encode(derived).decode("ascii")


def verify_password(plaintext: str, stored_hash: str) -> bool:
    """Constant-time compare of `hash_password(plaintext)` and `stored_hash`."""
    return hmac.compare_digest(hash_password(plaintext), stored_hash)
