# Design: full on-device port

Goal: the porter's whole pipeline (extract, shader split and translate, build host, package, install) runs on the phone with one-tap setup.

## Decision (2026-11-14): native app, Rust core

The project owner chose a native Android app with a Rust core: no WebView, no proot, no Python. This supersedes the 2026-11-13 note that kept Ubuntu and proot, which was a misreading of the request; the proot app's GUI was dropped from this repository. The plan is in [ARCHITECTURE.md](ARCHITECTURE.md); the questions below are still the right ones, and ARCHITECTURE.md gives the current answers or marks them as proposals.

## Open questions

| # | Question | Why it matters | First step to answer it |
| --- | --- | --- | --- |
| 1 | Can the .NET tools (extractor, `dmsh_xlate`) run natively on Android without proot, or must they be rewritten or replaced? | The standard runtime failed to start under proot ("Failed to create CoreCLR"); NativeAOT worked | Build the two tools as NativeAOT for linux-bionic arm64 and run them from the app |
| 2 | Does the host need compiling per game on the phone, or can a prebuilt host plus generated data do it? | Per-game builds need clang, lld and a per-game generated header; lld crashed under proot | List exactly what `nxnative/dm/android/build.py` generates per game |
| 3 | Can shader split and translation run once per game version and be shared? | Splitting is slow on a phone; profile tables already exist upstream | Compare the profile tables with split output; see the Castlevania mismatch after upstream `4c87f4d` |
| 4 | Where does the finished game live so the host reads it without adb? | Android blocks writing into another app's `Android/data` | Test app-private storage, `Android/media`, and a single-app design |
| 5 | How do runtime patches become upstream contributions? | Anchor drift breaks patches on every upstream refactor | Prepare PRs for copy-split, keep-shaders-without-reflection, retry crashed lld, optional strip |

## Constraints

- GPU must read BC-compressed textures (Adreno, Xclipse; not Mali or PowerVR).
- Needs about 6 GB free plus twice the game's size during a port.
- Android 14+ kills long child processes unless developer options are set; the new design should avoid needing that.
- Target API: the predecessor targeted an old API only because proot cannot run programs from app storage on newer targets. Removing proot may allow a modern target and Google Play.

## Proposed milestones

1. **Feasibility spikes** (one per open question), each with a measured result on the Poco X6.
2. **Minimal pipeline** for one known-good game, end to end on the phone.
3. **Auto-update layer**: porter sync plus version-aware patches.
4. **UI**: game folder and keys picker, progress, install.
5. **Upstream contributions**.

## Not yet decided

Language and framework for the app, and whether the porter's Python stays or is ported.
