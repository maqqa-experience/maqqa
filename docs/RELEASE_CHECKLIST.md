# Release checklist

Use before tagging `v0.x.y` or opening the repo publicly.

## Must

- [ ] `LICENSE` present (MIT)
- [ ] CI green on `main` (fmt, clippy, tests, desktop `npm run build`)
- [ ] README status banner still accurate (`docs/STATUS.md`)
- [ ] `config.example.toml` matches current config keys
- [ ] No secrets in tree (`.env`, ngrok URLs with credentials, API keys)
- [ ] `artifacts/` and demo media not committed (gitignored)
- [x] CI badge / Cargo `repository` → `maqqa-experience/maqqa`

## Should

- [ ] `CHANGELOG.md` updated for the tag
- [ ] `cargo test -p vibe_core --lib` and `cargo test -p vibe_ipc` locally
- [ ] Short scenario smoke: `vibe_scenario --from 1 --to 5` with a pulled model
- [ ] Desktop launches (`npm run tauri dev`) on a clean machine once

## macOS binary

Local (Apple Silicon or Intel host):

```bash
./scripts/build_macos_release.sh
# → dist/macos/*.dmg, *.app, maqqa-cli-*.tar.gz
```

CI: push a version tag; workflows `Release macOS` and `Release Linux / Windows` attach installers + CLI tarballs to a **draft** GitHub Release.

```bash
git tag -a v0.1.0 -m "v0.1.0 preview"
git push origin v0.1.0
```

First-run Gatekeeper (unsigned): right-click → Open, or `xattr -cr "Maqqa.app"`.

Apple Developer ID + notarization are optional later (not wired in CI yet).

## Tag / Release notes

Paste the matching `CHANGELOG` section; note **Ollama + model** requirements and that the app is unsigned preview.
