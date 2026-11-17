package org.nx2a.nativeport

import android.app.Activity
import android.app.Dialog
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.CheckBox
import android.widget.LinearLayout
import android.widget.TextView
import java.io.File

/** The file manager of the Port tab: looks through the app's private storage (where the games and their working files are) and deletes what the player does not need. */
class FileManager(private val a: MainActivity, private val ui: Ui) {
    private var cur: File = a.filesDir
    private val dlg = Dialog(a, android.R.style.Theme_Material_NoActionBar)
    private val places = LinearLayout(a)
    private val path = ui.sm()
    private val list = ui.column()
    private val msg = ui.sm()
    private val boxes = ArrayList<Pair<CheckBox, File>>()

    private val roots: List<Pair<String, File>> = listOfNotNull(
        "App storage" to a.filesDir,
        "Games" to File(a.filesDir, "data"),
        "Cache" to a.cacheDir,
        a.getExternalFilesDir(null)?.let { "Shared files" to it },
    )

    fun open() {
        val card = ui.card()
        places.orientation = LinearLayout.HORIZONTAL
        val hs = android.widget.HorizontalScrollView(a).apply { addView(places) }
        card.addView(hs)
        card.addView(path, ui.lp(top = 8, bottom = 4))
        card.addView(list)
        card.addView(ui.row(8,
            ui.button("Select all") { all() }, ui.button("Delete selected", true) { del() }, ui.button("Close") { dlg.dismiss() }))
        card.addView(msg, ui.lp(top = 8))
        val root = ui.column(14).apply { setBackgroundColor(Theme.bg); addView(card) }
        dlg.setContentView(ui.scroll(root))
        dlg.window?.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
        dlg.window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        dlg.window?.statusBarColor = Theme.bg
        show(cur)
        dlg.show()
    }

    private fun inside(f: File) = roots.any { f.canonicalPath == it.second.canonicalPath || f.canonicalPath.startsWith(it.second.canonicalPath + "/") }

    private fun show(dir: File) {
        cur = if (inside(dir)) dir else a.filesDir
        places.removeAllViews()
        for ((n, f) in roots) places.addView(ui.button(n) { show(f) }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { rightMargin = ui.dp(6) })
        path.text = cur.path
        msg.text = ""
        list.removeAllViews()
        boxes.clear()
        val up = cur.parentFile
        if (up != null && inside(up)) list.addView(rowOf("⬆ ..", "") { show(up) }, ui.lp())
        val kids = cur.listFiles()?.sortedWith(compareBy({ !it.isDirectory }, { it.name.lowercase() })) ?: emptyList()
        if (kids.isEmpty()) list.addView(ui.sm("(empty)"), ui.lp(top = 8))
        for (k in kids) {
            val cb = CheckBox(a).apply { buttonTintList = android.content.res.ColorStateList.valueOf(Theme.red) }
            boxes.add(cb to k)
            val name = TextView(a).apply {
                text = (if (k.isDirectory) "📁 " else "📄 ") + k.name
                setTextColor(Theme.fg)
                if (k.isDirectory) setOnClickListener { show(k) }
            }
            val size = ui.sm(if (k.isFile) Catalog.human(k.length()) else "")
            val row = LinearLayout(a).apply {
                gravity = android.view.Gravity.CENTER_VERTICAL
                setPadding(0, ui.dp(4), 0, ui.dp(4))
                addView(cb)
                addView(name, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                addView(size)
            }
            list.addView(row, ui.lp())
            list.addView(ui.divider())
        }
    }

    private fun rowOf(label: String, size: String, onClick: () -> Unit) = LinearLayout(a).apply {
        setPadding(ui.dp(4), ui.dp(10), 0, ui.dp(10))
        addView(TextView(a).apply { text = label; setTextColor(Theme.fg) }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        addView(ui.sm(size))
        setOnClickListener { onClick() }
    }

    private fun all() {
        val on = boxes.any { !it.first.isChecked }
        boxes.forEach { it.first.isChecked = on }
    }

    private fun del() {
        val sel = boxes.filter { it.first.isChecked }.map { it.second }
        if (sel.isEmpty()) { msg.text = "Nothing selected"; return }
        a.ask("Delete ${sel.size} item(s) for good?\n\n" + sel.take(8).joinToString("\n") { it.name } + if (sel.size > 8) "\n…" else "", "Delete") {
            msg.text = "deleting…"
            Thread {
                val bad = sel.filter { !it.deleteRecursively() }
                a.runOnUiThread {
                    show(cur)
                    msg.text = "${sel.size - bad.size} deleted" + if (bad.isEmpty()) "" else "; could not delete " + bad.joinToString { it.name }
                }
            }.start()
        }
    }
}
