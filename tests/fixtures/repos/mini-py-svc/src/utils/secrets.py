"""Env-var loader + planted secret-shaped strings.

PLANTED ISSUE: this module deliberately contains string literals whose
*lexical shape* matches real secrets. They are ALL fake — every one is
annotated with `# fake — fixture only` — but the cognis secret detector
should redact them anyway because shape is what matters for CP-7.
Patterns: AWS keys, OpenAI keys, JWTs, ``password = "..."``, PEM headers,
Slack/GitHub/Google tokens, DSN URIs.
"""

from __future__ import annotations

import os
import re
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any


# ---- Planted secret-shaped LITERALS (fake — fixture only) ----------------

# fake — fixture only. ``AKIA[0-9A-Z]{16}``.
SAMPLE_AWS_ACCESS_KEY: str = "AKIAIOSFODNN7EXAMPLE"
SAMPLE_AWS_ACCESS_KEY_ALT: str = "AKIAI44QH8DHBEXAMPLE"  # fake — fixture only
SAMPLE_AWS_SECRET_KEY: str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"  # fake — fixture only
# fake — fixture only. ``sk-[A-Za-z0-9]{20,}``.
SAMPLE_OPENAI_KEY: str = "sk-FakeFakeFakeFakeFakeFakeFakeFakeFakeFake1234"
SAMPLE_OPENAI_PROJECT_KEY: str = "sk-proj-AAAAbbbbCCCCddddEEEEffffGGGGhhhhIIIIjjjjKKKK"
# fake — fixture only. JWT shape.
SAMPLE_JWT: str = (
    "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"
    ".eyJzdWIiOiJ1MTIzIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjk5OTk5OTk5OTl9"
    ".F4keS1gN4tuREXAMPLE_REPLACE_BEFORE_DEPLOY"
)
# fake — fixture only. password = "..." shape.
DEMO_PASSWORD = "hunter2-fixture-NOT-real"  # password = "hunter2"
ADMIN_PASSWORD = "correct horse battery staple"  # password = "..."
LEGACY_PASSWORD: str = "p@ssw0rd-fixture"  # password = "p@ssw0rd"
# fake — fixture only. PEM block headers; cognis matches on the BEGIN line.
SAMPLE_RSA_PRIVATE_KEY_PEM: str = (
    "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAxF4keFakeBase64Body+NotARealKey==\n"
    "-----END RSA PRIVATE KEY-----\n"
)
SAMPLE_OPENSSH_PRIVATE_KEY_PEM: str = (
    "-----BEGIN OPENSSH PRIVATE KEY-----\nFAKEFAKE==\n-----END OPENSSH PRIVATE KEY-----\n"
)
SAMPLE_EC_PRIVATE_KEY_PEM: str = (
    "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIFakeECKey==\n-----END EC PRIVATE KEY-----\n"
)
# fake — fixture only.
SAMPLE_GITHUB_PAT: str = "ghp_FakeFakeFakeFakeFakeFakeFakeFakeAB12"
SAMPLE_SLACK_TOKEN: str = "xoxb-FAKE-fixture-token-do-not-use-FakeFakeFake"
SAMPLE_GOOGLE_API_KEY: str = "AIzaFakeFakeFakeFakeFakeFakeFakeFakeFake"
# fake — fixture only. URLs with credentials embedded.
SAMPLE_DSN_WITH_PASSWORD: str = "postgresql://admin:hunter2@db.example.com/prod"
SAMPLE_REDIS_URL: str = "redis://:hunter2@cache.example.com:6379/0"
SAMPLE_AMQP_URL: str = "amqps://app:hunter2@broker.example.com:5671/vhost"


# ---- Compiled regex set used by the redactor ----------------------------

