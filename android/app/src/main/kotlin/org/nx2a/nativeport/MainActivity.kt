package org.nx2a.nativeport

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.SharedPreferences
import android.graphics.Typeface
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import android.text.SpannableString
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import java.io.File

/** The app: a header, a banner when an update is ready, four tabs (Setup, Port, Games, Log), all native views in the nx2android phone app's dark design. */
class MainActivity : Activity() {
    private lateinit var prefs: SharedPreferences
    private lateinit var ui: Ui
    private val handler = Handler(Looper.getMainLooper())
    private val tabs = LinkedHashMap<String, Tab>()
    private val labels = LinkedHashMap<String, Pair<TextView, View>>()
    private lateinit var body: FrameLayout
    private lateinit var ver: TextView
    private lateinit var banner: LinearLayout
    private lateinit var bannerText: TextView
    private var current = "setup"
    private var onResult: ((Uri) -> Unit)? = null
    private var update: Updater.Info? = null


    override fun onResume() {
        super.onResume()
        // back from a game: what it captured is translated in on its own
        if (File(filesDir, "prod.keys").isFile) PortService.autoShaders(this)?.let {
            android.widget.Toast.makeText(this, "Updating the shaders of $it (see the Log tab)", android.widget.Toast.LENGTH_LONG).show()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = getSharedPreferences("nx2a", MODE_PRIVATE)
        ui = Ui(this)
        requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        Catalog.install(this)

        window.statusBarColor = Theme.bg
        window.navigationBarColor = Theme.bg
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setBackgroundColor(Theme.bg) }
        root.setOnApplyWindowInsetsListener { v, ins ->
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                val b = ins.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout() or WindowInsets.Type.ime())
                v.setPadding(b.left, b.top, b.right, b.bottom)
                WindowInsets.CONSUMED
            } else {
                @Suppress("DEPRECATION")
                v.setPadding(ins.systemWindowInsetLeft, ins.systemWindowInsetTop, ins.systemWindowInsetRight, ins.systemWindowInsetBottom)
                ins
            }
        }

        // header
        val title = TextView(this).apply {
            text = SpannableString("nx2android").apply { setSpan(ForegroundColorSpan(Theme.red), 0, 3, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE) }
            setTextColor(Theme.fg)
            textSize = 16f
            setTypeface(typeface, Typeface.BOLD)
        }
        ver = ui.sm("v${packageManager.getPackageInfo(packageName, 0).versionName} · ${BuildConfig.COMMIT}")
        val upd = ui.button("Update") { checkUpdate(true) }.apply { textSize = 13f; setPadding(ui.dp(10), ui.dp(7), ui.dp(10), ui.dp(7)); layoutParams = ui.lp(ViewGroup.LayoutParams.WRAP_CONTENT) }
        val header = LinearLayout(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            setPadding(ui.dp(16), ui.dp(12), ui.dp(16), ui.dp(12))
            addView(title)
            addView(ver, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { leftMargin = ui.dp(10) })
            addView(upd)
        }
        root.addView(header)
        root.addView(ui.divider())

        bannerText = ui.text("An update is ready.", 14f)
        banner = LinearLayout(this).apply {
            visibility = View.GONE
            gravity = Gravity.CENTER_VERTICAL
            setBackgroundColor(Theme.bannerBg)
            setPadding(ui.dp(16), ui.dp(10), ui.dp(16), ui.dp(10))
            addView(bannerText, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(ui.button("Install", true) { installUpdate() }.apply { layoutParams = ui.lp(ViewGroup.LayoutParams.WRAP_CONTENT) })
        }
        root.addView(banner)

        // tabs
        tabs["setup"] = SetupTab(this, ui)
        tabs["port"] = PortTab(this, ui)
        tabs["games"] = GamesTab(this, ui)
        tabs["log"] = LogTab(this, ui)
        val nav = LinearLayout(this)
        for ((k, name) in listOf("setup" to "Setup", "port" to "Port", "games" to "Games", "log" to "Log")) {
            val t = TextView(this).apply {
                text = name
                gravity = Gravity.CENTER
                textSize = 15f
                setPadding(0, ui.dp(12), 0, ui.dp(12))
                setOnClickListener { showTab(k) }
            }
            val bar = View(this)
            val cell = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; addView(t); addView(bar, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ui.dp(2))) }
            labels[k] = t to bar
            nav.addView(cell, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
        root.addView(nav)
        root.addView(ui.divider())
        body = FrameLayout(this)
        for ((_, t) in tabs) body.addView(t.view, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        root.addView(body, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        setContentView(root)

        showTab(if (Ports.running) "log" else "setup")
        handler.postDelayed({ checkUpdate(false) }, 2000)
        loop()
    }

    private fun loop() {
        tabs[current]?.tick()
        handler.postDelayed({ loop() }, 700)
    }

    fun showTab(k: String) {
        current = k
        for ((n, t) in tabs) t.view.visibility = if (n == k) View.VISIBLE else View.GONE
        for ((n, p) in labels) {
            p.first.setTextColor(if (n == k) Theme.fg else Theme.mut)
            p.second.setBackgroundColor(if (n == k) Theme.red else android.graphics.Color.TRANSPARENT)
        }
        tabs[k]?.onShow()
    }

    // ---- pickers ----
    fun pickFile(cb: (Uri) -> Unit) {
        onResult = cb
        startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*"), REQ_FILE)
    }

    fun pickTree(cb: (Uri) -> Unit) {
        onResult = cb
        startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION), REQ_TREE)
    }

    override fun onActivityResult(req: Int, res: Int, data: Intent?) {
        super.onActivityResult(req, res, data)
        val uri = data?.data
        if (res == RESULT_OK && uri != null && (req == REQ_FILE || req == REQ_TREE)) onResult?.invoke(uri)
        onResult = null
    }

    fun displayName(uri: Uri): String =
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { if (it.moveToFirst()) it.getString(0) else null } ?: "file"

    /** A dark confirm box; [cancelable] false makes it a plain message. */
    fun ask(msg: String, yes: String = "OK", cancelable: Boolean = true, onYes: () -> Unit) {
        Ui(this).dialog(message = msg, yes = yes, onYes = onYes, no = if (cancelable) "Cancel" else null).setCancelable(cancelable)
    }

    // ---- updates ----
    private fun checkUpdate(manual: Boolean) {
        val last = prefs.getLong("update_checked", 0)
        if (!manual && System.currentTimeMillis() - last < 6 * 3600_000L) return
        if (manual) ver.text = "checking…"
        Thread {
            try {
                val u = Updater.check()
                prefs.edit().putLong("update_checked", System.currentTimeMillis()).apply()
                runOnUiThread {
                    update = u
                    ver.text = "v${packageManager.getPackageInfo(packageName, 0).versionName} · ${BuildConfig.COMMIT}" + if (manual && u == null) " · up to date" else ""
                    if (u != null) {
                        bannerText.text = "An update is ready (build ${u.commit}, ${Catalog.human(u.size)})."
                        banner.visibility = View.VISIBLE
                    }
                }
            } catch (e: Exception) {
                if (manual) runOnUiThread { ver.text = "check failed: ${e.message}" }
            }
        }.start()
    }

    private fun installUpdate() {
        val u = update ?: return
        if (Ports.running) { ask("A port is running. Install the update when it has finished.", "OK", false) {}; return }
        bannerText.text = "Downloading…"
        Thread {
            try {
                val f = File(cacheDir, "update/nx2android.apk")
                Updater.download(u, f) { p -> runOnUiThread { bannerText.text = "Downloading… ${(p * 100).toInt()}%" } }
                runOnUiThread { bannerText.text = "Ready: confirm the installation." }
                Updater.install(this, f) { r -> runOnUiThread { bannerText.text = if (r == "installed") "Updated." else "Update not installed: $r" } }
            } catch (e: Exception) {
                runOnUiThread { bannerText.text = "Update failed: ${e.message}" }
            }
        }.start()
    }

    companion object {
        const val REQ_FILE = 1
        const val REQ_TREE = 2
    }
}
