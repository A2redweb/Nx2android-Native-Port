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
        super.onCreate(savedInstanceState)
    }

    companion object { const val EXTRA_GAME = "game" }
}
