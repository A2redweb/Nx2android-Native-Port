package org.nx2a.nativeport

import android.content.Context
import org.json.JSONObject
import java.io.File

/** The games the app has a profile for (from the profiles in the app's folder), by title ID. */
object Catalog {
    class Game(val id: String, val name: String, val status: String)

    fun profilesDir(c: Context) = File(c.filesDir, "games")

    fun games(c: Context): List<Game> = try {
        val j = JSONObject(Core.nativeGames(profilesDir(c).path, "", false))
        val a = j.optJSONArray("games")
        if (a == null) emptyList() else (0 until a.length()).map { a.getJSONObject(it) }.map { Game(it.optString("id"), it.optString("name"), it.optString("status")) }
    } catch (_: Exception) { emptyList() }

    fun byId(c: Context): Map<String, Game> = games(c).associateBy { it.id.uppercase() }

    fun error(c: Context): String? = try { JSONObject(Core.nativeGames(profilesDir(c).path, "", false)).optString("error").ifEmpty { null } } catch (e: Exception) { e.message }

    /** The profiles ship in the app's assets (copied from upstream at build time); the core reads them from a folder, so they are copied out once per app version. */
    fun install(c: Context) {
        val prefs = c.getSharedPreferences("nx2a", Context.MODE_PRIVATE)
        val dst = profilesDir(c)
        val ver = c.packageManager.getPackageInfo(c.packageName, 0).longVersionCode.toInt()
        val commit = BuildConfig.COMMIT
        if (dst.exists() && prefs.getInt("profiles_for", 0) == ver && prefs.getString("profiles_commit", "") == commit) return
        dst.deleteRecursively()
        fun copy(path: String, to: File) {
            val kids = c.assets.list(path) ?: return
            if (kids.isEmpty()) {
                to.parentFile?.mkdirs()
                c.assets.open(path).use { i -> to.outputStream().use { o -> i.copyTo(o) } }
            } else kids.forEach { copy("$path/$it", File(to, it)) }
        }
        copy("games", dst)
        prefs.edit().putInt("profiles_for", ver).putString("profiles_commit", commit).apply()
    }

    /** Bytes under [f]. An iterative walk, and slow on a game with tens of thousands of files (a ported game's shaders): call it off the main thread only. */
    fun size(f: File): Long {
        if (f.isFile) return f.length()
        var total = 0L
        val todo = arrayListOf(f)
        while (todo.isNotEmpty()) {
            val kids = todo.removeAt(todo.size - 1).listFiles() ?: continue
            for (k in kids) if (k.isDirectory) todo.add(k) else total += k.length()
        }
        return total
    }

    fun human(n: Long): String = when {
        n >= 1_000_000_000 -> "%.1f GB".format(n / 1e9)
        n >= 1_000_000 -> "%.1f MB".format(n / 1e6)
        n >= 1_000 -> "%d KB".format(n / 1000)
        else -> "$n B"
    }
}
