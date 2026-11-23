# Windows 10/11: WASAPI loopback

Silent AI використовує cpal 0.18 і WASAPI. Коли ви відкриваєте **output-пристрій як input**, cpal прозоро вмикає `AUDCLNT_STREAMFLAGS_LOOPBACK`. Окремий «Stereo Mix» не потрібен.

## 1. Виберіть playback-пристрій

```powershell
.\target\release\silent-ai.exe --list-devices
```

У списку `output`-пристрої мають підказку `WASAPI loopback`. Це може бути:

- `Speakers (Realtek ...)`
- `Headphones (...)`
- HDMI/DisplayPort монітор
- USB DAC

Саме в цей пристрій має грати Google Meet. Якщо Meet виводить звук у навушники, а системний вихід — колонки, обирайте навушники.

```toml
[audio]
mode = "system"
device = "auto"                    # default output
# або точна назва/ID:
# device = "Speakers (Realtek(R) Audio)"
# device = "wasapi:{...}"
```

## 2. Формат і частота

WASAPI shared mode зазвичай дає 48 kHz / 2 ch / f32. Silent AI просить F32, якщо пристрій його підтримує; інакше конвертує I16/I32/U16 у callback. Ресемплер приводить все до 16 kHz mono.

Якщо потік не відкривається:

1. **Sound settings → Device properties → Advanced**:
   - вимкніть *Exclusive mode* (Allow applications to take exclusive control);
   - переконайтеся, що sample rate не екзотичний (наприклад, 192 kHz); поставте 48 kHz.
2. Оновіть аудіодрайвер (Realtek/NVIDIA HDMI/USB).
3. Перезапустіть застосунок після зміни default output.
4. Перевірте, що обраний пристрій не «Disabled».

## 3. Перевірка

```powershell
.\target\release\silent-ai.exe --headless
# в іншому вікні/на телефоні увімкніть запитання в Meet
```

У консолі:

```
[audio] wasapi / Speakers ... (48000 Hz, 2 ch, loopback=true)
[stt ...] Why is the sky blue?
```

## 4. Типові проблеми

**«No playback device found for WASAPI loopback»**
- Немає активного output-пристрою або Windows Audio service зупинений:
  ```powershell
  Get-Service Audiosrv
  ```
- Перевірте `--list-devices`; якщо порожньо — проблема з драйвером/службою.

**Loopback стартує, але тиша**
- Meet грає в інший пристрій. Тимчасово зробіть потрібний пристрій default у Sound settings.
- Деякі Bluetooth-пристрої в hands-free режимі не дають loopback; використовуйте A2DP/інший вихід.
- HDMI без активного монітора може не мати потоку; виберіть колонки.

**Переривчастий звук**
- Збільште `audio.ring_buffer_ms` до 3000–4000.
- Закрийте додатки, що монопольно тримають пристрій (ASIO/Dolby Access).
- Для ноутбуків вимкніть аудіо-ефекти простору.

**Мікрофон**
```toml
[audio]
mode = "microphone"
```
WASAPI input відкривається як звичайний мікрофон. Loopback у цьому режимі не використовується.

## 5. Збірка

Потрібні:

- Rust stable MSVC (`rustup default stable-msvc`);
- Visual Studio Build Tools з workload «Desktop development with C++»;
- CMake (для whisper.cpp);
- WebView2 Runtime (Windows 11 — вбудований; Windows 10 — Evergreen).

```powershell
cargo build -p silent-ai-app --release
cargo build -p silent-ai-app --release --features cuda   # опційно, з CUDA Toolkit
```

Артефакти Tauri: `.msi` (WiX) та `.exe` (NSIS).

## 6. Приховування під час демонстрації екрану

Silent AI типово викликає:

```text
SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
```

Вікно залишається видимим для вас, але не потрапляє у screen capture (Google Meet, OBS, Snipping Tool тощо). Потрібна Windows 10 версії 2004+ або Windows 11. На старіших збірках API може поводитися як `WDA_MONITOR` (чорний прямокутник у записі) або не спрацювати; тоді використовуйте `Ctrl+Shift+H`.

Вимкнути: `[stealth] hide_from_capture = false` у `config.toml`.

## 7. Приватність

Loopback записує весь системний звук, зокрема повідомлення інших застосунків. Аудіо не зберігається; у хмару (LLM/Deepgram) іде лише транскрипт або аудіо, і тільки коли це налаштовано. Ключ API читається зі змінної середовища або файлу `llm.api_key_file`.
