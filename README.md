<p align="center">
  <img src="app/icons/128x128.png" width="88" alt="Silent AI">
</p>

<h1 align="center">Silent AI</h1>

<p align="center">
  Тихий помічник для онлайн-лекцій: слухає системний звук, локально транскрибує<br>
  мову викладача, відповідає через ваш LLM і не потрапляє у screen share.
</p>

<p align="center">
  <a href="https://github.com/illa-j/silent-ai/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/illa-j/silent-ai/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/illa-j/silent-ai/releases"><img alt="Release" src="https://img.shields.io/github/v/tag/illa-j/silent-ai?label=release&amp;sort=semver"></a>
  <img alt="Linux, Windows, macOS" src="https://img.shields.io/badge/platform-Linux%20%7C%20Windows%20%7C%20macOS-4c8dff">
  <img alt="MIT" src="https://img.shields.io/badge/license-MIT-green">
  <img alt="Rust 1.90+" src="https://img.shields.io/badge/Rust-1.90%2B-orange?logo=rust">
  <img alt="Local Whisper" src="https://img.shields.io/badge/STT-local%20Whisper-8a5cf6">
</p>

**Silent AI** — маленьке always-on-top вікно для Google Meet, Zoom або Teams.
Воно захоплює **системний звук** (те, що чуєте ви, а не мікрофон), розпізнає
запитання викладача локальним Whisper і показує відповідь від будь-якого
OpenAI-сумісного LLM. Окрема вкладка нотаток — ваш особистий Markdown-блокнот,
який **не надсилається AI**.

> A low-latency, cross-platform meeting copilot. System-audio capture, local
> Whisper STT, OpenAI-compatible LLM, private Markdown notes, and KDE
> screen-cast exclusion.

---

## Можливості

- **Системний звук, а не мікрофон.** Linux — monitor-джерело поточного sink
  через `parec` (PulseAudio/PipeWire); Windows — WASAPI loopback; macOS —
  Core Audio tap або BlackHole.
- **Локальний Whisper за замовчуванням.** Аудіо не залишає машину; модель
  завантажується один раз і працює офлайн.
- **Низька затримка.** VAD endpoint, спекулятивний LLM-запит на partial-гіпотезі
  та буферизація токенів до готового фінального транскрипту.
- **Повна історія чату.** Слова викладача та відповіді не зникають; повний
  транскрипт додатково пишеться у файл.
- **Нотатки.** Окрема вкладка з Markdown-редактором, режимом читання,
  форматуванням, чеклістами, автозбереженням і `Ctrl+S`.
- **Контекст проєкту.** Виберіть папку з кодом — AI отримає її текстові файли
  як reference-контекст до кожного запитання.
- **Режим «тільки клавіатура».** Пауза звуку одним горячим ключем; далі
  запитання вводяться вручну.
- **Не потрапляє у ScreenCast на KDE Plasma 6.6+** завдяки KWin-скрипту.
- **Англійські терміни залишаються англійськими.** `initial_prompt`, глосарій
  та інструкція для LLM зменшують транслітерацію (`Singleton`, `interface`,
  `C#`, `async`).
- **CPU за замовчуванням.** CUDA, Metal, CoreML, Vulkan і Deepgram — опційні
  feature-флаги.

---

## Швидкий старт

### 1. Завантажте збірку

