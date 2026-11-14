#!/usr/bin/env bash
# Builds the porter's two .NET tools (nx_extract, dmsh_xlate) as NativeAOT for Android itself (linux-bionic-arm64), so they run in the app with no proot.
# The old app's prebuilt tools are linux-arm64 glibc builds and cannot run natively on Android.
# UNPROVEN: the compiler and linker settings below are the first attempt; the CI run decides whether they work.
# usage: make_bionic_tools.sh <upstream checkout> <output dir>
set -euo pipefail
SRC="$1"; OUT="$2"
NDK="${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"
API="${NX2A_API:-26}"
TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64"
mkdir -p "$OUT"
for t in nx_extract:nxnative/dm/tools/nx_extract dmsh_xlate:nxnative/dm/gfx/tools/dmsh_xlate; do
  name="${t%%:*}"; proj="${t#*:}"
  dotnet publish "$SRC/$proj" -c Release -r linux-bionic-arm64 -o "$OUT/$name" -nologo -v quiet \
    --self-contained true -p:PublishAot=true -p:InvariantGlobalization=true \
    -p:CppCompilerAndLinker="$TC/bin/aarch64-linux-android${API}-clang" \
    -p:SysRoot="$TC/sysroot" -p:ObjCopyName="$TC/bin/llvm-objcopy"
done
# a bionic build names Android's own loader, not glibc's
for b in "$OUT/nx_extract/nx_extract" "$OUT/dmsh_xlate/dmsh_xlate"; do
  file "$b"
  file "$b" | grep -q '/system/bin/linker64' || { echo "not a bionic executable: $b" >&2; exit 1; }
done
