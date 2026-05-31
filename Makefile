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

# Strict-typing scope per Task 1 — start with packages/core, expand later.
MYPY_STRICT_PATHS ?= packages/core
PYTEST_ARGS ?=

.DEFAULT_GOAL := help

.PHONY: help
help:
	@echo "cognis dev recipes:"
	@echo "  make lint        # ruff format --check + ruff check"
	@echo "  make format      # ruff format (writes changes)"
	@echo "  make typecheck   # mypy (strict on $(MYPY_STRICT_PATHS))"
	@echo "  make test        # pytest unit + property tests"
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
	$(PYTEST) -m "not benchmark and not eval" $(PYTEST_ARGS)

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
