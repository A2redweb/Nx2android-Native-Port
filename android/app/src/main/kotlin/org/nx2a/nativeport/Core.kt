package org.nx2a.nativeport

/** The Rust core (libnxcore.so, crates/nx-jni). Only strings and ints cross; progress is polled with [nativeStatus]. */
object Core {
    init {
        System.loadLibrary("nxcore")
    }

    external fun nativeVersion(): String
    external fun nativeGames(dir: String, overlay: String, all: Boolean): String
    external fun nativeKeys(path: String): String
    external fun nativeExtract(keys: String, base: String, baseName: String, update: String, updateName: String, out: String, tmp: String): Int
    external fun nativeStatus(id: Int): String
    /** Writes env.txt and settings.txt into a game's folder; returns an error text, empty when done. */
    external fun nativePrepare(profiles: String, id: String, dir: String, language: String): String
    /** Rewrites the game's code into its mods/ folder (minutes for a big game). Returns "ok: ..." or "error: ...". */
    external fun nativeRewrite(profiles: String, id: String, dir: String): String
    external fun nativeBuildShaders(profiles: String, id: String, dir: String, xlate: String, threads: Int): String
}
