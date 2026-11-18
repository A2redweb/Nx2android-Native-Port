package org.nx2a.nativeport

/** The Rust core (libnxcore.so, crates/nx-jni). Only strings and ints cross; progress is polled with [nativeStatus]. */
object Core {
    init {
        System.loadLibrary("nxcore")
    }

    external fun nativeVersion(): String
    external fun nativeGames(dir: String, overlay: String, all: Boolean): String
    external fun nativeKeys(path: String): String
    external fun nativeExtract(keys: String, base: String, baseName: String, update: String, updateName: String, out: String, tmp: String, dlc: String): Int
    external fun nativeStatus(id: Int): String
    /** Stops an extraction at its next read of the dump. */
    external fun nativeCancel(id: Int)
    /** Writes env.txt and settings.txt into a game's folder; returns an error text, empty when done. */
    external fun nativePrepare(profiles: String, id: String, dir: String, language: String): String
    /** Rewrites the game's code into its mods/ folder (minutes for a big game). Returns "ok: ..." or "error: ...". */
    external fun nativeRewrite(profiles: String, id: String, dir: String): String
    external fun nativeTitle(keys: String, path: String, name: String): String
    external fun nativeFpsPlan(dir: String, titleId: String): String
    external fun nativeFpsConvert(dir: String, titleId: String, text: String, source: String): String
    external fun nativeBuildShaders(profiles: String, id: String, dir: String, xlate: String, threads: Int): String
}
