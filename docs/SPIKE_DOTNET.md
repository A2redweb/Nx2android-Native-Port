# Spike 1: .NET tools without proot

## Finding: the old prebuilt tools cannot be reused as they are

The `prebuilt` release of `A2redweb/nx2android-android-port` (checked: `nx-tools-b040e4a818c754b6e3cc`, upstream `d4bccf0`) holds `nx_extract` and `dmsh_xlate` as NativeAOT, linux-arm64. `file` on both shows an interpreter of `/lib/ld-linux-aarch64.so.1`, which is glibc. They ran in the old app only because proot provides Ubuntu. Android has no glibc loader, so they cannot start in an app process.

## What is reused

- The key function `tools/prebuilt_key.py` (copied unchanged), so tool builds are named by the tool sources and match the porter on the phone.
- The release layout idea: one archive per sources key, a `latest.json`, a start-up smoke test before use.
- The proven flags `PublishAot=true`, `InvariantGlobalization=true`, self-contained.

## What changes

- Runtime identifier `linux-bionic-arm64`, linked with the Android NDK's clang and sysroot (`tools/make_bionic_tools.sh`).
- Built on an x86-64 runner, because the NDK's tools are x86-64.
- The build script checks that the result names `/system/bin/linker64`.

## Status

Not run yet. No .NET SDK in the authoring environment, so the compiler and linker properties are unverified. Next: run the `bionic-tools` workflow, then start both tools from a test app on the Poco X6 (`dmsh_xlate --source-sha` must print 16 hex digits; `nx_extract` must print usage).

If NativeAOT for bionic does not link, fall back to: (a) a Mono or CoreCLR bionic runtime shipped in the app, or (b) replacing the tools.
