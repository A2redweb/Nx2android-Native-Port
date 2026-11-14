# Prior art: what the proot app worked around

| Stage or need | How the proot app does it | Pain |
| --- | --- | --- |
| Run Python and tools | Ubuntu under proot inside the APK | Slow; Android kills long child processes; old target API, so no Google Play |
| C# extractor and translator | CI-built NativeAOT binaries (`prebuilt` release) | Standard .NET runtime fails under proot |
| Build the Android host per game | System clang and lld with the NDK sysroot, via patches | NDK tools are x86-64; ld.lld crashed in realloc and needed retries |
| Run-time shader capture, Oodle archives | Run the host on the same phone over adb | Needs wireless debugging; upstream does it on a Mac |
| Copy game data | adb push | Android blocks `Android/data` writes |
| Storage | About 6 GB plus twice the game size | Working files pile up |
| Tracking upstream | Runtime patches in `apply_patches.py` | Anchors break on upstream refactors |

## Patches worth upstreaming

- Split a buffer copy at zero-copy chunk ends instead of dropping it (`NX2A_COPYSPLIT`).
- Keep shaders whose translator wrote SPIR-V but no reflection (`NX2A_NOREFL`, upstream #212).
- Retry a build command when clang or lld segfaults (`NX2A_RETRY`).
- A failed strip of the host library is not fatal (`NX2A_STRIPOPT`).
- Skip an empty `DATA1.bin` in the shader split (`NX2A_EMPTYDATA`).

## Known upstream issues carried over

- Castlevania: Belmont's Curse split tables differ from the profile after upstream `4c87f4d`.
- Start-up crash on upstream's newest host (`strcmp` on a bad pointer in `loader.c`); cause unknown.
