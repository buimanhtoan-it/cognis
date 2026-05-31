"""Enricher sub-package for the cognis indexer pipeline.

Exposes the three public surfaces consumed by the pipeline:

- :class:`~cognis_indexer.enricher.attributes.AttributeExtractor` — extract
  ``db_table``, ``http_route``, ``env_var``, ``external_call`` from symbol body
  text using regex patterns.
- :class:`~cognis_indexer.enricher.secrets.SecretDetector` — Shannon-entropy
  threshold + known regex set; ``redact(text)`` returns cleaned text and a list
  of redacted-type labels.
- :class:`~cognis_indexer.enricher.enricher.Enricher` — orchestrates the above
  over a :class:`~cognis_indexer.parsers.base.ParsedSymbol` and returns an
  :class:`~cognis_indexer.enricher.enricher.EnrichedSymbol`.
- :class:`~cognis_indexer.enricher.enricher.EnrichedSymbol` — dataclass wrapping
  the (redacted) symbol, extracted attributes, and untrusted flags.
"""

from cognis_indexer.enricher.attributes import AttributeExtractor
from cognis_indexer.enricher.enricher import EnrichedSymbol, Enricher
from cognis_indexer.enricher.secrets import SecretDetector

__all__ = [
    "AttributeExtractor",
    "EnrichedSymbol",
    "Enricher",
    "SecretDetector",
]
