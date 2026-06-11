# cognis developer Makefile
#
# Mirrors `tasks.py` (Invoke). Both expose the same recipes so contributors can
# pick whichever fits their shell. The CI pipeline uses these targets verbatim.

PYTHON      ?= python
PIP         ?= $(PYTHON) -m pip
PYTEST      ?= $(PYTHON) -m pytest
RUFF        ?= $(PYTHON) -m ruff
MYPY        ?= $(PYTHON) -m mypy
COGNIS_CLI  ?= $(PYTHON) -m cognis.cli.main

# Strict-typing scope per Task 1 — packages/core first, expanding each cycle.
MYPY_STRICT_PATHS ?= packages/core packages/indexer
PYTEST_ARGS ?=

.DEFAULT_GOAL := help

.PHONY: help
help:
	@echo "cognis dev recipes:"
	@echo "  make lint        # ruff format --check + ruff check"
	@echo "  make format      # ruff format (writes changes)"
	@echo "  make typecheck   # mypy (strict on $(MYPY_STRICT_PATHS))"
	@echo "  make test        # pytest unit + property tests"
	@echo "  make e2e         # full cross-app end-to-end flow (CLI+indexd+mcpd)"
	@echo "  make coverage    # full-flow coverage (unit+integration+pbt+e2e) + gap report"
	@echo "  make coverage-html # same, plus htmlcov/ report"
	@echo "  make bench       # pytest --benchmark-only"
	@echo "  make eval        # cognis-cli eval (golden-set runner)"
	@echo "  make clean       # remove build / cache artifacts"
	@echo "  make install-dev # editable install + dev deps + extension compile"
	@echo "  make install-extension # npm install + compile apps/cognis-vscode"

.PHONY: install-extension
install-extension:
	$(PYTHON) scripts/setup_extension.py

.PHONY: install-dev
install-dev:
	$(PIP) install -e ".[indexer,embed-local,vector,tokenizers,mcp]"
	$(PIP) install --group dev || $(PIP) install ruff mypy pytest pytest-asyncio pytest-benchmark hypothesis pre-commit invoke
	pre-commit install || true
	-$(PYTHON) scripts/prepare-branding.py
	$(PYTHON) scripts/setup_extension.py

.PHONY: lint
lint:
	$(RUFF) format --check .
	$(RUFF) check .

.PHONY: format
format:
	$(RUFF) format .
	$(RUFF) check --fix .

.PHONY: typecheck
typecheck:
	$(MYPY) $(MYPY_STRICT_PATHS)

.PHONY: test
test:
	$(PYTEST) -m "not benchmark and not eval and not e2e" $(PYTEST_ARGS)

.PHONY: e2e
e2e:
	$(PYTEST) -m e2e $(PYTEST_ARGS)

.PHONY: coverage
coverage:
	$(PYTHON) scripts/coverage_full.py $(COVERAGE_ARGS)

.PHONY: coverage-html
coverage-html:
	$(PYTHON) scripts/coverage_full.py --html $(COVERAGE_ARGS)

.PHONY: e2e-report
e2e-report:
	$(PYTHON) tests/e2e/report.py $(E2E_REPORT_ARGS)

.PHONY: e2e-baseline-update
e2e-baseline-update:
	$(PYTHON) tests/e2e/report.py --out eval-reports/e2e
	$(PYTHON) scripts/compare_baseline.py --current eval-reports/e2e/e2e-report.json --update

.PHONY: compare-baseline
compare-baseline:
	$(PYTHON) tests/e2e/report.py --out eval-reports/e2e
	$(PYTHON) scripts/compare_baseline.py --current eval-reports/e2e/e2e-report.json $(COMPARE_ARGS)

.PHONY: bench
bench:
	$(PYTEST) -m benchmark --benchmark-only $(PYTEST_ARGS)

.PHONY: eval
eval:
	$(COGNIS_CLI) eval $(EVAL_ARGS)

.PHONY: clean
clean:
	$(PYTHON) -c "import shutil, os; [shutil.rmtree(p, ignore_errors=True) for p in ['build','dist','.pytest_cache','.mypy_cache','.ruff_cache','.hypothesis','.benchmarks','htmlcov']]"
	$(PYTHON) -c "import pathlib; [p.unlink() for p in pathlib.Path('.').rglob('*.py[co]')]"
	$(PYTHON) -c "import pathlib, shutil; [shutil.rmtree(p, ignore_errors=True) for p in pathlib.Path('.').rglob('__pycache__')]"
