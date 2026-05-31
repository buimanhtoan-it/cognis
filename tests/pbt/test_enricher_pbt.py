"""Property-Based Test for the enricher — CP-7.

**Validates: Requirements 4.1** (REQ-IDX-4 attribute extraction + secret
redaction; cross-cuts NFR Security).

CP-7 (Secret post-redaction safety):
    For any input string containing a known-shape secret, the post-enricher
    persisted body MUST NOT contain any substring matching the original secret
    pattern.

    ∀ s with secret_pattern matches, redact(s) ∩ secret_value = ∅

Run with::

    pytest tests/pbt/test_enricher_pbt.py -m pbt
"""

from __future__ import annotations

import re

import pytest
from cognis_indexer.enricher.secrets import SecretDetector
from hypothesis import assume, given, settings
from hypothesis import strategies as st

pytestmark = [pytest.mark.pbt]

# ---------------------------------------------------------------------------
# Secret shape generators
# Each strategy generates a string that WILL match the corresponding
# secret pattern, so post-redaction we can assert the pattern is gone.
# ---------------------------------------------------------------------------

# AWS access key: AKIA + exactly 16 uppercase letters/digits
_AWS_KEY_ST = st.builds(
    lambda suffix: "AKIA" + suffix,
    suffix=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        min_size=16,
        max_size=16,
    ),
)

# GitHub PAT: ghp_ + exactly 36 alphanumeric chars
_GITHUB_PAT_ST = st.builds(
    lambda suffix: "ghp_" + suffix,
    suffix=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        min_size=36,
        max_size=36,
    ),
)

# OpenAI key: sk- + 20+ alphanumeric chars
_OPENAI_KEY_ST = st.builds(
    lambda suffix: "sk-" + suffix,
    suffix=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        min_size=20,
        max_size=40,
    ),
)

# Slack token: xoxb- + 10+ alphanumeric-dash chars
_SLACK_TOKEN_ST = st.builds(
    lambda suffix: "xoxb-" + suffix,
    suffix=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-",
        min_size=10,
        max_size=30,
    ),
)

# Google API key: AIza + exactly 35 chars (alphanumeric, dash, underscore)
_GOOGLE_KEY_ST = st.builds(
    lambda suffix: "AIza" + suffix,
    suffix=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
        min_size=35,
        max_size=35,
    ),
)

# JWT: three base64url segments; we ensure the header starts with eyJ
_JWT_ST = st.builds(
    lambda h, p, s: f"eyJ{h}.eyJ{p}.{s}",
    h=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_=-",
        min_size=5,
        max_size=40,
    ),
    p=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_=-",
        min_size=5,
        max_size=40,
    ),
    s=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.+/=-",
        min_size=5,
        max_size=40,
    ),
)

# DSN with credentials: scheme://user:pass@host/db
_DSN_ST = st.builds(
    lambda user, pwd, host: f"postgresql://{user}:{pwd}@{host}/mydb",
    user=st.text(
        alphabet="abcdefghijklmnopqrstuvwxyz0123456789",
        min_size=3,
        max_size=16,
    ),
    pwd=st.text(
        alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        min_size=6,
        max_size=20,
    ),
    host=st.text(
        alphabet="abcdefghijklmnopqrstuvwxyz0123456789-.",
        min_size=4,
        max_size=20,
    ),
)

# Surrounding context: arbitrary printable text before/after the secret
_CONTEXT_ST = st.text(
    alphabet=st.characters(
        whitelist_categories=("Lu", "Ll", "Nd", "Pc", "Pd"),
        whitelist_characters=" \n\t=:,'\"()[]{}",
    ),
    min_size=0,
    max_size=80,
)


def _build_text(prefix: str, secret: str, suffix: str) -> str:
    """Concatenate prefix + secret + suffix."""
    return prefix + secret + suffix


# Compiled patterns that MUST NOT appear in redacted output, keyed by label
_MUST_NOT_MATCH: dict[str, re.Pattern[str]] = {
    "aws-access-key": re.compile(r"AKIA[0-9A-Z]{16}"),
    "github-pat": re.compile(r"ghp_[A-Za-z0-9]{36}"),
    "openai-key": re.compile(r"sk-(?:proj-)?[A-Za-z0-9]{20,}"),
    "slack-token": re.compile(r"xox[baprs]-[A-Za-z0-9-]{10,}"),
    "google-api-key": re.compile(r"AIza[0-9A-Za-z\-_]{35}"),
    "jwt": re.compile(r"eyJ[A-Za-z0-9_=-]+\.[A-Za-z0-9_=-]+\.?[A-Za-z0-9_.+/=-]*"),
    "dsn-with-credentials": re.compile(
        r"[a-zA-Z][a-zA-Z0-9+.\-]*://[^/\s:@]+:[^/\s:@]+@[^\s'\"#]+"
    ),
}


# ---------------------------------------------------------------------------
# CP-7 Properties
# ---------------------------------------------------------------------------


@given(
    secret=_AWS_KEY_ST,
    prefix=_CONTEXT_ST,
    suffix=_CONTEXT_ST,
)
@settings(max_examples=100)
def test_cp7_aws_key_not_in_redacted_output(secret: str, prefix: str, suffix: str) -> None:
    """**Validates: Requirements 4.1** CP-7: AWS access key absent after redaction."""
    text = _build_text(prefix, secret, suffix)
    sd = SecretDetector()
    redacted, types = sd.redact(text)
    pattern = _MUST_NOT_MATCH["aws-access-key"]
    assert not pattern.search(redacted), (
        f"AWS key still present after redaction.\nOriginal: {text!r}\nRedacted: {redacted!r}"
    )
    assert "aws-access-key" in types


