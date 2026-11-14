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
}
