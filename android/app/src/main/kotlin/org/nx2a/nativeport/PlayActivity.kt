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

    companion object { const val EXTRA_GAME = "game" }
}
