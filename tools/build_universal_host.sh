#!/bin/sh
# Builds the universal libdmhost.so: tools/gen_universal_host.py makes a patched copy of upstream's host with every game's profile in it, upstream's own build.py compiles it.
# Usage: tools/build_universal_host.sh <upstream checkout> <work dir> [NDK dir]   (the NDK: $ANDROID_NDK_HOME by default). Result: <work dir>/out/libdmhost.so
set -e
here=$(cd "$(dirname "$0")" && pwd)
up=$(cd "$1" && pwd); mkdir -p "$2"; work=$(cd "$2" && pwd)
ndk=${3:-$ANDROID_NDK_HOME}
python3 "$here/gen_universal_host.py" "$up" "$work" "$work/gen"
UW_GEN="$work/gen" python3 "$work/dm/android/build.py" --game universal --ndk "$ndk" --out "$work/out" --lib --release --adrenotools
ls -l "$work/out/libdmhost.so"
