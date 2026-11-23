# macOS 13+: системний звук

macOS не дає звичайного loopback для output-пристрою, як Windows. Є два шляхи:

| macOS | Спосіб |
|---|---|
| **14.6+** | Core Audio process tap (cpal 0.18 робить це автоматично, коли ви відкриваєте output-пристрій як input) |
| **13 – 14.5** | BlackHole 2ch + Multi-Output Device |

Silent AI підтримує обидва. На 13–14.5 `mode = "system"` + `device = "auto"` автоматично шукає вхід BlackHole; якщо його немає — видає зрозумілу помилку.

## 1. Дозволи

System Settings → Privacy & Security:

- **Screen & System Audio Recording** — для Core Audio taps / системного звуку.
- **Microphone** — лише якщо `audio.mode = "microphone"`.

`app/Info.plist` містить `NSAudioCaptureUsageDescription` і `NSMicrophoneUsageDescription`; Tauri зливає їх із згенерованим Info.plist. Без першого ключа macOS не показує запит на System Audio Recording і tap не створюється.

Після першої спроби macOS може показати запит; дозвольте й **перезапустіть Silent AI**. Якщо випадково відмовили:

```bash
# Скидає всі дозволи лише для цього застосунку; macOS запитає знову.
tccutil reset All io.github.illa-j.silent-ai
```

(Ідентифікатор застосунку — `io.github.illa-j.silent-ai`.)

## 2. macOS 14.6+ (нативні taps)

```bash
silent-ai --list-devices
```

Output-пристрої мають підказку `Core Audio tap on macOS 14.6+`. Виберіть той, у який грає Google Meet:

```toml
[audio]
mode = "system"
device = "auto"
# або точна назва:
# device = "MacBook Pro Speakers"
# device = "External Headphones"
```

Нічого встановлювати не потрібно. cpal створює приватний aggregate device з process tap; звук далі йде в колонки (tap не глушить вихід).

## 3. macOS 13 – 14.5 (BlackHole)

### 3.1 Встановіть BlackHole 2ch

```bash
brew install blackhole-2ch
```

або з [existential.audio/blackhole](https://existential.audio/blackhole/). Перезавантажте Mac, якщо система просить.

### 3.2 Створіть Multi-Output Device

1. Відкрийте **Audio MIDI Setup** (`/Applications/Utilities/Audio MIDI Setup.app`).
2. `+` → **Create Multi-Output Device**.
3. Увімкніть:
   - ваші колонки/навушники (Master);
   - **BlackHole 2ch**.
4. Перейменуйте, наприклад `Meet + BlackHole`.
5. У System Settings → Sound → Output виберіть цей Multi-Output Device.

Тепер увесь системний звук іде і в колонки, і у віртуальний вхід.

### 3.3 Вкажіть BlackHole у конфізі

```bash
silent-ai --list-devices
```

Знайдіть `BlackHole 2ch` як `input`:

```toml
[audio]
mode = "system"
device = "BlackHole 2ch"
```

### 3.4 Перевірка

```bash
silent-ai --headless
```
Увімкніть запитання в Meet. У консолі має з'явитися транскрипт.

## 4. Типові проблеми

**«System audio on macOS 13–14.5 needs BlackHole 2ch»**
- BlackHole не встановлено або не видно як input. Перевірте `--list-devices`.
- Після встановлення драйвера потрібен reboot.

**BlackHole є, але тиша**
- Системний вихід не направлено на Multi-Output Device, у якому є BlackHole.
- У Multi-Output Device вимкнено BlackHole або неправильний master.
- Meet грає в інший пристрій (перевірте вихід у самому браузері).

**macOS 14.6+, але тиша**
- Немає дозволу Screen & System Audio Recording.
- Спробуйте інший output; іноді aggregate/Bluetooth-пристрої поводяться нестабільно.
- Перезапустіть застосунок після зміни output.

**Дуже тихий звук**
- Перевірте рівень у Audio MIDI Setup для BlackHole/Multi-Output.
- Увімкніть `vad.energy_fallback_dbfs = -42` для тихіших записів.

**Мікрофон**
```toml
[audio]
mode = "microphone"
```
Дозвіл Microphone обов'язковий.

## 5. Збірка

```bash
xcode-select --install
brew install cmake
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build -p silent-ai-app --release

# Apple Silicon / Intel GPU-прискорення:
cargo build -p silent-ai-app --release --features metal
# або
cargo build -p silent-ai-app --release --features coreml
```

Мінімальна версія в `tauri.conf.json` — macOS 13.0. Для розповсюдження поза локальною машиною потрібні code signing і notarization (див. документацію Tauri); без них Gatekeeper попередить користувача.

## 6. Приховування під час демонстрації екрану

Silent AI встановлює для вікна:

```text
NSWindow.sharingType = NSWindowSharingNone
```

Вікно видно вам, але воно не потрапляє у screen sharing/recording інших застосунків. Це не потребує додаткових дозволів. Через особливості macOS таке вікно також не бере участі в деяких системних сервісах (наприклад, деяких screenshot-інструментах) — це очікувано.

Ручний fallback: `Cmd+Shift+H`. Вимкнути: `[stealth] hide_from_capture = false`.

## 7. Приватність

Core Audio tap / BlackHole записують системний звук, включно з іншими застосунками. Аудіо обробляється в пам'яті; транскрипт іде в LLM лише для розпізнаного запитання. Ключ API зберігається лише в env.
