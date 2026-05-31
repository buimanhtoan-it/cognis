"""Secret detector for the cognis enricher pipeline.

Implements two complementary detection strategies:

1. **Known-shape regex patterns** — AWS access keys, GitHub PATs, Slack tokens,
   Google API keys, OpenAI keys, JWTs, PEM private-key headers, DSNs with
   embedded credentials, and password/secret assignment patterns.

2. **Shannon-entropy threshold** — string literals that exceed 4.5 bits/char
   AND are ≥ 16 characters long are flagged as high-entropy secrets.

The public interface is :class:`SecretDetector` with a single
:meth:`~SecretDetector.redact` method:

    >>> sd = SecretDetector()
    >>> redacted, types = sd.redact("key = 'AKIAIOSFODNN7EXAMPLE'")
    >>> assert "[REDACTED:aws-access-key]" in redacted
    >>> assert "aws-access-key" in types

Secrets are replaced in-place with ``[REDACTED:<type>]``.  The original string
is **never** persisted.

Design reference: design.md *Indexer Pipeline → Enricher → Secret detection*.
"""

from __future__ import annotations

import re
from collections import Counter
from math import log2

# ---------------------------------------------------------------------------
# Known-shape patterns
# ---------------------------------------------------------------------------
# Each entry: (compiled_pattern, label_used_in_[REDACTED:label])
# Patterns are ordered from most specific to least specific to avoid
# "almost matches" swallowing a more precise pattern.

_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    # AWS access key id (AKIA prefix + 16 uppercase alphanumeric chars)
    (re.compile(r"AKIA[0-9A-Z]{16}"), "aws-access-key"),
    # GitHub personal access token (ghp_ prefix)
    (re.compile(r"ghp_[A-Za-z0-9]{36}"), "github-pat"),
    # Slack tokens (xoxb/xoxa/xoxp/xoxr/xoxs)
    (re.compile(r"xox[baprs]-[A-Za-z0-9-]{10,}"), "slack-token"),
    # Google API key (AIza prefix)
    (re.compile(r"AIza[0-9A-Za-z\-_]{35}"), "google-api-key"),
    # OpenAI secret key / project key (sk- or sk-proj- prefix)
    (re.compile(r"sk-(?:proj-)?[A-Za-z0-9]{20,}"), "openai-key"),
    # JWT — three base64url segments separated by dots
    (
        re.compile(
            r"eyJ[A-Za-z0-9_=-]+"
            r"\.[A-Za-z0-9_=-]+"
            r"\.?[A-Za-z0-9_.+/=-]*"
        ),
        "jwt",
    ),
    # PEM private key header (BEGIN ... PRIVATE KEY)
    (
        re.compile(r"-----BEGIN [A-Z ]+PRIVATE KEY-----"),
        "pem-private-key-header",
    ),
    # DSN / URL with embedded credentials: scheme://[user]:pass@host
    # Supports empty username (redis://:pass@host) and non-empty (pg://user:pass@host)
    (
        re.compile(
            r"[a-zA-Z][a-zA-Z0-9+.\-]*://"  # scheme
            r"[^/\s:@]*"  # username (may be empty)
            r":"  # colon
            r"[^/\s:@]+"  # password (non-empty, no slashes/spaces/@)
            r"@"  # at-sign
            r"[^\s'\"#]+"  # host + path (no quotes/hash)
        ),
        "dsn-with-credentials",
    ),
]

# Password / secret assignment: password = "value", passwd = 'value', etc.
# We capture group 1 = keyword, group 2 = value so we can reconstruct the
# assignment with the value replaced.
_PASSWORD_RE = re.compile(
    r"""(?ix)
    \b
    (password|passwd|pwd|secret|api[_\-]?key|token)  # keyword
    \s*[:=]\s*                                         # separator
    ['"]                                               # opening quote
    ([^'"\n]{4,})                                      # value (4+ chars)
    ['"]                                               # closing quote
    """,
)

# ---------------------------------------------------------------------------
# Entropy helpers
# ---------------------------------------------------------------------------


