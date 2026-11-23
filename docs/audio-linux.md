# Linux: захоплення системного звуку

Silent AI на Linux захоплює monitor-джерело поточного default sink. Для довгих сесій застосунок запускає `parec` (PulseAudio-протокол; на PipeWire працює через `pipewire-pulse`). Це звичайний recording client, тому джерело не «засинає» після першої відповіді. Якщо `parec` недоступний, використовується cpal 0.18 із **PulseAudio-хостом** (чистий Rust).

`parec` входить до пакета `pulseaudio-utils` (Fedora/Debian/Ubuntu).

Опційно можна зібрати з нативним PipeWire-хостом (`--features native-pipewire`), який дає пристрій `default_sink`. Для більшості користувачів це не потрібно: типова збірка працює через `pipewire-pulse`.

## 1. Перевірте сервер

```bash
pactl info
pactl get-default-sink
pactl list short sources | grep -i monitor
```

Очікувано:

- `Server Name: PulseAudio (on PipeWire ...)` або `PipeWire`;
- default sink — пристрій, у який грає Google Meet;
- monitor source на кшталт
  `alsa_output.pci-0000_00_1f.3.analog-stereo.monitor`.

Якщо monitor-джерел немає:

```bash
systemctl --user enable --now pipewire pipewire-pulse wireplumber
```

## 2. Виберіть пристрій

```bash
silent-ai --list-devices
```

Типовий вивід (скорочено):

```text
pulseaudio:...analog-stereo                 pulseaudio  output  Speakers
pulseaudio:...analog-stereo.monitor         pulseaudio  input   Monitor of Speakers
pulseaudio:...hdmi-stereo.monitor           pulseaudio  input   Monitor of HDMI
```

| Хост | Що використовувати |
|---|---|
| `pulseaudio` | **`Monitor of <default sink>`** — рекомендовано для system audio |
| `pipewire` | `default_sink` — лише якщо зібрано з `native-pipewire` |
| `alsa` | зазвичай мікрофон або `pulse`-плагін; не для loopback |

Конфіг:

```toml
[audio]
mode = "system"
device = "auto"                    # monitor поточного default sink
# або точний ID з --list-devices:
# device = "pulseaudio:alsa_output.pci-0000_00_1f.3.analog-stereo.monitor"
# або частина назви:
# device = "Monitor of"
```

`device = "auto"` на Linux:

1. якщо є нативний PipeWire-хост — бере `default_sink`;
2. інакше знаходить default output (sink) у PulseAudio та вибирає **його** monitor;
3. якщо відповідність не знайдена — перший monitor source;
4. лише потім звичайний пристрій.

Крок 2 важливий: на машині може бути кілька monitor-джерел (HDMI, аналоговий вихід). Якщо взяти перший-ліпший, можна слухати тишу, поки Meet грає в інший sink.

## 3. Нативний PipeWire (опційно)

```bash
# Fedora
sudo dnf install pipewire-devel clang clang-devel libclang
# Debian/Ubuntu
sudo apt install libpipewire-0.3-dev clang libclang-dev

cargo build -p silent-ai-app --release --features native-pipewire
```

У `--list-devices` з'явиться хост `pipewire` і пристрій `default_sink`:

```toml
[audio]
device = "auto"          # спочатку спробує native pipewire default_sink
# або
# device = "pipewire:sink_default"
```

`libspa-sys`/`pipewire-sys` компілюються bindgen-ом і потребують `libclang` та стандартних C-заголовків (`glibc-headers`, GCC include). Якщо bindgen не бачить `stdbool.h`, встановіть `clang` + `glibc-devel` або задайте:

```bash
export BINDGEN_EXTRA_CLANG_ARGS="-I/usr/lib/gcc/$(gcc -dumpmachine)/$(gcc -dumpversion | cut -d. -f1)/include"
```

Це стосується **лише** опційного нативного PipeWire; типова збірка `libspa-sys` не компілює. Не використовуйте `--all-features`, доки ці залежності не встановлені.

## 4. Перевірте захоплення

```bash
silent-ai --headless --log-level info
# в іншому терміналі:
paplay bench/fixtures/sample_question.wav
```

У логах буде приблизно:

```
audio: pulseaudio — Monitor of Ryzen HD Audio Controller Analog Stereo (48000 Hz, 2 ch)
[final ...] Why is the sky blue?
```

Еквівалент без застосунку:

```bash
pactl list short sources
parec --device="$(pactl get-default-sink).monitor" --format=float32le --rate=48000 --channels=2 | head -c 100000 >/dev/null
```

## 5. Типові проблеми

### Немає monitor-джерела

- Перевірте `pipewire-pulse` (не лише `pipewire`).
- Перезапустіть WirePlumber: `systemctl --user restart wireplumber`.
- У `--list-devices` подивіться, чи є хост `pulseaudio`. Якщо є лише `alsa`, cpal зібрано без `pulseaudio` (перевірте feature у `core/Cargo.toml`).

