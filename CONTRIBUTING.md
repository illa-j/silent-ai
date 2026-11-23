# Contributing to Silent AI

Thanks for helping improve Silent AI. Bug reports, platform testing, docs, and
focused pull requests are welcome.

## Development setup

```bash
git clone https://github.com/illa-j/silent-ai.git
cd silent-ai
```

You need Rust 1.90+, CMake, a C++ compiler, and the Tauri/WebKitGTK
dependencies for your platform.

Fedora:

```bash
sudo dnf install -y \
  cmake gcc-c++ pkgconf-pkg-config \
  alsa-lib-devel \
  webkit2gtk4.1-devel javascriptcoregtk4.1-devel \
  libsoup3-devel gtk3-devel librsvg2-devel \
  pipewire-utils dbus-tools pulseaudio-utils
```

Debian/Ubuntu 24.04:

```bash
sudo apt update
sudo apt install -y \
  build-essential cmake pkg-config \
  libasound2-dev \
  libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev \
  libsoup-3.0-dev libgtk-3-dev \
  librsvg2-dev patchelf libayatana-appindicator3-dev libxdo-dev \
  pipewire-bin dbus pulseaudio-utils
```

Build and run:

```bash
cargo build -p silent-ai-app --release
./target/release/silent-ai --list-devices
./target/release/silent-ai
```

## Checks before opening a pull request

```bash
cargo fmt --all -- --check
cargo test -p silent-ai-core --lib --features deepgram
cargo test -p silent-ai-core --features test-support --test llm_no_question
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p silent-ai-app
```

For changes that touch the default configuration, also run:

```bash
cargo build -p silent-ai-app --release
```

If your change affects a platform-specific path (audio, stealth, hotkeys,
packaging), say which OS and session type you tested. CI covers Linux builds
and Windows/macOS compile checks, but not physical audio hardware.

## Code guidelines

- Keep the realtime audio callback allocation-free. It should only push samples
  into the ring buffer.
- Keep blocking work (Whisper, resampling, VAD) off the async runtime threads.
- Preserve cancellation: every LLM request and speculative task must remain
  cancellable.
- Add or update tests for parser, config, detector, glossary, and pipeline
  changes.
- Keep the public `config.toml` CPU-friendly. Put personal or GPU settings in a
  gitignored `config.local.toml`.
- Do not add telemetry or network calls that are not required for the user's
  configured STT/LLM provider.

## Configuration changes

`config.toml` is the documented public default. Built-in defaults in
`core/src/config.rs` must stay aligned with the fields covered by the
`repository_config_matches_built_in_defaults` test. If you add a new setting,
add it to:

1. the config struct and `Default` implementation;
2. `config.toml` with a short comment;
3. validation, if it has a safe range;
4. the README table or docs;
5. a test when parsing or defaults could regress.

## Secrets and local data

Never commit:

- API keys, Deepgram keys, tokens, or `.env` files;
- `config.local.toml`;
- notes, transcripts, logs, downloaded models, or bundled binaries;
- personal paths or machine-specific configuration.

Use a key file outside the repository:

```bash
mkdir -p ~/.config/silent-ai
printf '%s' 'YOUR_KEY' > ~/.config/silent-ai/api_key
chmod 600 ~/.config/silent-ai/api_key
```

If you accidentally commit a secret, rotate it immediately and mention it in
the pull request.

## Commits and pull requests

- Use short, imperative commit subjects, for example
  `fix(linux): keep parec alive after an answer`.
- Keep one logical change per pull request when practical.
- Describe user-visible behavior, platforms tested, and any known limitations.
- Link the issue being fixed.
- Update `CHANGELOG.md` under `[Unreleased]` for user-visible changes.

By contributing, you agree that your contribution is licensed under the MIT
License.
