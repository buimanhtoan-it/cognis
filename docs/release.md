# Release Guide

This guide prepares the public source release and the single prebuilt ZIP sold
through Polar.

## Distribution policy

- GitHub is the public Apache-2.0 source, tag, documentation, release-note, and
  managed-asset infrastructure.
- Polar receives exactly one File Download:
  `cognis-prebuilt-<version>.zip`.
- Do not publish or sell a separate VSIX, binary, container image, license key,
  activation benefit, or feature-gated edition.
- Anyone may build the same software from source for free.

## 1. Update versions

The root `[workspace.package].version` in `Cargo.toml` is the engine source of
truth. Keep these manual locations in lockstep:

- `apps/cognis-vscode/package.json`
- both root package version fields in `apps/cognis-vscode/package-lock.json`
- `site/config.json` `version`
- the baked `DEFAULTS.version` in `site/index.html`
- `CHANGELOG.md`

## 2. Run release gates

Engine, from the repository root with Cognis test-path variables unset:

```powershell
Remove-Item Env:\COGNIS_DB_PATH,Env:\COGNIS_MCP_FIXTURE,Env:\COGNIS_INDEXD_STATUS_PATH -ErrorAction SilentlyContinue
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

Extension, from `apps/cognis-vscode`:

```powershell
npm install
npm run compile
npm run lint
npm test
npm run test:e2e
npm run test:host
```

## 3. Tag the source release

Commit and push only after the gates pass. An annotated `v<version>` tag starts
the release workflow that builds checksum-verified engine/model assets required
by the managed installer.

```powershell
git tag -a v<version> -m "Release v<version>"
git push origin v<version>
```

Verify the workflow is green and that every supported platform engine, checksum,
and semantic model asset exists for the exact extension version. These assets
support **Install engine** inside the Polar-delivered extension; do not advertise
them as another end-user product download.

## 4. Build the Polar ZIP

Build the ordinary Apache-2.0 extension. There is no Pro key injection:

```powershell
Set-Location apps/cognis-vscode
npm run package
Set-Location ../..
```

Create a versioned `INSTALL.md` for the buyer, copy the Apache-2.0 license
notice, and package exactly these three files:

```powershell
Compress-Archive -Force -Path `
  apps/cognis-vscode/cognis-vscode-<version>.vsix, `
  business/dist/INSTALL.md, `
  apps/cognis-vscode/LICENSE.txt `
  -DestinationPath business/dist/cognis-prebuilt-<version>.zip
```

Inspect both the outer ZIP and nested VSIX. Confirm:

- names and manifest versions equal `<version>`;
- `INSTALL.md` mentions the same version;
- no license key, activation, private key, public-key injection, or seller
  secret is present;
- stale JavaScript is absent because `vscode:prepublish` cleans `out/`;
- README and LICENSE describe pure Rust and Apache-2.0;
- the extension's managed assets exist for `v<version>`.

## 5. Publish through Polar

Configure the Polar product as a one-time purchase with one **File Download**
benefit. Upload only `cognis-prebuilt-<version>.zip`.

Do not configure:

- a separate VSIX download;
- Polar license keys;
- an activation email or webhook;
- per-seat or per-minor software entitlements.

Update `site/config.json` and the baked site defaults only after that exact ZIP
is downloadable from Polar.

## Rollback

If the package is broken:

1. replace or remove the Polar File Download immediately;
2. stop advertising that ZIP version in `site/config.json`;
3. mark the corresponding GitHub release as pre-release if its managed assets
   are unsafe;
4. fix the issue, bump the patch version, rerun all gates, and upload a new ZIP.