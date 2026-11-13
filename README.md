# nx2android-native

An Android app that turns a Nintendo Switch game dump into a playable Android app, entirely on the phone, with **one-tap setup**. It runs [shirai91/nx2android](https://github.com/shirai91/nx2android) (the porter; its engine is [seedds/nxnative](https://github.com/seedds/nxnative)) inside Ubuntu under proot, like its predecessor [A2redweb/nx2android-android-port](https://github.com/A2redweb/nx2android-android-port), and installs beside it (own application id `unknown.none.nx2android.oneclick`).

## What is new compared with the predecessor

- **One-tap setup.** The Setup tab has one button. It installs the Ubuntu build tools, downloads the newest upstream porter and applies our patches, installs the ready-made .NET tools and the trimmed Android NDK, finds `prod.keys` where Switch tools keep it, creates the output folder, reconnects adb, and ends with the porter's own `doctor` check. Every step can be repeated; a step that needs you (choosing `prod.keys`, pairing adb) says so and does not block the others. See [docs/ONE_CLICK.md](docs/ONE_CLICK.md).
- **Every game upstream knows.** The Games tab lists whatever `porter profiles --all --json` returns, so a game upstream adds appears by itself. Choosing a game folder shows its card (status, tested device, update found, what this app adds, whether adb is needed) and sets *Experimental* when the game has no Android profile yet. [docs/SUPPORTED_GAMES.md](docs/SUPPORTED_GAMES.md) is generated from upstream by `tools/make_games_doc.py`.
- **One-tap adb pairing.** The app finds the wireless-debugging pairing service itself and asks for the six-digit code in a notification; after that adb reconnects by itself.
- Everything else (file manager, per-game Vulkan driver, copy data over adb, auto-follow of upstream, over-the-air payload) is carried over.

## Status

Written and checked as far as possible without a phone or the Android SDK: the Python modules compile and the game list and setup check were run against upstream's real profiles; the page script parses. **Not yet built or run on a device**: the APK build, the Java (`AdbPairing.java`), and the full setup run on the Poco X6 are still to be tested.

## Layout

- `app/`: Android app (a WebView, proot, a bundled Ubuntu) and `assets/nx2a/payload/` (the over-the-air part: GUI server, `oneclick.py`, `profiles_catalog.py`, patches).
- `tools/`: payload manifest, games doc, proot and rootfs fetch scripts, and the bionic-tools spike (`make_bionic_tools.sh`, for a later native runtime, see [docs/SPIKE_DOTNET.md](docs/SPIKE_DOTNET.md)).
- After editing the payload run `python3 tools/make_manifest.py --bump`; CI refuses a stale `payload.json`.

## Private repository note

Phones fetch payload updates from `raw.githubusercontent.com/<repo>`, which does not work for a private repository without a token. Until the repo is public (or payload updates are served another way), the app runs the payload bundled in its APK. The ready-made .NET tools and the trimmed NDK are fetched from the predecessor's public releases (`update.json`: `prebuilt_repo`, `tools_repo`).

## Test device

Poco X6, Adreno 710, Turnip driver, Android 14.
