# Архітектура та бюджет затримки

## Потік даних

```
┌──────────────────── parec reader / cpal callback ─────────────────────┐
│ PulseAudio monitor / WASAPI loopback / Core Audio tap                 │
│ єдина робота: Producer::push_slice(&[f32]) у ringbuf                 │
└──────────────────────────────────┬────────────────────────────────────┘
                                   │ lock-free ringbuf (2 s)
┌──────────────────────────────────▼────────────────────────────────────┐
│ audio processing thread (std::thread)                                 │
│ downmix → rubato FFT resampler → 16 kHz mono → 320-sample frames      │
└──────────────────────────────────┬────────────────────────────────────┘
                                   │ tokio mpsc<AudioFrame>
┌──────────────────────────────────▼────────────────────────────────────┐
│ VAD thread (std::thread; webrtc-vad не Send)                          │
│ 20 ms frames, pre-roll 400 ms, min speech 100 ms, silence 600 ms      │
│ energy fallback -38 dBFS для чистого системного звуку                 │
└───────────────┬──────────────────────────────┬────────────────────────┘
                │ Partial(audio)               │ Endpoint(audio, speech_end)
                ▼                              ▼
┌──────────────────────────────┐   ┌───────────────────────────────────┐
│ Whisper worker (std::thread) │   │ main async pipeline               │
│ greedy best_of=1             │   │ cancel old speculation, submit    │
│ no_context, single_segment   │   │ final job, compute metrics        │
│ adaptive audio_ctx           │   └───────────────┬───────────────────┘
└───────────────┬──────────────┘                   │
                │ SttEvent::Result                 │ final text
                ▼                                  ▼
┌───────────────────────────────────────────────────────────────────────┐
│ QuestionDetector (zero network)  ── partial question ──► LLM request  │
│ LocalAgreement (two hypotheses)  ── final question   ──► accept/retry │
└──────────────────────────────────┬────────────────────────────────────┘
                                   │ SSE deltas (buffered until final)
                                   ▼
┌───────────────────────────────────────────────────────────────────────┐
│ Tauri events: status / question / answer_start / answer_delta / done  │
│ UI paints, computes IPC+render ms, reports back for logging           │
└───────────────────────────────────────────────────────────────────────┘
```

## Чому це швидко

1. **Callback не рахує.** Жодного виділення пам'яті, ресемплингу чи VAD у realtime-потоці.
2. **Один inference на фінал.** Partial-и не конкурують із фіналом: `submit_final` виставляє `preempt`, і поточний partial скасовується через whisper.cpp abort callback.
3. **Adaptive audio context.** Whisper.cpp типово кодує 1500 mel-фреймів (30 s). Для 2–8-секундного запитання достатньо 256–450; це в кілька разів зменшує encoder time.
4. **Greedy + без timestamps + без auto-detect.** Мінімум декодера.
5. **Спекуляція.** LLM стартує на partial-гіпотезі. Токени не показуються, поки не прийде фінал; якщо фінал сумісний — буфер виливається одразу. Це прибирає з критичного шляху значну частину LLM TTFT.
6. **Один HTTP-клієнт.** HTTP/2, keep-alive, pool; warmup `GET /models` при старті.
7. **Статичний system prompt.** Короткий і незмінний, стоїть першим — провайдери з prefix caching можуть його перевикористовувати. Динаміка (останні 60 с + запитання) — в кінці.

## Вимірювання

`Latency` містить:

| Поле | Від → до |
|---|---|
| `vad_ms` | оцінений кінець останнього voiced-фрейму → рішення VAD про endpoint |
| `stt_ms` | постановка фінального job → готовий текст |
| `llm_ttft_ms` | HTTP-запит → перший видимий токен (сирий TTFT, навіть якщо токен був у буфері) |
| `backend_total_ms` | кінець мовлення → перший видимий токен у бекенді |
| `ui_ms` | перший токен → фактичний paint у webview (JS `requestAnimationFrame`) |
| `total_ms` | кінець мовлення → paint |

`AnswerStart` містить epoch-мітки `speech_end_epoch_ms` і `first_token_epoch_ms`, щоб JS міг порахувати IPC + render без спільного монотонного годинника.

## Стани

- `Loading` — модель/STT ініціалізується.
- `Listening` — аудіо обробляється, запитання ще немає.
- `Thinking` — фінал підтверджено, чекаємо перший токен.
- `Answering` — токени йдуть у UI.
- `Paused` — кадри скидаються, активні запити скасовано.
- `Error` / `Stopped`.

## Скасування та завершення

- Кореневий `CancellationToken` живить усі дочірні токени.
- Нове запитання скасовує попередній LLM-запит; частковий STT скасовується через `preempt`.
- `AudioGuard::shutdown` зупиняє `parec`/cpal, прибирає ringbuf і приєднується до потоків.
- Whisper worker завершується, коли закривається канал job-ів; модель і state звільняються.
- Tauri на `ExitRequested` блокує вихід, надсилає `Shutdown`, чекає до 10 с на `Notify` від pipeline і лише потім викликає `app.exit(0)`.

## Свідомі компроміси

- **WebRTC VAD + energy fallback** замість Silero/ONNX: нульове завантаження моделі й дуже мала затримка. Для галасливих кімнат можна підняти `webrtc_mode` до `aggressive` і знизити fallback.
- **Partial-и Whisper** перетранскрибують поточне мовлення (до 6 с), а не незалежні 1-секундні вікна. Це стабільніше для local agreement і краще для коротких запитань; фінал завжди використовує повний сегмент.
- **Спекуляція** може іноді відповісти на трохи іншу гіпотезу запитання. Фінальний текст замінює запитання в UI; якщо різниця суттєва, запит скасовується. Системний промпт `NO_QUESTION` відсіює хибні спроби.
- **Системний звук** може містити повідомлення з інших застосунків; це особливість loopback, не баг. Аудіо не пишеться на диск.
