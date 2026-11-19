# nx2android-native

A native Android app that turns a Nintendo Switch game dump into a game that runs on the phone, with no PC, no proot and no WebView. The core is Rust.

It builds on [shirai91/nx2android](https://github.com/shirai91/nx2android) (the porter) and its engine [seedds/nxnative](https://github.com/seedds/nxnative), rewriting the porter's Python and C# steps in Rust and keeping the engine, which is already native Android C. The earlier proot-and-WebView app, [A2redweb/nx2android-android-port](https://github.com/A2redweb/nx2android-android-port), is the predecessor: nothing of its GUI is carried over.

## Status

Early. Done and tested on a PC:

| Crate | What it does | Checked against |
| --- | --- | --- |
| `nx-profile` | Game and Android profiles, the games list | Upstream's own `porter profiles --all --json`: identical for all 87 games |
| `nx-container` | Files inside an NSP / NSZ / XCI / XCZ | Synthetic images (a real dump is not available here) |
| `nx-keys` | `prod.keys`, key values never printed | Unit tests |
| `nx-crypto` | AES-128 (ECB, CTR), AES-XTS with Nintendo's big-endian tweak, SHA-256, std only | FIPS-197, SP 800-38A, IEEE 1619, FIPS 180 vectors |
| `nx-nca` | NCA headers, title ID, key derivation, CTR sections, BKTR layering, hash-tree verification | Key derivation and header_key checked against a real `prod.keys` (17/17 master keys, 22/22 titlekeks, 66/66 key area keys); the rest on synthetic images only |
| `nx-zstd` | Zstandard decoder in plain Rust (no dictionaries) | libzstd output at levels 1-19, 4 kinds of data, corrupt inputs; 120-400 MB/s on the dev machine |
| `nx-nsz` | NSZ/XCZ to NSP (NCZ sections re-encrypted, name hash checked) | Synthetic NCZ files incl. a libzstd stream; no real NSZ yet |
| `nx-nso` | NSO/NRO header, LZ4, memory image, segment hashes, build ID | Hand-made vectors and round trips; no real module yet |
| `nx-fs` | RomFS reader, PFS0 and RomFS extraction, safe path joining | Synthetic images |
| `nx-extract` | Game from an NSP/XCI/NSZ/XCZ, with an update layered over it (BKTR) and add-on content: ExeFS, RomFS, control.nacp, icon, game name and version (tickets, key area and title keys; inflater for compressed NACPs) | End-to-end on a synthetic NSP; not a real dump |
| `nx-core` | What the app calls: games list, keys check, extraction jobs polled as JSON | Unit tests (errors, junk dump, JSON) |
| `nx-jni` | `libnxcore.so`: five JNI functions, strings and ints only | Symbols checked with `nm`; never loaded on a device |
| `nxn` | Command line for the above | |

Not done: hash checks of patch (update) sections, system data (`--sysdata`), the NSO rewriter, shaders, the host build and the packaging of a runnable game.

## The app (android/)

Written, **never built or run**: this environment has no Android SDK, NDK or network. It is one screen of native views (no WebView): choose `prod.keys`, choose the folder with your dumps, tap a dump to extract it (optionally with an update). Extraction runs in a foreground service (type dataSync) and survives the screen turning off. Files are read through storage-access descriptors, so no storage permission and no adb. What it produces today is the extracted ExeFS / RomFS / control data in the app's own storage; turning that into a playable game is the unfinished part of the pipeline above.
`.github/workflows/android.yml` builds it (cargo-ndk, 16 KiB page alignment, Gradle, SDK 36); the first run will show what is wrong. Locally: `sh tools/build_android.sh`., the code rewriter, shaders, the Android app. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the stage-by-stage plan, what is measured and what is a proposal.

```
cargo test
cargo run -p nxn -- games <upstream>/nxnative/dm/games --all --overlay <upstream>/nxnative/dm/android/overlay_default.tsv
cargo run -p nxn -- dump game.nsp
cargo run -p nxn -- keys prod.keys
cargo run -p nxn -- title game.nsp prod.keys
cargo run -p nxn -- extract prod.keys game.nsp out/ --update update.nsp   # .nsz / .xcz work too
cargo run -p nxn -- decompress game.nsz game.nsp
cargo run -p nxn -- aoc prod.keys dlc.nsp out/aoc
cargo run -p nxn -- verify prod.keys game.nsp        # hash trees: finds damaged files
```

## Saves and logs

- Export save (pause menu) writes to `Nx2android/exported saves/<game name>/` on the phone's storage. That needs Android's "All files access" for the app (Games tab > Saves > Allow access); no root or Shizuku. Without it, exports stay in the app's own folder.
- Games tab > Saves lists the saves in that folder to import, or imports from any folder you pick. An import is applied the next time the game starts, after the current save is backed up.
- Games tab > Copy log link uploads that game's log (port log and the game's output) to a public paste site and copies the link.

## Docs

- [ARCHITECTURE.md](docs/ARCHITECTURE.md): the plan
- [PRIOR_ART.md](docs/PRIOR_ART.md): what the proot app worked around
- [SPIKE_DOTNET.md](docs/SPIKE_DOTNET.md): running the C# shader translator on Android itself
- [DESIGN.md](docs/DESIGN.md): the first design notes (superseded by ARCHITECTURE.md where they differ)
- [SUPPORTED_GAMES.md](docs/SUPPORTED_GAMES.md): generated by the predecessor's tool from upstream; to be regenerated by `nxn`

## Test device

Poco X6, Adreno 710, Turnip driver, Android 16.

## The app

Package `unknownnone.nx2a.native`, with the icon and the dark design of the [nx2android phone app](https://github.com/A2redweb/nx2android-android-port), drawn with native views (no WebView).
Tabs: **Setup** (checks, prod.keys), **Port** (game folder, found games grouped as base + newest update + DLC, options, file manager, free space), **Games** (ported games: Play / Delete; supported games), **Log** (stage, progress, Cancel, Copy log).
The **Update** button (and a check at start) installs the newest build from the `nightly` pre-release over the running one; every build is a release build (not debuggable), signed with the key kept in `android/app/debug.keystore`, so updates install in place.
What the phone app did through proot and adb is not needed here: no tools to download, no adb pairing, no APK to install, no data to copy by hand; the app runs the games itself. Custom Vulkan drivers (a Turnip .so or .zip) are added in the Port tab and loaded through libadrenotools (not yet tried on a device). 60 fps uses the profile's own frame-rate cap or the community's FPSLocker config of your build (downloaded at port time, converted by `nx-fps`; configs that need assembly patches or variables are refused, as upstream does). 

**Not in this app (and why)**
- Experimental ports of games without an upstream profile: upstream's porter surveys the game and guesses a profile, then needs rounds of on-device runs; the app extracts such a game but cannot start it.
- Shaders of Unreal Engine and KTGL games (and other engines without a reader): upstream makes those on a connected device through adb; here they are skipped and the Games tab says so.
- Firmware system data (`--firmware`): only the console's own shared fonts use it; the host's stand-in fonts (or your own .ttf, Port tab) are used instead.
- A game build the profile does not register: the port warns in red; upstream derives a profile for such a build, this app does not.
- Alias-region headroom, the external-display gamepad and spin-wait copies in the rewriter (performance only), and the one function (Final Fantasy Resonance) the rewriter cannot rewrite.

Nothing in this app has been run on a phone beyond the Setup screen: expect bugs in the untested stages.
