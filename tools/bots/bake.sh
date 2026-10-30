#!/usr/bin/env bash
set -euo pipefail
GAME="${FN30_GAME:-D:/Fortnite/30.0 Forntie/30.00-CL-33962396}"
USMAP="${FN30_USMAP:-D:/Fortnite/FortniteRef30/usmap/fn30.usmap}"
EXE="${FN30_EXE:-D:/Fortnite/FortniteRef30/bin/FortniteClient-Win64-Shipping.exe}"
KEYFILE="${FN30_KEY:-D:/Fortnite/FortniteRef30/keys/main.key}"
OUT="${1:-D:/Fortnite/FortniteRust/FortniteRuntime/assets/bots}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
UEX="${UEX:-$HOME/.claude/skills/extracting-unreal-assets/uex}"
FN_PAK_AES_KEY="$(tr -d '\r\n' < "$KEYFILE")" "$UEX" export "$GAME" --filter "Athena/AI/Phoebe/|AISettings/PlayerBotsFlow|BRCosmetics/Content/Athena/AI/Phoebe" --ext uasset --max 0 --out "$(cygpath -w "$WORK" 2>/dev/null || echo "$WORK")" --ue 5.3 --usmap "$USMAP" --aes-env FN_PAK_AES_KEY > /dev/null
cd "$(dirname "$0")/../.."
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-D:/target-fn-bots}" cargo run -q -p fn-bots --bin phoebe-bake -- --src "$WORK" --exe "$EXE" --out "$OUT"
