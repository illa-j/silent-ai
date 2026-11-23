#!/usr/bin/env bash
# Install the KWin script that excludes Silent AI from Plasma ScreenCast.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/packaging/kwin/silent-ai-stealth"
DEST="$HOME/.local/share/kwin/scripts/silent-ai-stealth"

mkdir -p "$DEST"
cp -r "$SRC/." "$DEST/"
kwriteconfig6 --file kwinrc --group Plugins --key silent-ai-stealthEnabled true
busctl --user call org.kde.KWin /KWin org.kde.KWin reconfigure >/dev/null 2>&1 || true
echo "Installed KWin stealth script. Check:"
echo "  journalctl --user -n 50 | grep -i 'Silent AI: excludeFromCapture'"
