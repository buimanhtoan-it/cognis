# Contributing to cognis

Thank you for contributing to `cognis`.

## Development setup

Clone the repository and create a virtual environment:

```bash
git clone https://github.com/buimanhtoan-it/cognis
cd cognis
python -m venv .venv
```

Activate the virtual environment:

- Windows PowerShell: `.\.venv\Scripts\Activate.ps1`
- macOS / Linux: `source .venv/bin/activate`

Install the development environment:

```bash
make install-dev
```

On Windows, you can also use the one-step helper:

```powershell
.\scripts\setup-dev.ps1
```

The development install includes:

- the editable Python package
- development dependencies
- pre-commit hooks
- a compiled VS Code / Cursor extension build

## Day-to-day workflow

Typical local workflow:

```bash
make test
make lint
make typecheck
```

If you prefer Invoke on Windows, use the matching tasks in `tasks.py`.

## Before opening a pull request

Run the full local checks:

```bash
make lint
make typecheck
make test
python -m pytest -m integration --maxfail=5
python -m cognis.cli.main mcp-conformance
```

## Pull request expectations

- Keep each pull request focused on a single problem or feature.
- Update documentation when changing installation, configuration, or operational behavior.
- Add or update tests when the change affects runtime behavior.
- Do not commit secrets, `.cognis/` runtime state, generated reports, or local scratch files.

## Release process

Maintainers should follow [docs/release.md](docs/release.md). Releases are
tag-driven through `.github/workflows/release.yml`.

## Code of conduct

Be respectful, specific, and constructive in issues, reviews, and commits.
