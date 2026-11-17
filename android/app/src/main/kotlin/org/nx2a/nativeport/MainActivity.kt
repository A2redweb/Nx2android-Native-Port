package org.nx2a.nativeport

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.SharedPreferences
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import org.json.JSONObject
import java.io.File

/** One screen, native views (no WebView): your keys, your games folder, the dumps found in it, and a button per dump. */
class MainActivity : Activity() {
    private lateinit var prefs: SharedPreferences
    private lateinit var info: TextView
    private lateinit var list: LinearLayout
    private val ui = Handler(Looper.getMainLooper())
    private var pendingBase: Pair<Uri, String>? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = getSharedPreferences("nx2a", MODE_PRIVATE)
        requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        copyProfiles()

        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(24), dp(16), dp(16))
        }
        col.addView(TextView(this).apply { text = "Nx2Android Native"; textSize = 24f })
        info = TextView(this).apply { setPadding(0, dp(8), 0, dp(8)) }
        col.addView(info)
        col.addView(button("1. Choose prod.keys") { pick(REQ_KEYS) })
        col.addView(button("2. Choose the folder with your dumps") { startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).addFlags(treeFlags), REQ_TREE) })
        col.addView(button("Games the porter supports") { showSupported() })
        list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        col.addView(list)
        setContentView(ScrollView(this).apply { addView(col) })
        refresh()
        tick()
    }

    private val treeFlags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun button(label: String, onClick: () -> Unit) = Button(this).apply {
        text = label
        isAllCaps = false
        gravity = Gravity.START or Gravity.CENTER_VERTICAL
        setOnClickListener { onClick() }
    }

    private fun pick(req: Int) {
        startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*"), req)
    }

    /** The game profiles ship in the app's assets (copied from upstream at build time); the core reads them from a folder. */
    private fun copyProfiles() {
        val dst = File(filesDir, "games")
        if (dst.exists() && prefs.getInt("profiles_for", 0) == packageManager.getPackageInfo(packageName, 0).longVersionCode.toInt()) return
        dst.deleteRecursively()
        fun copy(path: String, to: File) {
            val kids = assets.list(path) ?: return
            if (kids.isEmpty()) {
                to.parentFile?.mkdirs()
                assets.open(path).use { i -> to.outputStream().use { o -> i.copyTo(o) } }
            } else kids.forEach { copy("$path/$it", File(to, it)) }
        }
        copy("games", dst)
        prefs.edit().putInt("profiles_for", packageManager.getPackageInfo(packageName, 0).longVersionCode.toInt()).apply()
    }

    private fun refresh() {
        val keys = File(filesDir, "prod.keys")
        val keyText = if (keys.exists()) {
            val j = JSONObject(Core.nativeKeys(keys.path))
            if (j.has("error")) j.getString("error") else "keys: ${j.getInt("keys")} (${j.getInt("master_keys")} master keys)" + if (j.getBoolean("header_key")) "" else ", header_key MISSING"
        } else "keys: not chosen yet"
        val tree = prefs.getString("tree", null)
        info.text = "core ${Core.nativeVersion()}\n$keyText\ndumps folder: ${tree?.let { Uri.parse(it).lastPathSegment } ?: "not chosen yet"}"
        list.removeAllViews()
        showPorted()
        if (tree != null) scan(Uri.parse(tree))
    }

    /** Games extracted so far, each with a Play button. The host needs the rewritten code in mods/, which the app does not produce yet, so Play is off until it is there. */
    private fun showPorted() {
        val games = File(filesDir, "data").listFiles { f -> File(f, "ExeFS").isDirectory }?.sortedBy { it.name } ?: return
        if (games.isEmpty()) return
        list.addView(TextView(this).apply { text = "Extracted games"; textSize = 18f; setPadding(0, dp(8), 0, 0) })
        for (g in games) {
            val ready = File(g, "mods").isDirectory
            val nm = try { JSONObject(Core.nativeGames(File(filesDir, "games").path, "", false)).getJSONArray("games").let { a -> (0 until a.length()).map { a.getJSONObject(it) }.firstOrNull { it.optString("id").equals(g.name, true) }?.optString("name") } } catch (_: Exception) { null }
            list.addView(TextView(this).apply { text = (nm ?: g.name) + if (ready) "" else "\nextracted, but its code is not rewritten (see the log)"; setPadding(0, dp(8), 0, 0) })
            list.addView(button("Play") { startActivity(Intent(this@MainActivity, PlayActivity::class.java).putExtra(PlayActivity.EXTRA_GAME, g.name)) }.apply { isEnabled = ready })
        }
    }

    private class Dump(val uri: Uri, val name: String, val mb: Long) {
        /** The title ID in the file name, as dumpers write it: [0100…]. */
        val id: Long? = Regex("\\[([0-9A-Fa-f]{16})]").find(name)?.groupValues?.get(1)?.toULong(16)?.toLong()
        val version: Long = Regex("\\[v(\\d+)]", RegexOption.IGNORE_CASE).find(name)?.groupValues?.get(1)?.toLongOrNull() ?: -1
        /** 0 base, 1 update, 2 add-on content (base …000, update …800, add-on content base + 0x1000 + n) */
        val role: Int get() = when (val low = (id ?: 0L) and 0xFFFL) { 0L -> 0; 0x800L -> 1; else -> 2 }
        val app: Long get() = (id ?: 0L) and 0x1FFFL.inv()
    }

    /** The dumps in the folder, grouped by game: one button ports the base with its newest update and all its add-on content. */
    private fun scan(tree: Uri) {
        val id = DocumentsContract.getTreeDocumentId(tree)
        val kids = DocumentsContract.buildChildDocumentsUriUsingTree(tree, id)
        val cols = arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_SIZE)
        val dumps = ArrayList<Dump>()
        try {
            contentResolver.query(kids, cols, null, null, null)?.use { c ->
                while (c.moveToNext()) {
                    val name = c.getString(1) ?: continue
                    if (!Regex(".*\\.(nsp|nsz|xci|xcz)$", RegexOption.IGNORE_CASE).matches(name)) continue
                    dumps.add(Dump(DocumentsContract.buildDocumentUriUsingTree(tree, c.getString(0)), name, c.getLong(2) shr 20))
                }
            }
        } catch (e: Exception) {
            list.addView(TextView(this).apply { text = "Cannot read the folder: ${e.message}" })
        }
        if (dumps.isEmpty()) {
            list.addView(TextView(this).apply { text = "No .nsp, .nsz, .xci or .xcz files in that folder (subfolders are not searched yet)." })
            return
        }
        val used = HashSet<Dump>()
        val byApp = dumps.filter { it.id != null }.groupBy { it.app }
        for ((_, g) in byApp.toSortedMap()) {
            val base = g.firstOrNull { it.role == 0 } ?: continue
            val update = g.filter { it.role == 1 }.maxWithOrNull(compareBy({ it.version }, { it.name }))
            val dlc = g.filter { it.role == 2 }.sortedBy { it.name }
            used.addAll(g)
            val what = listOfNotNull(if (update != null) "update" + (if (update.version >= 0) " v${update.version}" else "") else null, if (dlc.isNotEmpty()) "${dlc.size} add-on" else null)
            list.addView(TextView(this).apply { text = "${base.name} (${base.mb} MB)" + if (what.isEmpty()) "" else "\n+ " + what.joinToString(", "); setPadding(0, dp(12), 0, 0) })
            list.addView(button("Port" + if (what.isEmpty()) "" else " with " + what.joinToString(" and ")) {
                PortService.start(this@MainActivity, base.uri, base.name, update?.uri, update?.name ?: "", dlc.map { it.uri to it.name })
            })
            if (update != null) list.addView(button("Port the base only") { PortService.start(this@MainActivity, base.uri, base.name, null, "") })
        }
        // files whose name carries no title ID, or an update / add-on with no base beside it: one at a time, as before
        for (d in dumps.filter { it !in used }) {
            list.addView(TextView(this).apply { text = "${d.name} (${d.mb} MB)"; setPadding(0, dp(12), 0, 0) })
            val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
            row.addView(button("Port") { PortService.start(this@MainActivity, d.uri, d.name, null, "") })
            row.addView(button("Port with an update…") { pendingBase = d.uri to d.name; pick(REQ_UPDATE) })
            list.addView(row)
        }
    }

    private fun showSupported() {
        val j = JSONObject(Core.nativeGames(File(filesDir, "games").path, "", false))
        list.removeAllViews()
        if (j.has("error")) {
            list.addView(TextView(this).apply { text = j.getString("error") })
            return
        }
        val arr = j.getJSONArray("games")
        list.addView(TextView(this).apply { text = "${arr.length()} games with an Android profile"; textSize = 18f })
        for (i in 0 until arr.length()) {
            val g = arr.getJSONObject(i)
            list.addView(TextView(this).apply { text = "${g.optString("name")}  [${g.optString("status")}]"; setPadding(0, dp(4), 0, 0) })
        }
    }

    override fun onActivityResult(req: Int, res: Int, data: Intent?) {
        super.onActivityResult(req, res, data)
        val uri = data?.data
        if (res != RESULT_OK || uri == null) return
        when (req) {
            REQ_KEYS -> {
                contentResolver.openInputStream(uri)?.use { i -> File(filesDir, "prod.keys").outputStream().use { o -> i.copyTo(o) } }
                refresh()
            }
            REQ_TREE -> {
                contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                prefs.edit().putString("tree", uri.toString()).apply()
                refresh()
            }
            REQ_UPDATE -> pendingBase?.let { (base, name) ->
                PortService.start(this, base, name, uri, displayName(uri))
            }
        }
    }

    private fun displayName(uri: Uri): String =
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { if (it.moveToFirst()) it.getString(0) else null } ?: "update.nsp"

    /** Shows the running job's state under the info text. */
    private fun tick() {
        val id = Ports.jobId
        if (id != 0) {
            val s = JSONObject(Core.nativeStatus(id))
            val line = when (s.optString("state")) {
                "running" -> "${Ports.title}: ${s.optString("stage")}, ${s.optLong("bytes") shr 20} MB"
                "done" -> "${s.optString("name").ifEmpty { Ports.title }} ${s.optString("display_version")}: extracted (title ${s.optString("title_id")}, ${s.optInt("romfs_files")} RomFS files)"
                "failed" -> "${Ports.title}: failed: ${s.optString("message")}"
                else -> ""
            }
            title = line
            info.visibility = View.VISIBLE
            info.text = info.text.toString().substringBefore("\n\n") + "\n\n" + line
        }
        ui.postDelayed({ tick() }, 1000)
    }

    companion object {
        const val REQ_KEYS = 1
        const val REQ_TREE = 2
        const val REQ_UPDATE = 3
    }
}
