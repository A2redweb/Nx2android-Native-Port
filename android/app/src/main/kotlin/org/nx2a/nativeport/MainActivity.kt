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
        if (tree != null) scan(Uri.parse(tree))
    }

    private fun scan(tree: Uri) {
        val id = DocumentsContract.getTreeDocumentId(tree)
        val kids = DocumentsContract.buildChildDocumentsUriUsingTree(tree, id)
        val cols = arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_SIZE)
        var n = 0
        try {
            contentResolver.query(kids, cols, null, null, null)?.use { c ->
                while (c.moveToNext()) {
                    val name = c.getString(1) ?: continue
                    if (!Regex(".*\\.(nsp|nsz|xci|xcz)$", RegexOption.IGNORE_CASE).matches(name)) continue
                    val uri = DocumentsContract.buildDocumentUriUsingTree(tree, c.getString(0))
                    val mb = c.getLong(2) shr 20
                    n++
                    list.addView(TextView(this).apply { text = "$name ($mb MB)"; setPadding(0, dp(12), 0, 0) })
                    val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
                    row.addView(button("Port") { PortService.start(this@MainActivity, uri, name, null, "") })
                    row.addView(button("Port with an update…") { pendingBase = uri to name; pick(REQ_UPDATE) })
                    list.addView(row)
                }
            }
        } catch (e: Exception) {
            list.addView(TextView(this).apply { text = "Cannot read the folder: ${e.message}" })
        }
        if (n == 0) list.addView(TextView(this).apply { text = "No .nsp, .nsz, .xci or .xcz files in that folder (subfolders are not searched yet)." })
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
                "done" -> "${Ports.title}: extracted (title ${s.optString("title_id")}, ${s.optInt("romfs_files")} RomFS files)"
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
