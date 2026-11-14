# Architecture: a native Android app, Rust core

Decision (2026-11-14, project owner): no WebView, no proot, no Python. A native Android app with a Rust core. Rust wherever it can be; the engine that runs the game stays C (it is already native Android code), and Kotlin is used only where Android forces it.

Everything below marked **measured** was read from upstream (`shirai91/nx2android` main) or run here. **Proposal** and **unproven** mark what has not been tested.

## What a port does today (upstream, Python)

Measured by reading `porter/pipeline.py`, `nxnative/scripts/build_ipa_from_nsp.py`, `nxnative/dm/android/build.py` and the tools they call.

| Stage | Done by today | Size | Per game? | Rust plan |
| --- | --- | --- | --- | --- |
| Read the dump (NSP/NSZ/XCI/XCZ container) | Python (`dump_files`) | ~20 lines | no | **Done**: `nx-container` |
| Keys (`prod.keys`) | Python | small | no | **Done**: `nx-keys` |
| Game list, profiles, advice | Python (`game.py`, `android_profile.py`, `porter/profiles.py`) | ~400 lines | data | **Done**: `nx-profile`, output identical to upstream for all 87 games |
| Extract (NCAs, update layered over base, RomFS, ExeFS, DLC) | C# `nx_extract` on LibHac | 290 lines on top of LibHac | no | Rust: NCA headers (AES-XTS), section crypto (AES-CTR), patch layering (BKTR), RomFS and ExeFS readers. The largest Rust piece after the translator |
| NSZ/XCZ decompression | Python (zstd, AES-CTR) | ~190 lines (`porter/dumps.py`) | no | Rust with a zstd and an AES crate |
| Rewrite the game's code (NSO modules) | Python `patch_nso.py` + capstone | 1,175 + 123 + 207 lines | yes, by build ID | Rust: AArch64 decode and rewrite. The per-game patch files are data tables (`CUTS`, `OBS`, `FRAME_STEP`; 6 `def`s in all 25 files), so they convert to plain data |
| Shader split (find every shader in the game's files) | Python `shader_split.py` | 1,697 lines, a reader per engine (Unity, Unreal paks, KTGL, Oodle archives...) | yes | Rust, reader by reader, starting with the games that matter most |
| Shader translation (Maxwell to SPIR-V) | C# `dmsh_xlate` over Ryujinx's translator | 280 lines + Ryujinx.Graphics.Shader 41,856, Spv.Generator 9,413, Ryujinx.Common 13,114 | yes | **Not rewritten at first**: ship the C# as a NativeAOT binary built for Android (see `SPIKE_DOTNET.md`) and drive it from Rust. A Rust port is a separate, large project |
| Shader bundle, profile tables | Python `shader_vk.py`, `shader_products.py`, `dmsv.py` | ~700 lines | yes | Rust |
| Build the host | `build.py` runs clang and lld per game | 144 lines | see below | **Removed**: one prebuilt host |
| Package the APK | Python `mkapk.py` + `apksign.py` | 330 lines, standard library only (binary manifest, stored ZIP, signature scheme v2, RSA-2048) | yes | **Removed** in the single-app design below. If per-game APKs are kept: a Rust port is small |
| Copy data into `Android/data` | adb | | | **Removed** in the single-app design |

The engine itself (`nxnative/dm/host` 10,666 lines of C and `dm/android` 9,989 lines of C) stays as it is.

## The host does not need compiling per game (proposal, partly measured)

`build.py` compiles the same C sources for every game. What differs per game is two generated headers: `game_profile.h` (from `game.tsv`; the host reads about 23 of its macros, such as `GAME_FPS_TARGET`, `GAME_ARGS`, `GAME_MEM_ALIAS`, `GAME_MODULES`) and `android_profile.h` (the gamepad layout from `android.tsv`). Everything else, including the backend's own shaders, is identical.

So: build `libdmhost.so` once (CI, with the NDK), and make the host read those values at run time from a small profile file the app writes into the game's data folder. That needs a change in upstream's C (macro to run-time value at about 23 use sites). **Unproven**: how many of those sites sit in hot paths or in array sizes that need a compile-time constant. Counting them is the first job of the host spike.

## One app, not one APK per game (proposal)

Upstream makes one code-less NativeActivity APK per game and puts the data into `Android/data/<game package>/files`, which Android 11 and later let only that app write (hence adb today). Making the player install an APK per game also needs install prompts and signing keys.

Proposal: **one app** that
- owns the game data in its own storage (no adb, no `Android/data` problem),
- carries the prebuilt host and the Vulkan driver loader in its own library folder,
- runs a game in a separate process (`android:process`) as the host expects one game per process, started with the game's id and data folder,
- offers a launcher shortcut per ported game.

Costs: the host's paths currently derive from the game's package name (`datapack.files_folder(package)`); they must take a data root from the launch intent. **Unproven** until tried on the Poco X6. The Oodle and run-time-shader capture steps, which upstream runs on a Mac and the old app ran over adb, become a child process of the app (the host built as an executable, started from the app's library folder).

## App layers

| Layer | Language | What it does |
| --- | --- | --- |
| UI | Kotlin, Android views | Setup, game list, game card, progress, settings. No WebView |
| Android glue | Kotlin | Storage picker for the dump and `prod.keys`, foreground service so a port survives the screen turning off, notifications, shortcuts, launching the game process |
| Core | Rust (`crates/`) | Everything in the table above, behind a C-style interface (`nx-jni`, to do) |
| Translator | C# NativeAOT for Android | `dmsh_xlate`, run as a child process |
| Engine | C (upstream) | The host library and executable, prebuilt by CI |

A command-line face of the core (`nxn`) runs on a PC or in Termux, so each Rust piece can be tested against upstream's Python on real data before the app exists. That is how `nx-profile` was checked.

## What was checked in this session

- `cargo test` passes for all crates (about 30 tests, including synthetic NSP and XCI images, corrupt headers, AES/XTS standard vectors and NCA headers).
- `nxn games` on upstream's `nxnative/dm/games`: output identical to `python -m porter profiles --all --json` for all 87 games, and the 55 `android.tsv` files all validate under the same rules.
- Not checked: any Android build. This environment has no Android Rust target, no NDK and no network access to crates.io, so the crates are dependency-free for now. Crypto, zstd, LZ4 and signing crates will be added on a machine with crates access.

## Milestones

1. **Done:** profiles, container listing, keys, CLI, tests.
2. **Extraction in Rust** (`nx-nca`, `nx-fs`): decrypt headers, read RomFS/ExeFS, layer the update. Check against `nx_extract` output on a real dump, file by file.
3. **Code rewriter in Rust** (`nx-nso`): output compared byte for byte with `patch_nso.py` for one game.
4. **Host spike:** a prebuilt host that takes its profile at run time; one app, one game process, data in app storage; run a known-good game by hand.
5. **Shaders:** translator as a child process; shader split in Rust for the first engines; bundle compared with upstream's.
6. **App shell** (Kotlin) around the core; setup, game list, port with progress.
7. Upstream contributions for the C changes and for the fixes carried from the proot app (copy split, shaders without reflection).
