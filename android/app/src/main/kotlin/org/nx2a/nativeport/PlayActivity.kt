package org.nx2a.nativeport

import android.os.Bundle
import android.system.Os
import org.nx2a.GameActivity
import java.io.File

/**
 * The game screen: upstream's NativeActivity wrapper running the universal libdmhost.so. The library picks the game's profile from DM_GAME and keeps its files under
 * DM_GAME_ROOT, so both are set before super.onCreate loads it. Saves and temp files live beside the game's data, not in a shared folder.
 */
class PlayActivity : GameActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        val id = intent.getStringExtra(EXTRA_GAME) ?: error("no game")
        gameId = id
        val root = File(filesDir, "data/$id")
        File(root, "save").mkdirs()
        File(root, "temp").mkdirs()
        Os.setenv("DM_GAME", id, true)
        Os.setenv("DM_GAME_ROOT", root.path, true)
        Os.setenv("DM_SAVE", File(root, "save").path, true)
        Os.setenv("DM_TEMP", File(root, "temp").path, true)
        if (Saves.access()) {   // exports and the import list live where the player can open them
            val shown = File(root, "name.txt").takeIf { it.exists() }?.readText()?.trim()?.ifEmpty { null } ?: id
            val d = Saves.dirFor(shown)
            if (d.isDirectory || d.mkdirs()) Os.setenv("DM_SAVES_DIR", d.path, true)
        }
        File(root, "aoc").takeIf { it.isDirectory }?.let { Os.setenv("DM_AOC_DIR", it.path, true) }
        Drivers.chosenFor(this, id)?.let {
            Os.setenv("DM_VK_DRIVER", it.lib, true)
            Os.setenv("DM_VK_DRIVER_DIR", it.dir.path + "/", true)
        }
        // the last run died in the driver right after its first attachment-less pass: this run skips those passes (the host's DM_VK_X_NOTARGET=0, as before they existed)
        try {
            val last = File(root, "log.txt").takeIf { it.isFile }?.readText() ?: ""
            val mark = File(root, "notarget")
            if (last.contains("CRASH sig=11") && last.contains("attachment-less pass")) mark.writeText("1")   // kept: the next runs do not go back to the crash
            if (mark.exists()) Os.setenv("DM_VK_X_NOTARGET", "0", true)
        } catch (_: Exception) {}
        // the game's own control defaults (a line in Extra settings overrides them): Pokémon Legends: Z-A turns the camera by dragging on the right half of the screen
        if (id == "0100F43008C44000") { Os.setenv("DM_CAM_ZONE", "0.5", true); Os.setenv("DM_THEME", "pokemonza", true) }   // (its pad pack: the letters with their actions' icons)
        // settings the player (or whoever is helping) typed in the game's sheet: KEY=VALUE lines, applied last
        try {
            File(root, "env_user.txt").takeIf { it.isFile }?.readLines()?.forEach { l ->
                val m = Regex("^\\s*([A-Z][A-Z0-9_]*)=(.*)$").find(l) ?: return@forEach
                Os.setenv(m.groupValues[1], m.groupValues[2].trim(), true)
            }
        } catch (_: Exception) {}
        super.onCreate(savedInstanceState)
        liveShaders(id, root)
    }

    @Volatile private var live = true

    /**
     * Shaders no game file holds are saved by the game as it meets them (captured_shaders/); this translates them into shaders_live.dmsv while the game runs, and the host
     * (gfx_vk_shader.c) opens that file beside the bundle, so such a shader is drawn within seconds of its first use instead of after the next shader update.
     */
    private fun liveShaders(id: String, root: File) {
        val xlate = File(applicationInfo.nativeLibraryDir, "libdmsh_xlate.so")
        if (!xlate.isFile) return
        Thread {
            try { android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_BACKGROUND) } catch (_: Exception) {}
            var done = -1
            while (live) {
                try {
                    Thread.sleep(3000)
                    val n = File(root, "captured_shaders").list { _, f -> f.endsWith(".bin") }?.size ?: 0
                    if (n == 0 || n == done) continue
                    val r = Core.nativeBuildLive(File(filesDir, "games").path, id, root.path, xlate.path, 1)
                    done = n
                    android.util.Log.i("nx2a", "live shaders ($n captured): $r")
                } catch (_: InterruptedException) { return@Thread } catch (e: Throwable) { android.util.Log.w("nx2a", "live shaders failed", e); done = -1; Thread.sleep(20000) }
            }
        }.also { it.isDaemon = true; it.name = "live-shaders" }.start()
    }

    override fun onDestroy() { live = false; super.onDestroy() }

    private var traceN = 0
    private var traceBusy = false
    private var gameId = ""

    override fun onKeyDown(keyCode: Int, event: android.view.KeyEvent): Boolean {
        if (keyCode == android.view.KeyEvent.KEYCODE_VOLUME_DOWN) { if (event.repeatCount == 0) traceFrame(); return true }   // volume down = "trace what is on screen now"
        return super.onKeyDown(keyCode, event)
    }

    override fun onKeyUp(keyCode: Int, event: android.view.KeyEvent): Boolean =
        if (keyCode == android.view.KeyEvent.KEYCODE_VOLUME_DOWN) true else super.onKeyUp(keyCode, event)

    /**
     * Frame trace: asks the host (DM_VK_LIST_AT, read again at every submission) for the pass and draw list of the next two presents (every pass: its targets, draws, shaders
     * by key, barriers), waits for them to reach log.txt and keeps that part as trace_N.txt. The Log link carries the latest. Used when a game draws something wrong: take
     * it with the wrong thing on screen, and what the GPU was told to draw can be compared with what is seen.
     */
    private fun traceFrame() {
        val id = gameId.ifEmpty { intent.getStringExtra(EXTRA_GAME) ?: return }
        if (traceBusy) return
        traceBusy = true
        val root = File(filesDir, "data/$id")
        val logf = File(root, "log.txt")
        val from = if (logf.isFile) logf.length() else 0L
        val n = ++traceN
        runOnUiThread { android.widget.Toast.makeText(this, "Tracing the next frames…", android.widget.Toast.LENGTH_SHORT).show() }
        Os.setenv("DM_VK_LIST_AT", "0" + " ".repeat(n), true)
        Thread {
            try {
                Thread.sleep(4000)
                val raf = java.io.RandomAccessFile(logf, "r")
                val txt = try { raf.seek(from.coerceAtMost(raf.length())); val b = ByteArray((raf.length() - raf.filePointer).coerceAtMost(6_000_000).toInt()); raf.readFully(b); String(b) } finally { raf.close() }
                val i = txt.indexOf("--- submission at present")
                val msg = if (i < 0) "no trace reached the log (the game was not drawing?)" else {
                    val t = txt.substring(i)
                    File(root, "trace_$n.txt").writeText("frame trace $n, ${t.lines().size} lines\n" + t.take(2_000_000))
                    File(root, "trace_latest.txt").writeText(File(root, "trace_$n.txt").readText())
                    "Frame trace saved (${t.lines().size} lines). Add it with Log link in the game's sheet."
                }
                runOnUiThread { android.widget.Toast.makeText(this, msg, android.widget.Toast.LENGTH_LONG).show() }
            } catch (_: Throwable) {} finally { traceBusy = false }
        }.also { it.isDaemon = true }.start()
    }

    companion object { const val EXTRA_GAME = "game" }
}
