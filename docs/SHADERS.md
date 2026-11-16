# Shaders on the phone

Every game's NVN shaders are turned into Vulkan SPIR-V on the phone, once, after the game is imported. The result is `shaders.dmsv` in the game's data folder; the host draws nothing without it.

Chain (all Rust, `crates/nx-shader`, except the translator):

1. **find** the shaders in the game's RomFS and in `main`'s memory image (`scan`, `split`, `container`, `unity`, `lzma`);
2. **describe** each as a translator job (`job`), using the profile's `shader_facts.tsv`;
3. **translate** (`pipeline`) with `libdmsh_xlate.so`, upstream's translator (C#, over Ryujinx's shader code) compiled with NativeAOT for Android and run as a child process from the app's native library directory;
4. **classify and pack** (`spirv`, `bind`, `dmsv`) into the bundle.

## What is checked, and how

- The JSON, SPIR-V interface, binding classification, job building and handle remapping were compared with upstream's Python on generated inputs (byte-identical).
- LZMA and zstd dictionary decoding were compared with the reference encoders' output.
- The Unity serialized-file reader was compared with `unityfs.py` on a synthetic file.
- **Not yet run:** the translator on Android (the NativeAOT build is decided by CI), and any of the readers on a real game's files. A game's first import is the first real test.

## Where shaders are found

| Source | Reader | Games (of 81 profiles) |
|---|---|---|
| GLSLC output (in `main`, loose files, archives) | `glslc_shaders` | 43 |
| BNSH / BAHS in files, SARC, Yaz0, zstd, size-prefixed `.cmp` (zstd / LZ4) | `bnsh_shaders`, `bahs_shaders` | 29 / 10 |
| Unity bundles and assets (type trees, LZMA / LZ4) | `unity` | 27 |
| Tears of the Kingdom's zstd dictionaries (`Pack/ZsDic.pack.zs`) | `zs_dicts` | 1 |
| Metroid Dread (MSHD), Celeste (MGFX) | `mshd_shaders`, `mgfx_shaders` | 1 each |

Games can use several of these. A game whose shaders sit inside a container this table does not list is in the next section.

## Not supported on the phone yet

These games keep their shaders in formats whose readers are not ported (several need Oodle or other game-specific decoders that upstream runs on a Mac). Their shader step reports the shaders it could not find rather than guessing.

- Unreal Engine: `01005270232F2000`, `010057D006492000`, `0100606026F12000`, `01006C300E9F0000`, `0100A9D01C446000`
- KTGL (idrk): `01002B00111A2000`, `0100C2A01A730000`
- Others: `0100300012F2A000` (control sections in main), `010036E00FB20000` (MTSPK), `01004A4010FEA000` (PGSO), `010074F013262000` (HCPS / SLCT), `0100B04011742000` (SDF), `0100DCA0064A6000` (NLG)

Regenerate the Unity layouts with `tools/gen_unity_layouts.py`, the zstd dictionary vectors with `tools/make_zstd_dict_vectors.py`.
