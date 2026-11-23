## What does this change?

<!-- A short description of the problem and the solution. -->

## How was it tested?

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test -p silent-ai-core --lib --features deepgram`
- [ ] `cargo test -p silent-ai-core --features test-support --test llm_no_question`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo check -p silent-ai-app`
- [ ] `cargo build -p silent-ai-app --release`

Platforms tested:

<!-- Fedora 44 / KDE Wayland, Windows 11, macOS 15, etc. -->

## Checklist

- [ ] I did not include API keys, `config.local.toml`, notes, transcripts, or logs.
- [ ] I updated `CHANGELOG.md` under `[Unreleased]` for user-visible changes.
- [ ] I updated `config.toml`, defaults, validation, and docs if I added a setting.
- [ ] I kept the realtime audio path allocation-free.
- [ ] My change is licensed under the MIT License.
