#!/bin/sh
# Builds the app: the Rust core for arm64 with cargo-ndk, the profiles, then Gradle. Needs the Android SDK and NDK (ANDROID_NDK_HOME), rustup target aarch64-linux-android, cargo-ndk.
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
cd "$here"
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release -p nx-jni
sh tools/fetch_profiles.sh "$@"
cd android && gradle --no-daemon assembleDebug
echo "APK: $here/android/app/build/outputs/apk/debug/app-debug.apk"
