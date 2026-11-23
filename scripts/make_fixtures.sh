#!/usr/bin/env bash
# Regenerate the committed WAV fixtures. Requires espeak-ng and ffmpeg.
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p bench/fixtures
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

espeak-ng -v en-us -s 145 -w "$tmp/en.wav" "Why is the sky blue?"
espeak-ng -v uk -s 140 -w "$tmp/uk.wav" "Чому небо синє?"
espeak-ng -v uk -s 140 -w "$tmp/notq.wav" "Всі мене чують?"

ffmpeg -y -hide_banner -loglevel error -i "$tmp/en.wav" \
  -af "adelay=500|500,apad=pad_dur=1.0" -ar 16000 -ac 1 -c:a pcm_s16le \
  bench/fixtures/sample_question.wav
ffmpeg -y -hide_banner -loglevel error -i "$tmp/uk.wav" \
  -af "adelay=500|500,apad=pad_dur=1.0" -ar 16000 -ac 1 -c:a pcm_s16le \
  bench/fixtures/sample_question_uk.wav
ffmpeg -y -hide_banner -loglevel error -i "$tmp/notq.wav" \
  -af "adelay=500|500,apad=pad_dur=1.0" -ar 16000 -ac 1 -c:a pcm_s16le \
  bench/fixtures/not_question_uk.wav

ls -lh bench/fixtures/*.wav
