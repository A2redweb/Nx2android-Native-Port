package org.nx2a.nativeport

import android.content.Context
import android.net.Uri
import java.io.File
import java.util.zip.ZipInputStream

/**
 * Custom Vulkan drivers (a Mesa Turnip build for Adreno, as a .so or as the .zip the driver builders publish: meta.json plus the library). Each lives in its own folder
 * under files/drivers; the one the player chose is handed to the game host (DM_VK_DRIVER, DM_VK_DRIVER_DIR), which loads it through libadrenotools. The host falls back to the
 * phone's own driver when this one does not load or offers no Vulkan 1.3 GPU, and says so in the log.
 */
object Drivers {
    class Driver(val id: String, val dir: File, val lib: String)

    private fun root(c: Context) = File(c.filesDir, "drivers")

    fun list(c: Context): List<Driver> = (root(c).listFiles() ?: emptyArray()).filter { it.isDirectory }.sortedBy { it.name }.mapNotNull { d ->
        File(d, "lib.name").takeIf { it.isFile }?.readText()?.trim()?.takeIf { File(d, it).isFile }?.let { Driver(d.name, d, it) }
    }

    fun chosen(c: Context): Driver? {
        val id = c.getSharedPreferences("nx2a", Context.MODE_PRIVATE).getString("driver", null) ?: return null
        return list(c).firstOrNull { it.id == id }
    }

    /** the driver for one game: its own choice if it has one ("system" = the phone's), else the one chosen for all games */
    fun chosenFor(c: Context, game: String): Driver? {
        val own = c.getSharedPreferences("nx2a", Context.MODE_PRIVATE).getString("driver_$game", null) ?: return chosen(c)
        return if (own == "system") null else list(c).firstOrNull { it.id == own } ?: chosen(c)
    }

    /** null: as for all games; "system": the phone's own driver; else a driver id */
    fun chooseFor(c: Context, game: String, id: String?) {
        c.getSharedPreferences("nx2a", Context.MODE_PRIVATE).edit().apply { if (id == null) remove("driver_$game") else putString("driver_$game", id) }.apply()
    }

    fun ownChoice(c: Context, game: String): String? = c.getSharedPreferences("nx2a", Context.MODE_PRIVATE).getString("driver_$game", null)

    fun choose(c: Context, id: String?) {
        c.getSharedPreferences("nx2a", Context.MODE_PRIVATE).edit().apply { if (id == null) remove("driver") else putString("driver", id) }.apply()
    }

    /** Imports a driver; returns its id. Throws with a sentence the UI shows. */
    fun add(c: Context, uri: Uri, fileName: String): String {
        val id = fileName.substringBeforeLast('.').replace(Regex("[^A-Za-z0-9._-]"), "_").ifEmpty { "driver" }
        val dir = File(root(c), id)
        dir.deleteRecursively(); dir.mkdirs()
        try {
            c.contentResolver.openInputStream(uri)!!.use { ins ->
                if (fileName.endsWith(".zip", true)) {
                    var meta: String? = null
                    ZipInputStream(ins.buffered()).use { z ->
                        while (true) {
                            val e = z.nextEntry ?: break
                            if (e.isDirectory) continue
                            val n = e.name.substringAfterLast('/')
                            if (n == "meta.json") meta = z.readBytes().toString(Charsets.UTF_8)
                            else if (n.endsWith(".so")) File(dir, n).outputStream().use { z.copyTo(it) }
                        }
                    }
                    val named = meta?.let { Regex("\"libraryName\"\\s*:\\s*\"([^\"]+)\"").find(it)?.groupValues?.get(1) }
                    val lib = named?.takeIf { File(dir, it).isFile }
                        ?: dir.listFiles()?.map { it.name }?.firstOrNull { it.startsWith("vulkan.") || it.startsWith("libvulkan_") }
                        ?: dir.listFiles()?.firstOrNull { it.name.endsWith(".so") }?.name
                        ?: throw IllegalStateException("no driver library (.so) in that zip")
                    File(dir, "lib.name").writeText(lib)
                } else {
                    if (!fileName.endsWith(".so", true)) throw IllegalStateException("choose a .so or a .zip")
                    File(dir, fileName).outputStream().use { ins.copyTo(it) }
                    File(dir, "lib.name").writeText(fileName)
                }
            }
        } catch (e: Exception) { dir.deleteRecursively(); throw e }
        return id
    }

    fun remove(c: Context, id: String) {
        File(root(c), id).deleteRecursively()
        if (c.getSharedPreferences("nx2a", Context.MODE_PRIVATE).getString("driver", null) == id) choose(c, null)
    }
}
