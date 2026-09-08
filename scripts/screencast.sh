#!/usr/bin/env bash
# Record a short tour of manaline with asciinema (https://asciinema.org).
#
#   scripts/screencast.sh            # records to screencast.cast in the repo root
#   scripts/screencast.sh out.cast   # records to the given file
#
# The tour: a card search, a deck check with stats, a stepped replay of a
# random-bot game, and finally the deckbuilder for you to drive (press q when
# done). Stop the recording with Ctrl-D once the deckbuilder exits.
set -euo pipefail
cd "$(dirname "$0")/.."
out="${1:-screencast.cast}"
command -v asciinema >/dev/null || { echo "install asciinema first (brew install asciinema)"; exit 1; }
cargo build --release --quiet
export PATH="$PWD/target/release:$PATH"
tour=$(mktemp -t manaline-tour.XXXXXX)
cat > "$tour" <<'TOUR'
set -e
pause() { sleep "${1:-2}"; }
say() { printf '\n\033[1m$ %s\033[0m\n' "$*"; sleep 1; "$@"; }
say manaline cards search 't:creature c:g mv<=2 kw:deathtouch'
pause
say manaline cards show Elvish Archdruid
pause 3
say manaline deck check decks/rg-stompy.txt
pause
say manaline deck stats decks/rg-stompy.txt --hands 2
pause 4
say manaline sim --deck rg-stompy --deck wu-fliers --games 1 --seed 42
replay=$(ls -t ~/.local/share/manaline/games/*.jsonl 2>/dev/null | head -1 || true)
if [ -n "$replay" ]; then
  printf '\n\033[1m$ manaline replay %s --step\033[0m   (→ steps, Space plays, q quits)\n' "$replay"; sleep 2
  manaline replay "$replay" --step
fi
cp decks/rg-stompy.txt /tmp/tour-deck.txt
printf '\n\033[1m$ manaline deck edit /tmp/tour-deck.txt\033[0m   (type to search, Enter adds, Tab, s saves, q quits)\n'; sleep 2
manaline deck edit /tmp/tour-deck.txt
TOUR
asciinema rec --title "manaline" --command "bash $tour" "$out"
echo "recorded $out — upload with: asciinema upload $out"
