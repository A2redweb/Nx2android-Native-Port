#!/bin/sh
# Builds the app: the Rust core (cargo-ndk), upstream's game profiles and Java wrapper, the universal host (NDK r27c, HOST_NDK), then Gradle.
# Usage: tools/build_android.sh [upstream checkout]   (no argument: a shallow clone of shirai91/nx2android). Needs the Android SDK, the NDK (ANDROID_NDK_HOME) for the core,
# HOST_NDK (r27c, has glslc) for the host, rustup target aarch64-linux-android, cargo-ndk, python3.
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here"
src=${1:-}
if [ -z "$src" ]; then
  tmp=$(mktemp -d); git clone --depth 1 https://github.com/shirai91/nx2android "$tmp/up"; src=$tmp/up
fi
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release -p nx-jni
sh tools/fetch_profiles.sh "$src"
# upstream's Java classes (the NativeActivity subclass with text entry and its helpers), compiled with the app
rm -rf android/app/src/upstream-java && mkdir -p android/app/src/upstream-java/org/nx2a
cp "$src"/nxnative/dm/android/java/org/nx2a/*.java android/app/src/upstream-java/org/nx2a/
# the universal host
ANDROID_NDK_HOME=${HOST_NDK:?set HOST_NDK to NDK r27c} sh tools/build_universal_host.sh "$src" "${TMPDIR:-/tmp}/uw"
for l in libdmhost libadrenotools libhook_impl libmain_hook libfile_redirect_hook libgsl_alloc_hook; do cp "${TMPDIR:-/tmp}/uw/out/$l.so" android/app/src/main/jniLibs/arm64-v8a/; done
# the button-icon font (stand-in for the console's Nintendo extension font; game-independent), drawn once here as upstream's porter does, so the app needs no Python
mkdir -p android/app/src/main/assets
rm -f android/app/src/main/assets/nintendo_ext.ttf
LETTERS=""
for f in /usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf /usr/share/fonts/TTF/DejaVuSans-Bold.ttf /usr/share/fonts/liberation/LiberationSans-Bold.ttf /usr/share/fonts/noto/NotoSans-Bold.ttf; do [ -f "$f" ] && LETTERS=$f && break; done
if [ -n "$LETTERS" ] && python3 -c "import fontTools" 2>/dev/null; then
  python3 "$src/nxnative/dm/tools/make_ext_font.py" android/app/src/main/assets/nintendo_ext.ttf "$LETTERS" || { echo "WARNING: nintendo_ext.ttf not made; button icons draw as ?" >&2; rm -f android/app/src/main/assets/nintendo_ext.ttf; }
else
  echo "WARNING: no fontTools or no bold font: the button-icon font is left out (icons draw as ?)" >&2
fi
# the shader translator, NativeAOT for bionic (UNPROVEN, see docs/SPIKE_DOTNET.md): shipped as an executable named like a library so Android extracts it to nativeLibraryDir, where the app may run it.
# A build without dotnet or one that fails leaves it out; the app then says so instead of building shaders.
if command -v dotnet >/dev/null 2>&1; then
  if ANDROID_NDK_HOME=${ANDROID_NDK_HOME:?} bash tools/make_bionic_tools.sh "$src" "${TMPDIR:-/tmp}/bt"; then
    cp "${TMPDIR:-/tmp}/bt/dmsh_xlate/dmsh_xlate" android/app/src/main/jniLibs/arm64-v8a/libdmsh_xlate.so
  else
    echo "WARNING: dmsh_xlate for bionic did not build; this APK cannot build shaders" >&2
  fi
fi
cd android && gradle --no-daemon assembleRelease
echo "APK: $here/android/app/build/outputs/apk/release/app-release.apk"
