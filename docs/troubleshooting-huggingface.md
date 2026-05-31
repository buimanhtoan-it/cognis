# Hugging Face and Embedder Troubleshooting

Use this guide when local embedding downloads fail or semantic indexing is not
working as expected.

## Common symptom: `401 Unauthorized`

You may see an error similar to:

```text
Repository Not Found for url: .../sentence-transformers/bge-small-en-v1.5/...
Invalid username or password.
```

## Check the model id

The local embedder configuration should use:

```text
BAAI/bge-small-en-v1.5
```

If your local `.cognis/config.yaml` contains only `bge-small-en-v1.5`, update it:

```yaml
embedder:
  backend: local
  model: BAAI/bge-small-en-v1.5
```

If needed, regenerate the default config:

```powershell
python -m cognis.cli.main init --force
```

## Check for stale Hugging Face credentials

If `HF_TOKEN` or `HUGGING_FACE_HUB_TOKEN` is set and invalid, public model
downloads can still fail.

Inspect the current session:

```powershell
echo $env:HF_TOKEN
echo $env:HUGGING_FACE_HUB_TOKEN
```

Clear the variables for the current session:

```powershell
Remove-Item Env:HF_TOKEN -ErrorAction SilentlyContinue
Remove-Item Env:HUGGING_FACE_HUB_TOKEN -ErrorAction SilentlyContinue
```

If your environment requires an authenticated session, generate a new token with
read access and log in again:

```powershell
pip install -U huggingface_hub
hf auth login
```

`BAAI/bge-small-en-v1.5` is a public model. In most cases you do not need a paid
account; you only need to remove or replace a broken token.

## Temporary workaround: skip embeddings

If you need indexing to succeed immediately, you can skip embeddings and come
back to semantic retrieval later:

```powershell
$env:SKIP_EMBEDDINGS = "1"
.\scripts\ci_index_fixtures.ps1
```

Or for a normal repository:

```powershell
python -m cognis.cli.main bootstrap . --skip-embeddings
```

Lexical and structural retrieval will still work. Semantic search will remain
unavailable until you re-index without `--skip-embeddings`.