_PASSWORD_ASSIGN_RE = re.compile(
    r"""(?ix)\b(password|passwd|pwd|secret|api[_-]?key|token)\s*[:=]\s*['\"]([^'\"\n]{4,})['\"]""",
)
_SECRET_REGEXES: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"AKIA[0-9A-Z]{16}"), "[REDACTED:aws-access-key]"),
    (re.compile(r"ghp_[A-Za-z0-9]{36}"), "[REDACTED:github-pat]"),
    (re.compile(r"xox[baprs]-[A-Za-z0-9-]{10,}"), "[REDACTED:slack-token]"),
    (re.compile(r"AIza[0-9A-Za-z\-_]{35}"), "[REDACTED:google-api-key]"),
    (re.compile(r"sk-(?:proj-)?[A-Za-z0-9]{20,}"), "[REDACTED:openai-key]"),
    (re.compile(r"eyJ[A-Za-z0-9_=-]+\.[A-Za-z0-9_=-]+\.?[A-Za-z0-9_.+/=-]*"), "[REDACTED:jwt]"),
    (re.compile(r"-----BEGIN [A-Z ]+PRIVATE KEY-----"), "[REDACTED:pem-private-key-header]"),
    (re.compile(r"[a-zA-Z][a-zA-Z0-9+.-]*://[^/\s:@]+:[^/\s:@]+@[^\s'\"]+"), "[REDACTED:dsn-with-credentials]"),
]


@dataclass(slots=True)
class RedactionResult:
    """Outcome of a `scrub_secrets()` call."""

    text: str
    replaced: list[str]


def scrub_secrets(text: str) -> str:
    """Replace secret-shaped substrings with ``[REDACTED:<type>]``."""
    return scrub_secrets_detailed(text).text


def scrub_secrets_detailed(text: str) -> RedactionResult:
    """Same as `scrub_secrets` but also reports which kinds matched."""
    if not text:
        return RedactionResult(text=text, replaced=[])
    out, replaced = text, []
    for pattern, replacement in _SECRET_REGEXES:
        new_out, n = pattern.subn(replacement, out)
        if n > 0:
            replaced.append(replacement.split(":", 1)[1].rstrip("]"))
            out = new_out
    out = _PASSWORD_ASSIGN_RE.sub(
        lambda m: f'{m.group(1)}="[REDACTED:password-assignment]"', out
    )
    if _PASSWORD_ASSIGN_RE.search(text):
        replaced.append("password-assignment")
    return RedactionResult(text=out, replaced=replaced)


def looks_like_secret(value: str) -> bool:
    """True when ``value`` matches any known secret shape."""
    if not value:
        return False
    return any(p.search(value) for p, _ in _SECRET_REGEXES) or bool(_PASSWORD_ASSIGN_RE.search(value))


def shannon_entropy_bits(value: str) -> float:
    """Compute Shannon entropy in bits-per-char for ``value``."""
    if not value:
        return 0.0
    from collections import Counter
    from math import log2

    counts = Counter(value)
    total = float(len(value))
    return -sum((n / total) * log2(n / total) for n in counts.values())


def is_high_entropy(value: str, threshold: float = 4.5) -> bool:
    """High-entropy gate; values shorter than 16 chars are exempt."""
    return len(value) >= 16 and shannon_entropy_bits(value) >= threshold


def load_env_secret(name: str, default: str | None = None) -> str:
    """Read ``name`` from the environment."""
    if raw := os.getenv(name):
        return raw
    if default is not None:
        return default
    raise KeyError(f"env var {name} not configured")


def load_many_env_secrets(names: Iterable[str]) -> dict[str, str]:
    """Read a batch of env vars; missing entries are silently dropped."""
    return {name: value for name in names if (value := os.getenv(name))}


def known_secret_kinds() -> list[str]:
    """Redaction kinds emitted by `scrub_secrets`."""
    return [r.split(":", 1)[1].rstrip("]") for _, r in _SECRET_REGEXES] + ["password-assignment"]


_SAMPLE_NAMES = (
    "SAMPLE_AWS_ACCESS_KEY", "SAMPLE_AWS_ACCESS_KEY_ALT", "SAMPLE_AWS_SECRET_KEY",
    "SAMPLE_OPENAI_KEY", "SAMPLE_OPENAI_PROJECT_KEY", "SAMPLE_JWT",
    "DEMO_PASSWORD", "ADMIN_PASSWORD", "LEGACY_PASSWORD",
    "SAMPLE_RSA_PRIVATE_KEY_PEM", "SAMPLE_OPENSSH_PRIVATE_KEY_PEM",
    "SAMPLE_EC_PRIVATE_KEY_PEM", "SAMPLE_GITHUB_PAT", "SAMPLE_SLACK_TOKEN",
    "SAMPLE_GOOGLE_API_KEY", "SAMPLE_DSN_WITH_PASSWORD",
    "SAMPLE_REDIS_URL", "SAMPLE_AMQP_URL",
)


def planted_samples() -> dict[str, Any]:
    """Return every planted sample by name."""
    return {name: globals()[name] for name in _SAMPLE_NAMES}
