#!/usr/bin/env bash
# Builds the porter's shader translator (dmsh_xlate; extraction is Rust here, nx_extract is not needed) as NativeAOT for Android itself (linux-bionic-arm64), so they run in the app with no proot.
# The old app's prebuilt tools are linux-arm64 glibc builds and cannot run natively on Android.
# UNPROVEN: the compiler and linker settings below are the first attempt; the CI run decides whether they work.
# usage: make_bionic_tools.sh <upstream checkout> <output dir>
set -euo pipefail
SRC="$1"; OUT="$2"
NDK="${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"
API="${NX2A_API:-26}"
TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64"
mkdir -p "$OUT"
for t in dmsh_xlate:nxnative/dm/gfx/tools/dmsh_xlate; do
  name="${t%%:*}"; proj="${t#*:}"
  # The SDK has no NativeAOT runtime pack of its own for linux-bionic-arm64: it restores the glibc one (linux-arm64), whose sdk/ assemblies the compiler then misses ("The PrivateSdkAssemblies
  # ItemGroup is required"). NuGet has the bionic pack under its own ID, so it is fetched at the compiler's version and named by hand.
  # no System.Security.Cryptography on Android without a JVM (it needs libssl): the two SHA-256 calls use a managed one
  cp "$(dirname "$0")/dotnet/ManagedSha256.cs" "$SRC/$proj/ManagedSha256.cs"
  sed -i 's/System\.Security\.Cryptography\.SHA256\.HashData(/ManagedSha256.HashData(/g' "$SRC/$proj/Program.cs"
  ! grep -q 'System.Security.Cryptography' "$SRC/$proj/Program.cs" || { echo "Program.cs still uses System.Security.Cryptography" >&2; exit 1; }
  dotnet restore "$SRC/$proj" -r linux-bionic-arm64 -p:PublishAot=true -nologo -v quiet
  VER="$(ls "$HOME/.nuget/packages/microsoft.dotnet.ilcompiler" | sort -V | tail -1)"
  PACK_ID=microsoft.netcore.app.runtime.nativeaot.linux-bionic-arm64
  PACK="$OUT/packs/$PACK_ID"
  if [ ! -d "$PACK" ]; then
    mkdir -p "$PACK"
    curl -fsSL "https://api.nuget.org/v3-flatcontainer/$PACK_ID/$VER/$PACK_ID.$VER.nupkg" -o "$PACK/pack.zip"
    (cd "$PACK" && unzip -q pack.zip && rm pack.zip)
  fi
  ls "$PACK/runtimes/linux-bionic-arm64"
  dotnet publish "$SRC/$proj" -c Release -r linux-bionic-arm64 -o "$OUT/$name" -nologo -v minimal \
    --self-contained true -p:PublishAot=true -p:InvariantGlobalization=true \
    -p:CppCompilerAndLinker="$TC/bin/aarch64-linux-android${API}-clang" \
    -p:IlcSdkPath="$PACK/runtimes/linux-bionic-arm64/native/" -p:IlcFrameworkNativePath="$PACK/runtimes/linux-bionic-arm64/native/" -p:IlcFrameworkPath="$PACK/runtimes/linux-bionic-arm64/lib/net10.0/" \
    -p:SysRoot="$TC/sysroot" -p:ObjCopyName="$TC/bin/llvm-objcopy"
done
# a bionic build names Android's own loader, not glibc's
for b in "$OUT/dmsh_xlate/dmsh_xlate"; do
  file "$b"
  file "$b" | grep -q '/system/bin/linker64' || { echo "not a bionic executable: $b" >&2; exit 1; }
done