@given(
    secret=_GITHUB_PAT_ST,
    prefix=_CONTEXT_ST,
    suffix=_CONTEXT_ST,
)
@settings(max_examples=100)
def test_cp7_github_pat_not_in_redacted_output(secret: str, prefix: str, suffix: str) -> None:
    """**Validates: Requirements 4.1** CP-7: GitHub PAT absent after redaction."""
    text = _build_text(prefix, secret, suffix)
    sd = SecretDetector()
    redacted, types = sd.redact(text)
    pattern = _MUST_NOT_MATCH["github-pat"]
    assert not pattern.search(redacted), (
        f"GitHub PAT still present after redaction.\nOriginal: {text!r}\nRedacted: {redacted!r}"
    )
    assert "github-pat" in types


@given(
    secret=_OPENAI_KEY_ST,
    prefix=_CONTEXT_ST,
    suffix=_CONTEXT_ST,
)
@settings(max_examples=100)
def test_cp7_openai_key_not_in_redacted_output(secret: str, prefix: str, suffix: str) -> None:
    """**Validates: Requirements 4.1** CP-7: OpenAI key absent after redaction."""
    text = _build_text(prefix, secret, suffix)
    sd = SecretDetector()
    redacted, types = sd.redact(text)
    pattern = _MUST_NOT_MATCH["openai-key"]
    assert not pattern.search(redacted), (
        f"OpenAI key still present after redaction.\nOriginal: {text!r}\nRedacted: {redacted!r}"
    )
    assert "openai-key" in types


@given(
    secret=_SLACK_TOKEN_ST,
    prefix=_CONTEXT_ST,
    suffix=_CONTEXT_ST,
)
@settings(max_examples=100)
def test_cp7_slack_token_not_in_redacted_output(secret: str, prefix: str, suffix: str) -> None:
    """**Validates: Requirements 4.1** CP-7: Slack token absent after redaction."""
    text = _build_text(prefix, secret, suffix)
    sd = SecretDetector()
    redacted, types = sd.redact(text)
    pattern = _MUST_NOT_MATCH["slack-token"]
    assert not pattern.search(redacted), (
        f"Slack token still present after redaction.\nOriginal: {text!r}\nRedacted: {redacted!r}"
    )
    assert "slack-token" in types


@given(
    secret=_GOOGLE_KEY_ST,
    prefix=_CONTEXT_ST,
    suffix=_CONTEXT_ST,
)
@settings(max_examples=100)
def test_cp7_google_key_not_in_redacted_output(secret: str, prefix: str, suffix: str) -> None:
    """**Validates: Requirements 4.1** CP-7: Google API key absent after redaction."""
    text = _build_text(prefix, secret, suffix)
    sd = SecretDetector()
    redacted, types = sd.redact(text)
    pattern = _MUST_NOT_MATCH["google-api-key"]
    assert not pattern.search(redacted), (
        f"Google API key still present after redaction.\nOriginal: {text!r}\nRedacted: {redacted!r}"
    )
    assert "google-api-key" in types


@given(
    header=st.sampled_from(["RSA PRIVATE KEY", "OPENSSH PRIVATE KEY", "EC PRIVATE KEY"]),
    prefix=_CONTEXT_ST,
    suffix=_CONTEXT_ST,
)
@settings(max_examples=50)
def test_cp7_pem_header_not_in_redacted_output(header: str, prefix: str, suffix: str) -> None:
    """**Validates: Requirements 4.1** CP-7: PEM private key header absent after redaction."""
    secret = f"-----BEGIN {header}-----"
    text = _build_text(prefix, secret, suffix)
    sd = SecretDetector()
    redacted, types = sd.redact(text)
    pem_pattern = re.compile(r"-----BEGIN [A-Z ]+PRIVATE KEY-----")
    assert not pem_pattern.search(redacted), (
        f"PEM header still present.\nOriginal: {text!r}\nRedacted: {redacted!r}"
    )
    assert "pem-private-key-header" in types


@given(
    dsn_data=_DSN_ST,
    prefix=_CONTEXT_ST,
    suffix=_CONTEXT_ST,
)
@settings(max_examples=100)
def test_cp7_dsn_credentials_not_in_redacted_output(
    dsn_data: str, prefix: str, suffix: str
) -> None:
    """**Validates: Requirements 4.1** CP-7: DSN credentials absent after redaction."""
    text = _build_text(prefix, dsn_data, suffix)
    # Only proceed if text actually contains a DSN pattern
    dsn_pattern = _MUST_NOT_MATCH["dsn-with-credentials"]
    assume(dsn_pattern.search(dsn_data) is not None)

    sd = SecretDetector()
    redacted, types = sd.redact(text)
    assert not dsn_pattern.search(redacted), (
        f"DSN still present.\nOriginal: {text!r}\nRedacted: {redacted!r}"
    )
    assert "dsn-with-credentials" in types


@given(
    secret=_JWT_ST,
    prefix=_CONTEXT_ST,
    suffix=_CONTEXT_ST,
)
@settings(max_examples=100)
def test_cp7_jwt_not_in_redacted_output(secret: str, prefix: str, suffix: str) -> None:
    """**Validates: Requirements 4.1** CP-7: JWT absent after redaction."""
    text = _build_text(prefix, secret, suffix)
    sd = SecretDetector()
    redacted, types = sd.redact(text)
    # Verify the original JWT value is not present verbatim after redaction
    assert secret not in redacted, (
        f"JWT still present verbatim.\nOriginal: {text!r}\nRedacted: {redacted!r}"
    )
    assert "jwt" in types
