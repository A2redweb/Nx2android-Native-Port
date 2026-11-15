#!/bin/sh
# Copies upstream's game profiles (game.tsv and android.tsv of every game) into the app's assets. Usage: tools/fetch_profiles.sh [upstream checkout]
# With no argument it makes a shallow clone of shirai91/nx2android in a temporary folder.
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
src=${1:-}
if [ -z "$src" ]; then
  tmp=$(mktemp -d)
  git clone --depth 1 https://github.com/shirai91/nx2android "$tmp/up"
  src=$tmp/up
fi
dst=$here/android/app/src/main/assets/games
rm -rf "$dst"
mkdir -p "$dst"
for d in "$src"/nxnative/dm/games/*/; do
  [ -f "${d}game.tsv" ] || continue
  id=$(basename "$d")
  mkdir -p "$dst/$id"
  for f in game.tsv android.tsv; do
    [ -f "$d$f" ] && cp "$d$f" "$dst/$id/$f"
  done
done
python3 "$here/tools/gen_cuts.py" "$src" "$dst"
echo "profiles: $(ls "$dst" | wc -l) games copied to $dst"
