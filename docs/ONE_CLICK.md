# One-tap setup

`payload/oneclick.py` runs these steps in order. `check` only looks; `start` (the button) fixes what is missing, retrying downloads up to three times.

| Step | Required | What it does |
| --- | --- | --- |
| Free space | yes | needs about 4 GB for setup; stops before downloading if there is less |
| Ubuntu build tools | yes | runs `inner_setup.sh` when clang, lld, glslc or adb is missing or the setup number is newer |
| Porter | yes | `porter_sync.sync()`: upstream's newest commit (or the known-good one when chosen), patched |
| Ready-made .NET tools | yes | `prebuilt.install()` from the predecessor's public `prebuilt` release, with a real start-up test |
| Android NDK | yes | the trimmed NDK from the predecessor's `tools` release |
| prod.keys | yes | uses the configured file, else looks in the usual places (and 3 folder levels of internal storage, never `Android/`); otherwise asks you to choose it |
| Output folder | yes | `/sdcard/Switch/Ports` unless set |
| adb | no | reconnects wireless debugging if paired; otherwise offers notification pairing |
| Final check | yes | `porter doctor --json`; any required failure is shown |
| Game list | no | loads `porter profiles --all --json` |

The setup is never started without the button, because it downloads about 1.5 GB. At start-up the app runs `check` so the list is filled in.

Endpoints (payload server): `GET /api/oneclick`, `POST /api/oneclick`, `GET /api/games[?refresh=1]`, `POST /api/match {folder}`.
