# Changelog

All notable changes to Silent AI are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- The project-context **AI** button now pauses or resumes sending the selected
  folder to the LLM without clearing it. The choice persists across launches,
  which makes quick questions faster when the full project block is not needed.
- `llm.answer_language` pins the reply language (`uk`, `en`, `ru`, ...), while
  `auto` keeps following the language of the latest utterance.

### Fixed

- The expected `failed to encode` message from whisper.cpp when a partial STT
  job is preempted by a final is now logged at debug level instead of ERROR.
  Real encoder failures still surface at ERROR.
- A final STT job skipped as too short now clears the preempt flag, so it can no
  longer abort subsequent partial jobs.

## [0.1.1] - 2026-11-23

### Added

- Optional project-folder context for the LLM. The chat toolbar can select a
  local source folder, scan its text files with safe defaults, and include the
  selected project block in every voice or manual question. The selected path
  is remembered across launches and can be cleared from the toolbar.
- `[project]` configuration for file count, per-file size, total character
  budget, hidden files, and additional excluded directories.
- Project scanner that skips binaries, lockfiles, `.git`, `target`,
  `node_modules`, build output, hidden files, and obvious secret files.

## [0.1.0] - 2026-11-23

First public release.

### Added

- Cross-platform desktop app (Tauri 2) with an always-on-top, draggable,
  translucent window.
- System-audio capture:
  - Linux via `parec` from the monitor of the current default sink, with a
    cpal fallback;
  - Windows via WASAPI loopback;
  - macOS via Core Audio process tap, or BlackHole on macOS 13–14.5.
- Local Whisper STT through whisper.cpp, with CPU builds by default and
  optional `cuda`, `metal`, `coreml`, and `vulkan` features.
- Optional Deepgram streaming STT (`nova-3`) behind the `deepgram` feature.
- OpenAI-compatible LLM client with SSE streaming, history, retry, warmup, and
  provider-specific `extra_body`.
- Low-latency pipeline: WebRTC VAD, energy fallback, partial hypotheses,
  speculative LLM requests, and buffered tokens until the final transcript.
- Question heuristics for Ukrainian, English, and Russian, plus `answer_all`
  mode for statements and explanations.
- Chat tab with persistent history, per-stage latency chips, and the complete
  teacher transcript written to a daily log file.
- Notes tab: Markdown editor, rendered reading mode, formatting toolbar,
  checklists, code blocks, autosave, `Ctrl+S`, and notes stored outside the
  project with `600` permissions.
- Audio pause / keyboard-only mode: stop VAD/STT, ask manually, and resume
  without restarting the audio stream.
- English programming terms preserved through the Whisper `initial_prompt`, a
  deterministic glossary, and an LLM instruction.
- Screen-share exclusion:
  - Windows `WDA_EXCLUDEFROMCAPTURE`;
  - macOS `NSWindow.sharingType = .none`;
  - KDE Plasma 6.6+ KWin script `silent-ai-stealth` (`excludeFromCapture`).
- Global hotkeys for pause, manual question, copy, regenerate, and window
  visibility.
- Linux Wayland always-on-top through XWayland when available.
- CPU-friendly default `config.toml` and a gitignored `config.local.toml`
  override for GPU/provider/machine-specific settings.
- CI for Linux tests, clippy, Tauri checks, and Windows/macOS compile checks.
- Tag-triggered release workflow that builds CPU bundles for Windows, macOS
  (Intel and Apple Silicon), and Linux (AppImage and deb) into a draft GitHub
  release.
- Documentation for installation, configuration, audio capture, architecture,
  privacy, security, contribution, and release management.

### Fixed

- A missing default API-key file no longer prevents startup when no API key is
  configured at all, so local OpenAI-compatible endpoints such as Ollama work
  out of the box.

### Known limitations

- The first release is live-verified on Fedora 44 / KDE Plasma 6.8 / Wayland;
  Windows and macOS are covered by CI builds and platform code but need broader
  field testing.
- macOS 13–14.5 requires BlackHole; native process taps require macOS 14.6+.
- Per-window screen-cast exclusion on Linux is available only on KDE Plasma
  6.6+ through the bundled KWin script. GNOME Wayland has no equivalent API.
- GitHub release assets are CPU-only; GPU and Deepgram builds are source builds.
- Audio devices are not reconnected automatically after a change.
- Binaries are not code-signed or notarized in this release.
