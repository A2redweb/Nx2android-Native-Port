package org.nx2a.nativeport

import android.app.Activity
import android.content.ContentResolver
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.Settings
import java.io.File

/**
 * Where saves go: a folder the player can open without root or Shizuku, `Nx2android/exported saves/<game name>` on the phone's storage. Writing there needs the
 * "All files access" switch (Android's own page, one tap); without it the game keeps its exports in its app folder as before.
 */
object Saves {
    class Entry(val name: String, val files: Int, val bytes: Long, val time: Long, val device: Boolean)

    fun root() = File(Environment.getExternalStorageDirectory(), "Nx2android/exported saves")

    fun access() = Build.VERSION.SDK_INT >= 30 && Environment.isExternalStorageManager()

    fun folderName(shown: String) = shown.replace(Regex("[\\\\/:*?\"<>|\\p{Cntrl}]"), "_").trim().trim('.').ifEmpty { "game" }.take(80)

    fun dirFor(shown: String) = File(root(), folderName(shown))

    fun requestAccess(a: Activity) {
        try {
            a.startActivity(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:${a.packageName}")))
        } catch (_: Exception) {
            a.startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
        }
    }

    private fun size(f: File): Pair<Int, Long> {
        var n = 0; var b = 0L
        f.walkTopDown().forEach { if (it.isFile) { n++; b += it.length() } }
        return n to b
    }

    /** the saves in [dir], newest first: folders other than hidden ones and the `.device` partners */
    fun list(dir: File): List<Entry> =
        (dir.listFiles { f -> f.isDirectory && !f.name.startsWith(".") && !f.name.endsWith(".device") } ?: emptyArray()).map {
            val (n, b) = size(it)
            val dev = File(dir, it.name + ".device")
            val (n2, b2) = if (dev.isDirectory) size(dev) else 0 to 0L
            Entry(it.name, n + n2, b + b2, it.lastModified(), dev.isDirectory)
        }.sortedByDescending { it.time }

    /** asks the game to import [name] from [dir] at its next start (saves_android.c reads import.txt) */
    fun requestImport(dir: File, name: String) {
        dir.mkdirs()
        File(dir, "import.txt.new").writeText("$name\nrequest=app\n")
        File(dir, "import.txt.new").renameTo(File(dir, "import.txt"))
    }

    /** copies the folder the player picked ([tree], a document tree) into [dst] */
    fun copyTree(cr: ContentResolver, tree: Uri, docId: String, dst: File): Int {
        dst.mkdirs()
        var n = 0
        val kids = DocumentsContract.buildChildDocumentsUriUsingTree(tree, docId)
        cr.query(kids, arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE), null, null, null)?.use { c ->
            while (c.moveToNext()) {
                val id = c.getString(0); val name = c.getString(1) ?: continue
                if (name.contains('/') || name == ".." || name == ".") continue
                if (c.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR) n += copyTree(cr, tree, id, File(dst, name))
                else cr.openInputStream(DocumentsContract.buildDocumentUriUsingTree(tree, id))?.use { i -> File(dst, name).outputStream().use { o -> i.copyTo(o) }; n++ }
            }
        }
        return n
    }
}
