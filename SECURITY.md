# Security Policy

## Supported versions

| Version | Supported |
|---|---|
| `0.1.x` | Yes |
| Older | No |

Silent AI is a young project. Security fixes are applied to the latest release
and `main`.

## Reporting a vulnerability

Please do **not** open a public issue for a security problem.

Use GitHub's private vulnerability reporting for this repository:

1. Open <https://github.com/illa-j/silent-ai/security/advisories/new>.
2. Include the affected version, platform, a reproduction, and the impact.

If private reporting is unavailable, contact the maintainers through the
repository's issue tracker without including exploit details, and ask for a
private channel.

Please allow reasonable time for a fix before public disclosure.

## What the app stores and sends

Silent AI is designed to run locally, but it can send data to services the user
configures.

### Stays local

- Captured audio is processed in memory. The app does not write WAV files while
  running.
- Local Whisper inference does not require a network after the model is
  downloaded.
- Notes: `~/.local/share/silent-ai/notes.md` (or the platform equivalent).
- Selected project path: `~/.local/share/silent-ai/project_path`.
- Project-context on/off switch: `~/.local/share/silent-ai/project_enabled`.
- Transcript log: `~/.local/share/silent-ai/transcripts/` when
  `logging.transcript = true`.
- Logs: `~/.local/share/silent-ai/logs/`.
- API keys in files: `~/.config/silent-ai/api_key` and
  `~/.config/silent-ai/deepgram_key` (or the platform config directory).

### Can leave the machine

- **LLM provider:** the latest transcript, the configured history window, the
  system prompt, and — when the project **AI** switch is on — the text of the
  selected project folder are sent to
  `llm.base_url`. The folder scanner skips binaries, lockfiles, common build
  directories, and obvious secret files, but it cannot detect a secret
  hardcoded in a source file. Review the project before selecting it.
- **Deepgram (optional):** audio streams to Deepgram when
  `stt.backend = "deepgram"` and the feature is compiled in.
- **Model download:** the first Whisper launch downloads the model from the
  URL configured in `stt.whisper.model_url` (empty means the official
  whisper.cpp Hugging Face URL).

There is no telemetry.

## Key handling

- The key is read from the environment variable named by `llm.api_key_env`, or
  from `llm.api_key_file`.
- The key is never written to `config.toml`, chat history, or logs.
- On Linux/macOS, create key files with `chmod 600`.
- On Windows, protect the key file with your user account's ACLs.
- Prefer a dedicated, revocable API key with a spending limit.

## Desktop-specific notes

- **KDE stealth script:** `scripts/install-kde-stealth.sh` copies a local KWin
  script and enables it with `kwriteconfig6`. It does not download code. Review
  `packaging/kwin/silent-ai-stealth/` before installing.
- **Screen-share exclusion** reduces accidental disclosure but is not a
  security boundary. A compositor update, a different desktop, or a capture
  method can change the behavior. Do not rely on it for confidential material.
- **Release binaries** in the first release are unsigned. Verify the SHA-256
  digest GitHub shows for each release asset, and build from source if that
  matters for your threat model.

## Dependencies

Dependency vulnerabilities can be reported through the same private channel.
Please include the crate name and version from `Cargo.lock`.
