#!/bin/sh
# Builds the universal libdmhost.so: tools/gen_universal_host.py makes a patched copy of upstream's host with every game's profile in it, upstream's own build.py compiles it.
# Usage: tools/build_universal_host.sh <upstream checkout> <work dir> [NDK dir]   (the NDK: $ANDROID_NDK_HOME by default). Result: <work dir>/out/libdmhost.so and libdmhost_exe.so
set -e
here=$(cd "$(dirname "$0")" && pwd)
up=$(cd "$1" && pwd); mkdir -p "$2"; work=$(cd "$2" && pwd)
ndk=${3:-$ANDROID_NDK_HOME}
python3 "$here/gen_universal_host.py" "$up" "$work" "$work/gen"
UW_GEN="$work/gen" python3 "$work/dm/android/build.py" --game universal --ndk "$ndk" --out "$work/out" --exe --lib --release --adrenotools
# the host as an executable too: the port runs the game's own Oodle decoder with it (DM_OODLE_SERVE), shipped as libdmhost_exe.so so the app may run it from its library folder
cp "$work/out/dmhost" "$work/out/libdmhost_exe.so"
"$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip" --strip-unneeded "$work/out/libdmhost_exe.so"
ls -l "$work/out/libdmhost.so" "$work/out/libdmhost_exe.so"