Готові CPU-збірки публікуються в [**GitHub Releases**](https://github.com/illa-j/silent-ai/releases):

| Платформа | Файли |
|---|---|
| Windows 10 2004+ / 11 | `.msi` або NSIS `.exe` |
| macOS 13+ | `.dmg` для Intel (`x86_64`) та Apple Silicon (`aarch64`) |
| Linux x86_64 | `.AppImage` або `.deb` |

Збірки поки що **без підпису**. Windows SmartScreen може показати попередження
(«More info → Run anyway»), macOS — «Open» через контекстне меню або
System Settings → Privacy & Security → Open Anyway.

Linux AppImage:

```bash
chmod +x ./*AI*.AppImage
./*AI*.AppImage
```

Якщо немає FUSE:

```bash
./*AI*.AppImage --appimage-extract-and-run
```

Debian/Ubuntu:

```bash
sudo apt install ./*AI*.deb
```

### 2. Дайте ключ LLM

Потрібен OpenAI-сумісний endpoint. Найпростіше — файл із ключем, який
працює і при запуску з меню:

```bash
mkdir -p ~/.config/silent-ai
printf '%s' 'YOUR_API_KEY' > ~/.config/silent-ai/api_key
chmod 600 ~/.config/silent-ai/api_key
```

macOS: `~/Library/Application Support/silent-ai/api_key`.
Windows: `%APPDATA%\silent-ai\api_key`.

Або змінна середовища:

```bash
export OPENCODE_API_KEY='...'
```

`llm.api_key_env` містить **назву** змінної, а не ключ. Якщо змінна задана і
непорожня, вона має пріоритет; інакше читається файл.

### 3. Налаштуйте LLM (за потреби)

Застосунок шукає конфіг у такому порядку:

1. `./config.local.toml` — локальні/персональні налаштування (gitignored);
2. `./config.toml` — публічний CPU-дефолт із репозиторію;
3. системний каталог конфігурації:
   - Linux: `~/.config/silent-ai/config.toml`
   - macOS: `~/Library/Application Support/silent-ai/config.toml`
   - Windows: `%APPDATA%\silent-ai\config.toml`
4. вбудовані дефолти.

Для встановленого застосунку створіть файл у системному каталозі, якщо
потрібно змінити дефолти:

```bash
mkdir -p ~/.config/silent-ai
cat > ~/.config/silent-ai/config.toml <<'EOF'
[llm]
base_url = "https://opencode.ai/zen/go/v1"
model = "glm-5.3-flash"
api_key_env = "OPENCODE_API_KEY"
max_tokens = 4096
timeout_ms = 120000
answer_all = true
EOF
```

Повний довідковий файл із коментарями — [config.toml](config.toml) у
репозиторії.

Інші варіанти — окремі приклади `[llm]`:

```toml
# OpenAI
[llm]
base_url = "https://api.openai.com/v1"
model = "gpt-4.1-mini"
api_key_env = "OPENAI_API_KEY"
```

```toml
# локальний Ollama (без ключа)
[llm]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5:7b"
api_key_env = ""
```

```toml
# OpenCode Zen (pay-as-you-go)
[llm]
base_url = "https://opencode.ai/zen/v1"
```

Якщо провайдер не розуміє `reasoning_effort`, приберіть `extra_body` з
`[llm]`. Для швидкої відповіді обирайте non-reasoning модель.

Щоб модель відповідала завжди українською, навіть на англійське чи російське
запитання, додайте в `[llm]`:

```toml
answer_language = "uk"
```

`"auto"` (типово) — відповідати мовою останньої фрази; також можна вказати
`"en"`, `"ru"`, `"pl"` тощо.

### 4. Запустіть

```bash
silent-ai
```

При першому запуску завантажиться `ggml-small-q5_1.bin` у
`~/.cache/silent-ai/models`. Windows: `%USERPROFILE%\.cache\silent-ai\models`.

Перевірити пристрої та завантажити модель заздалегідь:

```bash
silent-ai --list-devices
silent-ai --download-model
```

---

## Системний звук

| Платформа | Як захоплюється |
|---|---|
| **Linux** | `parec` з monitor-джерела поточного default sink; cpal — fallback. `audio.device = "auto"` обирає правильний monitor. |
| **Windows** | WASAPI loopback: output-пристрій відкривається як input. Окремий Stereo Mix не потрібен. |
| **macOS 14.6+** | Core Audio process tap; дозвольте Screen & System Audio Recording. |
| **macOS 13–14.5** | Потрібен [BlackHole 2ch](https://existential.audio/blackhole/) + Multi-Output Device. |

Деталі: [Linux](docs/audio-linux.md) · [Windows](docs/audio-windows.md) ·
[macOS](docs/audio-macos.md).

Linux-перевірка:

```bash
pactl get-default-sink
pactl list short sources | grep monitor
silent-ai --list-devices   # шукайте "Monitor of ..."
```

Якщо Meet грає в інший пристрій, оберіть його monitor у `config.toml`:

```toml
[audio]
device = "pulseaudio:alsa_output.pci-0000_00_1f.3.analog-stereo.monitor"
```

---

## Використання

### Гарячі клавіші

| Дія | Типово |
|---|---|
| Пауза / увімкнути звук | `Ctrl/Cmd+Shift+Space` |
| Ручне запитання | `Ctrl/Cmd+Shift+Q` |
| Копіювати відповідь | `Ctrl/Cmd+Shift+C` |
| Перегенерувати | `Ctrl/Cmd+Shift+R` |
| Приховати / показати вікно | `Ctrl/Cmd+Shift+H` |
| Перемкнути чат / нотатки | `Ctrl+Shift+N` (у вікні) |
| Зберегти нотатки | `Ctrl+S` (у редакторі) |

### Чат

- Слова викладача з'являються зліва, відповіді AI — справа.
- Історія не очищується; нові відповіді додаються нижче.
- Під чатом — таймінги VAD / STT / LLM / UI / TOTAL; перевищення бюджету
  підсвічується.
- Повний транскрипт: `~/.local/share/silent-ai/transcripts/transcript.log.YYYY-MM-DD`
  (вимикається через `logging.transcript = false`).

### Нотатки

Вкладка **НОТАТКИ** — особистий Markdown-блокнот:

- `✎` — редактор, `👁` — режим читання;
- панель форматування: заголовки, жирний/курсив, списки, чекліст, код, цитата;
- автозбереження і `Ctrl+S`;
- файл: `~/.local/share/silent-ai/notes.md` (права `600` на Linux/macOS);
- **не надсилається AI** і не потрапляє в історію чату.

### Контекст проєкту

У вкладці **ЧАТ** є компактна панель проєкту:

1. Натисніть **📁** і виберіть кореневу папку проєкту.
2. Застосунок просканує текстові файли й покаже назву, кількість файлів і
   обсяг контексту.
3. Далі кожне запитання — і голосове, і ручне — надсилається в LLM разом із
   цим контекстом.
4. Кнопка **AI** вимикає або вмикає надсилання контексту, не скидаючи вибрану
   папку — зручно для швидких запитань, коли 48k символів тільки додають
   затримку.
5. **×** прибирає контекст; вибрана папка зберігається між запусками.

Що потрапляє в контекст:

- вихідний код, конфіги, README та текстові файли;
- спершу манифести (`Cargo.toml`, `package.json`, `pyproject.toml`), потім
  source-файли;
- ліміти: `project.max_files` (80), `project.max_file_bytes` (64 KiB) і
  `project.max_chars` (48 000).

Блок додається до **кожного** LLM-запиту, тому більший `max_chars` означає
більше токенів і повільнішу/дорожчу відповідь. Для великого монорепо почніть із
меншого ліміту або виберіть підпапку.

Що ігнорується:

- `.git`, `target`, `node_modules`, `dist`, `build`, `.venv`, `Pods` та інші
  build-каталоги;
- бінарники, lock-файли, `.env`, ключі, `*.pem`, `credentials*`;
- приховані файли й каталоги (крім `.env.example`).

Це best-effort фільтр: якщо в коді є hardcoded секрети, вони можуть потрапити
в контекст. Перед вибором папки переконайтеся, що її можна надсилати вашому
LLM-провайдеру.

### Тільки клавіатура

1. `Ctrl+Shift+Space` або `⏸` — звук вимикається, VAD/STT більше не
   створюють фраз.
2. Поле введення відкривається автоматично і залишається відкритим після
   кожного запитання.
3. `Ctrl+Shift+Q` або `⌨` — відкрити поле в будь-який момент.
4. `▶` — знову ввімкнути звук.

Ручні запитання йдуть у той самий LLM і зберігаються в чаті.

---

## Screen share

| Платформа | Поведінка |
|---|---|
| **Windows 10 2004+ / 11** | `WDA_EXCLUDEFROMCAPTURE`: вікно бачите ви, але не viewer. |
| **macOS** | `NSWindow.sharingType = .none`. |
| **KDE Plasma 6.6+** | KWin `excludeFromCapture`: вікно видиме локально, але не в ScreenCast. |
| **GNOME Wayland** | Per-window exclusion API немає. Діліться вкладкою/вікном Chrome або увімкніть auto-hide. |

KDE Plasma — один раз:

```bash
./scripts/install-kde-stealth.sh
```

Перевірка:

```bash
journalctl --user -n 50 | grep -i 'Silent AI: excludeFromCapture'
```

Скрипт ставить `excludeFromCapture=true` для вікна `SILENT AI`. Для
always-on-top GUI на Wayland типово працює через XWayland
(`ui.force_x11_on_wayland = true`).

GNOME, якщо потрібно приховати вікно під час демонстрації:

```toml
[stealth]
auto_hide_on_screen_share = true
```

---

## Конфігурація

Повний файл із коментарями — [config.toml](config.toml). Основне:

| Параметр | Призначення |
|---|---|
| `audio.mode` | `system` (зустріч) або `microphone` |
| `audio.device` | `auto`, ID з `--list-devices`, назва або підрядок |
| `stt.backend` | `whisper` (локально) або `deepgram` (хмара) |
| `stt.whisper.model` | `small-q5_1` (CPU), `large-v3-turbo-q5_0` / `distil-large-v3` (GPU) |
| `stt.whisper.language` | `uk`, `ru`, `en` або `auto` |
| `stt.whisper.gpu` | `true` лише для GPU-збірки |
| `llm.base_url` / `model` | OpenAI-сумісний endpoint і модель |
| `llm.api_key_env` / `api_key_file` | назва env-змінної / файл із ключем |
| `llm.max_tokens` | до 4096 токенів |
| `llm.answer_all` | відповідати на всі фрази, не лише питання |
| `llm.answer_language` | `auto`, `uk`, `en`, `ru` — мова відповіді |
| `llm.timeout_ms` | 120000 ms на весь стрім |
| `vad.silence_ms` | 600 ms тиші до endpoint |
| `detector.send_all` | передавати в LLM усі фінальні фрази |
| `project.max_files` | максимум файлів у контексті проєкту (80) |
| `project.max_chars` | максимум символів контексту (48 000) |
| `logging.transcript` | писати повний транскрипт у файл |
| `ui.always_on_top` | тримати вікно поверх інших |
| `stealth.*` | screen-share поведінка |

Локальні зміни (GPU, інший провайдер, персональні шляхи) тримайте у
`config.local.toml` — він має пріоритет і не комітиться.

---

## Приватність

- Аудіо обробляється в пам'яті; Silent AI не зберігає WAV під час роботи.
- Локальний Whisper не потребує мережі після завантаження моделі.
- Якщо обрано хмарний LLM, туди йде транскрипт запитання, контекст останніх
  секунд і текст вибраної папки проєкту. Не використовуйте хмару для
  конфіденційних зустрічей або кодів, які не можна передавати провайдеру.
- **Deepgram** (опційно) надсилає аудіо в хмару; за замовчуванням вимкнений.
- Ключі читаються з env-змінної або файлу з правами `600`; у `config.toml`,
  логи та чат вони не потрапляють.
- Нотатки й транскрипти зберігаються локально у `~/.local/share/silent-ai/`.
- Телеметрії немає.

---

## Збірка з джерел

### Залежності

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

Потрібен Rust **1.90+**, CMake і C++ компілятор. `libclang` — лише для
`native-pipewire`.

### CPU (типова збірка)

```bash
cargo build -p silent-ai-app --release
./target/release/silent-ai
```

### GPU та опційні бекенди

```bash
# NVIDIA CUDA (потрібен CUDA Toolkit + nvcc)
cargo build -p silent-ai-app --release --features cuda

# macOS
cargo build -p silent-ai-app --release --features metal
cargo build -p silent-ai-app --release --features coreml

# AMD/Intel GPU
cargo build -p silent-ai-app --release --features vulkan

# Deepgram cloud STT
cargo build -p silent-ai-app --release --features deepgram

# Нативний PipeWire замість PulseAudio-сумісного шару
cargo build -p silent-ai-app --release --features native-pipewire
```

Для GPU-збірки поставте `stt.whisper.gpu = true` і, за потреби,
`flash_attn = true`. Release-асети GitHub — **CPU-only**; GPU-версії
збираються з джерел.

### Пакети

```bash
cargo install tauri-cli --version "^2" --locked
cd app
cargo tauri build
```

Артефакти: `target/release/bundle/` (`.deb`, `.AppImage`, `.msi`, `.exe`,
`.dmg`).

---

## Діагностика

**Немає звуку на Linux**

```bash
systemctl --user status pipewire pipewire-pulse wireplumber
pactl info
pactl get-default-sink
pactl list short sources | grep monitor
silent-ai --list-devices
```

Переконайтеся, що Meet грає в default sink, а `audio.device = "auto"` або
вказує на його monitor. Нативний PipeWire-хост потребує окремої збірки.

**Немає звуку на Windows**

- Оберіть playback-пристрій, а не мікрофон.
- Вимкніть exclusive mode для пристрою.
- WASAPI loopback є з Windows 10 2004+.

**Немає звуку на macOS**

- System Settings → Privacy & Security → Screen & System Audio Recording →
  дозвольте Silent AI, перезапустіть.
- macOS 13–14.5: перевірте BlackHole і Multi-Output Device.
- macOS 14.6+: спробуйте інший playback-пристрій.

**LLM: `environment variable ... is not set`**

- `llm.api_key_env` — назва змінної, не ключ.
- Для запуску з меню використовуйте `api_key_file`.
- Ключ у полі `api_key_env` викликає окрему помилку.

**LLM: HTTP 402 / insufficient balance**

Перевірте, що `base_url` відповідає вашому тарифу. OpenCode Go і Zen —
різні billing-ендпоінти. Для Ollama ключ не потрібен (`api_key_env = ""`).

**Повільно на CPU**

- `small-q5_1` — баланс; `base-q5_1` або `tiny-q5_1` — швидше.
- `threads = 0` використовує фізичні ядра.
- `adaptive_audio_ctx = true` вже увімкнено.
- GPU-збірка з `large-v3-turbo-q5_0` значно швидша.

**Вікно не поверх інших на Wayland**

Перевірте, що доступний XWayland і `ui.force_x11_on_wayland = true`. Без
XWayland композитор може ігнорувати always-on-top.

**Погано розпізнає українську**

`language = "uk"` точніше за `auto` на коротких фразах. Для російської —
`ru`, для змішаних лекцій — `auto`. Складнішу мову/акцент обробляє
`medium-q5_0` або GPU-модель.

---

## Обмеження

- Основна live-перевірка першого релізу — **Fedora 44 / KDE Plasma 6.8 /
  Wayland**. Windows і macOS проходять CI-збірку; [повідомлення про проблеми](https://github.com/illa-j/silent-ai/issues)
  цих платформ вітаються.
- macOS 13–14.5 потребує BlackHole; нативний process tap — з 14.6.
- Linux per-window exclusion є лише для KDE Plasma 6.6+ (KWin-скрипт).
  GNOME Wayland не має такого API.
- Release-асети — CPU-only. CUDA/Metal/CoreML/Vulkan збираються локально.
- Немає автоматичного перепідключення при зміні аудіопристрою — перезапустіть
  застосунок.
- Затримка LLM залежить від провайдера й мережі; спекуляція не гарантує
  бюджет для будь-якої моделі.
- Бінарники не підписані й не нотаризовані.

---

## Розробка

```
core/                  audio, VAD, STT, detector, LLM, pipeline
app/                   Tauri 2 застосунок, UI, гарячі клавіші
bench/                 WAV/live benchmark
config.toml            публічний CPU-конфіг
config.local.toml      локальні налаштування (gitignored)
docs/                  архітектура та інструкції зі звуку
packaging/kwin/        KWin-скрипт для ScreenCast
scripts/               інсталятор KDE stealth, фікстури
```

Перевірки:

```bash
cargo fmt --all -- --check
cargo test -p silent-ai-core --lib --features deepgram
cargo test -p silent-ai-core --features test-support --test llm_no_question
cargo test -p silent-ai-core --test llm_project_context
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p silent-ai-app
cargo build -p silent-ai-app --release
```

Деталі архітектури: [docs/architecture.md](docs/architecture.md).
Правила внеску: [CONTRIBUTING.md](CONTRIBUTING.md).
Безпека: [SECURITY.md](SECURITY.md).
Історія змін: [CHANGELOG.md](CHANGELOG.md).
Чекліст релізу: [RELEASE.md](RELEASE.md).

---

## Ліцензія

[MIT](LICENSE) © 2026 Silent AI contributors.
