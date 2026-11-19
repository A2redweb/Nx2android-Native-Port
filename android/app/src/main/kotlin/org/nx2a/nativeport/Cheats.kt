package org.nx2a.nativeport

import org.json.JSONObject
import java.io.File

/**
 * Cheats from the community's online database (HamletDuFromage/switch-cheats-db, cheats/<title id>.json: build ID (first 8 bytes) -> cheat name -> Atmosphère cheat text).
 * The cheats of the game's own build are written to cheats.txt in the game's folder, the switched-on names to cheats_on.txt; the game's host runs them (dm_cheats.c) and
 * its pause menu has a Cheats tab to switch them too.
 */
object Cheats {
    private const val BASE = "https://raw.githubusercontent.com/HamletDuFromage/switch-cheats-db/master/cheats/"

    class Cheat(val name: String, var on: Boolean, val master: Boolean)

    /** the cheats now in the game's folder (empty if none were downloaded) */
    fun list(dir: File): List<Cheat> {
        val f = File(dir, "cheats.txt")
        if (!f.isFile) return emptyList()
        val on = File(dir, "cheats_on.txt").takeIf { it.isFile }?.readLines()?.map { it.trim() }?.toSet() ?: emptySet()
        val out = ArrayList<Cheat>()
        for (l in f.readLines()) {
            val s = l.trim()
            if (s.length > 2 && (s[0] == '[' || s[0] == '{')) {
                val master = s[0] == '{'
                val n = s.substring(1, s.length - 1)
                out.add(Cheat(n, master || n in on, master))
            }
        }
        return out
    }

    fun setOn(dir: File, name: String, on: Boolean) {
        val cur = (File(dir, "cheats_on.txt").takeIf { it.isFile }?.readLines()?.map { it.trim() }?.filter { it.isNotEmpty() } ?: emptyList()).toMutableList()
        cur.remove(name)
        if (on) cur.add(name)
        File(dir, "cheats_on.txt").writeText(cur.joinToString("\n") + (if (cur.isEmpty()) "" else "\n"))
    }

    /** Downloads (or reads the kept copy of) this game's cheat file and writes the ones for its build. Returns a line for the player: how many, or why none. Run off the UI thread. */
    fun fetch(a: android.content.Context, dir: File, tid: String, refresh: Boolean): String {
        val plan = Core.nativeFpsPlan(dir.path, tid)
        if (!plan.startsWith("ok: ")) return "Cannot read the game's build: ${plan.removePrefix("error: ")}"
        val bid = plan.removePrefix("ok: ").substringBefore('\t').take(16).uppercase()
        val cache = File(a.cacheDir, "cheats").apply { mkdirs() }
        val f = File(cache, "${tid.uppercase()}.json")
        if (refresh || !f.isFile) {
            try {
                val c = java.net.URL(BASE + tid.uppercase() + ".json").openConnection() as java.net.HttpURLConnection
                c.connectTimeout = 20000; c.readTimeout = 30000; c.setRequestProperty("User-Agent", "nx2android-native")
                when (c.responseCode) {
                    200 -> f.writeText(c.inputStream.bufferedReader().readText())
                    404 -> return "The online database has no cheats for this game."
                    else -> return "The cheat server answered ${c.responseCode}; try again later."
                }
            } catch (e: Exception) { return "Could not download the cheats (${e.message}); is the phone online?" }
        }
        val root = try { JSONObject(f.readText()) } catch (e: Exception) { f.delete(); return "The cheat file could not be read (${e.message})." }
        val mine = root.optJSONObject(bid)
            ?: return "The database has cheats for ${root.length()} other version(s) of this game, none for your build ($bid)."
        val sb = StringBuilder()
        var n = 0
        val keys = mine.keys().asSequence().toList()
        for (k in keys) {
            val text = mine.optString(k)
            val lines = text.lines()
            sb.append(k).append('\n')
            lines.drop(if (lines.firstOrNull()?.trim() == k.trim()) 1 else 0).forEach { sb.append(it).append('\n') }
            sb.append('\n')
            if (!k.trim().startsWith("{")) n++
        }
        File(dir, "cheats.txt").writeText(sb.toString())
        val names = list(dir).map { it.name }.toSet()   // keep the switched-on ones that still exist
        val on = File(dir, "cheats_on.txt").takeIf { it.isFile }?.readLines()?.map { it.trim() }?.filter { it in names } ?: emptyList()
        File(dir, "cheats_on.txt").writeText(on.joinToString("\n") + (if (on.isEmpty()) "" else "\n"))
        return "$n cheats for your build of the game."
    }
}