### Застосунок бачить лише ALSA

`alsa` не є loopback-бекендом. Для system audio потрібен PulseAudio/PipeWire хост. Перевірте:

```bash
cargo tree -p silent-ai-core | grep cpal
```

У збірці cpal має бути feature `pulseaudio` (типово) або `pipewire` (native).

### Звук із Zoom/Discord, але не з Meet

Meet може грати в інший sink (HDMI). `pactl get-default-sink` показує типовий, але браузер міг вибрати інший. Перемкніть Meet на потрібний вихід; `device = "auto"` підхопить monitor нового default sink після перезапуску застосунку.

### Bluetooth-навушники

Monitor source з'являється, але після перемикання профілю A2DP/HFP пристрій може перестворюватися. Перезапустіть застосунок або виберіть новий ID.

### Wayland/X11

Захоплення звуку не залежить від графічного сервера. Tauri-вікно на Wayland потребує звичайних GTK/WebKit залежностей; для прозорості — композитор із підтримкою alpha. Глобальні гарячі клавіші на Wayland можуть бути обмежені; кнопки вікна працюють завжди.

### Мікрофон замість системного звуку

```toml
[audio]
mode = "microphone"
device = "auto"
```

Це корисно, якщо викладач фізично в кімнаті. Для Google Meet майже завжди потрібен `mode = "system"`.

## 6. Демонстрація екрана і always-on-top

### Чому вікно не можна «сховати лише з демонстрації»

Linux-композитори (зокрема GNOME/Mutter) не мають API, який дозволяє вікну
бути видимим на екрані, але відсутнім у screencast. Windows
(`WDA_EXCLUDEFROMCAPTURE`) і macOS (`NSWindowSharingType.None`) такий API
мають, Linux — ні. Тому під час **full-screen** демонстрації видиме вікно
неминуче потрапляє у потік.

Практичні варіанти:

1. У Google Meet ділитися **вкладкою Chrome** або **вікном Chrome**, а не всім
   екраном. Асистент залишається видимим локально і не потрапляє у демонстрацію.
2. Демонструвати один монітор, а вікно асистента тримати на іншому
   (вікно перетягується за шапку).
3. Увімкнути авто-приховування — тоді вікно зникає і у вас, і з демонстрації:

   ```toml
   [stealth]
   auto_hide_on_screen_share = true
   ```

### Детекція демонстрації

Типово (`auto_hide_on_screen_share = false`) вікно **не ховається**, але
застосунок визначає початок демонстрації та показує підказку в UI. Два джерела:

1. **XDG Desktop Portal `ScreenCast`** — фоновий `dbus-monitor` слухає `Start`
   на `org.freedesktop.portal.ScreenCast` і `Close` на
   `org.freedesktop.portal.Session`. Chrome/Firefox/Meet на Wayland працюють
   через портал, тому реакція за мілісекунди.
2. **`pw-dump`** — запасний варіант раз на `screen_share_poll_ms` (типово 1.5 с).
   Шукає `Stream/Input/Video` з `media.role = "Screen"` / `"Presentation"` або
   назвою screencast; вебкамери (`media.role = "Camera"`) ігноруються. Ловить
   демонстрацію, яка вже тривала до запуску застосунку.

Потрібні:

```bash
# Fedora
sudo dnf install pipewire-utils dbus-tools
# Debian/Ubuntu
sudo apt install pipewire-bin dbus
```

Якщо браузер/композитор не використовує ні портал, ні PipeWire-стрім, підказка
не з'явиться. Ручне керування — `Ctrl+Shift+H` або кнопка `◌`.

Налаштування:

```toml
[stealth]
hide_from_capture = true
auto_hide_on_screen_share = false   # true = ховати вікно під час демонстрації
notify_on_screen_share = true       # підказка в UI
screen_share_poll_ms = 1500
```

### Always-on-top на Wayland

GNOME/KDE Wayland ігнорують keep-above для нативних Wayland-вікон. Тому типово
Silent AI запускає GTK-вікно через XWayland (`GDK_BACKEND=x11`), де працює
`_NET_WM_STATE_ABOVE`. Це вмикається `ui.force_x11_on_wayland = true` і не
впливає на захоплення аудіо. Для нативного Wayland поставте `false` — але тоді
вікно не гарантовано буде поверх інших.

## 7. Залежності збірки

Типова збірка (PulseAudio compatibility):

Fedora:

```bash
sudo dnf install alsa-lib-devel cmake gcc-c++ pkgconf-pkg-config
```

Debian/Ubuntu:

```bash
sudo apt install libasound2-dev cmake build-essential pkg-config
```

Для Tauri-вікна додайте WebKit/GTK-залежності з README.

`alsa-sys` потрібен завжди: cpal компілює ALSA-хост навіть якщо фактично використовується PulseAudio. `libspa-sys`/`pipewire-sys` потрібні лише для `native-pipewire`.
