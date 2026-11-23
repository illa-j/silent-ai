# Release checklist

This project publishes CPU builds for Windows, macOS, and Linux from a Git tag.
The workflow creates a **draft** GitHub release; a human reviews it before
publishing.

## 1. Prepare `main`

```bash
git switch main
git pull --ff-only
git status --short
```

The working tree must be clean, and CI on `main` must be green.

## 2. Bump the version

The version lives in two places and must match the tag:

- `Cargo.toml` → `[workspace.package] version`
- `app/tauri.conf.json` → `version`

For example, `0.1.0` → `0.1.0` for the first release, then `0.1.1`, `0.2.0`,
and so on.

Update `CHANGELOG.md`:

- move user-visible items from `[Unreleased]` into the new version section;
- use the release date in `YYYY-MM-DD` form;
- add a fresh empty `[Unreleased]` section.

## 3. Run the release checks

Use a clean CPU build; this is what the GitHub workflow ships.

```bash
cargo fmt --all -- --check
cargo test -p silent-ai-core --lib --features deepgram
cargo test -p silent-ai-core --features test-support --test llm_no_question
cargo test -p silent-ai-core --test llm_project_context
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p silent-ai-app
cargo build -p silent-ai-app --release
```

Smoke-test the CPU binary on the host:

```bash
./target/release/silent-ai --help
./target/release/silent-ai --list-devices
./target/release/silent-ai --headless
```

At minimum, confirm that the GUI starts, the configured model loads, one manual
question receives an answer, and the notes tab saves a file.

Optional GPU build (not shipped as a release asset):

```bash
cargo build -p silent-ai-app --release --features cuda
```

## 4. Check for secrets and local files

```bash
git status --short
git ls-files | grep -E '(^|/)(config\.local|notes|transcript|\.env|api_key|deepgram_key)' || true
git grep -nE '(sk-[A-Za-z0-9]{16,}|Bearer [A-Za-z0-9._-]{12,})' || true
```

`config.toml` must remain CPU-friendly (`small-q5_1`, `gpu = false`). Personal
settings belong in the gitignored `config.local.toml`.

## 5. Commit and tag

```bash
git add -A
git commit -m "release: v0.1.0"
git tag -a v0.1.0 -m "Silent AI v0.1.0"
git push origin main
git push origin v0.1.0
```

The tag starts `.github/workflows/release.yml`. The workflow:

1. verifies that the tag matches the workspace version;
2. builds CPU bundles:
   - Windows: `.msi`, NSIS `.exe`;
   - macOS Intel: `.dmg`;
   - macOS Apple Silicon: `.dmg`;
   - Linux: `.AppImage`, `.deb`;
3. creates a draft release named `Silent AI v0.1.0`.

## 6. Review the draft release

In GitHub → **Releases** → the draft:

- Check that all platform assets are present and non-trivial in size.
- Download at least one asset and verify its SHA-256 against the digest shown
  by GitHub.
- Install and launch it on a clean machine or VM when possible.
- Confirm the release notes mention CPU-only assets and the LLM requirement.
- Confirm the tag and title match the version.
- Mark as pre-release only if it is intentionally incomplete.

Then publish.

## 7. After publishing

- Verify the README download links work (or point users to **Releases**).
- Announce the release with the support matrix and limitations.
- Open issues for platform reports.
- Bump `main` to the next development version when the next cycle starts
  (optional for `0.x`).
- Keep the previous release assets available for rollback.

## What is intentionally not automated

- Code signing and notarization (Windows/macOS assets are unsigned).
- GPU (CUDA/Metal/CoreML/Vulkan) bundles.
- Deepgram-enabled bundles.
- AUR/Flatpak/Snap/Homebrew packages.

Those can be added later without changing the tag scheme.