def _shannon_entropy(value: str) -> float:
    """Return Shannon entropy in bits per character for *value*.

    Uses the standard formula ``-sum(p * log2(p))`` where *p* is the
    probability of each unique character.

    Returns ``0.0`` for empty strings.
    """
    if not value:
        return 0.0
    counts = Counter(value)
    total = float(len(value))
    return -sum((n / total) * log2(n / total) for n in counts.values())


def _is_high_entropy(value: str, threshold: float = 4.5) -> bool:
    """Return True when *value* looks like a secret based on entropy alone.

    Both conditions must hold:
    - ``len(value) >= 16`` (short strings are exempt).
    - Shannon entropy >= *threshold* (default 4.5 bits/char).
    """
    return len(value) >= 16 and _shannon_entropy(value) >= threshold


# Pattern to find quoted string literals (used for entropy scan).
_QUOTED_STRING_RE = re.compile(
    r"""(?x)
    (?:
        '([^'\n]{16,})'       # single-quoted, 16+ chars
    |
        "([^"\n]{16,})"       # double-quoted, 16+ chars
    )
    """,
)

# ---------------------------------------------------------------------------
# SecretDetector
# ---------------------------------------------------------------------------


class SecretDetector:
    """Detect and redact secret-shaped strings.

    This class is stateless; a single instance is safe to share across threads.

    Example::

        sd = SecretDetector()
        clean, types = sd.redact(text)
        if types:
            # at least one secret was found and redacted
            ...
    """

    def redact(self, text: str) -> tuple[str, list[str]]:
        """Replace all secret-shaped substrings in *text* with ``[REDACTED:<type>]``.

        Applies pattern matching first, then entropy-based detection on any
        remaining quoted string literals.

        Args:
            text: Input string (body_excerpt, signature, docstring, etc.).

        Returns:
            A 2-tuple ``(redacted_text, types_found)`` where:

            - *redacted_text* is *text* with every secret replaced by its
              ``[REDACTED:<type>]`` placeholder.  If no secrets are found,
              *redacted_text* is the original *text* unchanged.
            - *types_found* is a list of unique redaction-type labels (e.g.
              ``["aws-access-key", "jwt"]``).  Order reflects first occurrence.
              Empty list when no secrets found.
        """
        if not text:
            return text, []

        out = text
        found_types: list[str] = []
        seen_types: set[str] = set()

        def _record(label: str) -> None:
            if label not in seen_types:
                seen_types.add(label)
                found_types.append(label)

        # 1. Apply known-shape regex patterns
        for pattern, label in _PATTERNS:
            new_out, count = pattern.subn(f"[REDACTED:{label}]", out)
            if count > 0:
                _record(label)
                out = new_out

        # 2. Apply password/secret assignment pattern
        def _replace_password(m: re.Match[str]) -> str:
            _record("password-assignment")
            return f'{m.group(1)}="[REDACTED:password-assignment]"'

        out = _PASSWORD_RE.sub(_replace_password, out)

        # 3. Entropy-based scan on any quoted strings that survived step 1+2
        def _replace_high_entropy(m: re.Match[str]) -> str:
            # group(1) = single-quoted value, group(2) = double-quoted value
            value = m.group(1) or m.group(2) or ""
            if _is_high_entropy(value):
                _record("high-entropy")
                quote = "'" if m.group(1) is not None else '"'
                return f"{quote}[REDACTED:high-entropy]{quote}"
            return m.group(0)

        out = _QUOTED_STRING_RE.sub(_replace_high_entropy, out)

        return out, found_types

    # ------------------------------------------------------------------
    # Convenience helpers
    # ------------------------------------------------------------------

    def is_secret(self, value: str) -> bool:
        """Return True when *value* matches any known secret pattern.

        This is a fast check (no redaction); useful for validation tests.
        """
        if not value:
            return False
        return any(p.search(value) for p, _ in _PATTERNS) or bool(_PASSWORD_RE.search(value))

    def shannon_entropy(self, value: str) -> float:
        """Expose :func:`_shannon_entropy` as a public method for testing."""
        return _shannon_entropy(value)

    def is_high_entropy(self, value: str, threshold: float = 4.5) -> bool:
        """Expose :func:`_is_high_entropy` as a public method for testing."""
        return _is_high_entropy(value, threshold)
