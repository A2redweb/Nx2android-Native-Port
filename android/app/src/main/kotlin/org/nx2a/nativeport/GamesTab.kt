package org.nx2a.nativeport

import android.content.Intent
import android.graphics.BitmapFactory
import android.view.View
import android.widget.ImageView
import android.widget.LinearLayout
import java.io.File

/** Games: what has been ported, with Play and Delete, and the list of games this app has a profile for. */
class GamesTab(private val a: MainActivity, private val ui: Ui) : Tab {
    private val list = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL }
    private val supported = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL }
    private var supportedOpen = false
    override val view: View

    init {
        val col = ui.column(14)
        col.addView(list)
        val c = ui.card()
        val games = Catalog.games(a)
        c.addView(ui.text("Supported games", 15f, Theme.fg, true))
        c.addView(ui.sm("${games.size} games have a profile in this app (the games upstream's porter supports)."))
        c.addView(ui.button("Show the list") {
            supportedOpen = !supportedOpen
            fillSupported(games)
        })
        c.addView(supported)
        col.addView(c)
        view = ui.scroll(col)
        refresh()
    }

    override fun onShow() = refresh()

    private fun fillSupported(games: List<Catalog.Game>) {
        supported.removeAllViews()
        if (!supportedOpen) return
        for (g in games.sortedBy { it.name.lowercase() }) {
            supported.addView(ui.sm("${g.name}  [${g.status}]"), ui.lp(top = 4))
        }
    }

    private fun first(f: File): String = f.takeIf { it.exists() }?.readLines()?.firstOrNull { it.isNotBlank() }?.take(160) ?: ""

    fun refresh() {
        list.removeAllViews()
        val dirs = File(a.filesDir, "data").listFiles { f -> File(f, "ExeFS").isDirectory }?.sortedBy { it.name } ?: emptyList()
        if (dirs.isEmpty()) {
            val c = ui.card()
            c.addView(ui.text("No ported games yet", 15f, Theme.fg, true))
            c.addView(ui.sm("Port a game from the Port tab; it appears here, ready to play."))
            list.addView(c)
            return
        }
        val names = Catalog.byId(a)
        for (g in dirs) {
            val ready = File(g, "mods").isDirectory
            val shown = File(g, "name.txt").takeIf { it.exists() }?.readText()?.trim()?.ifEmpty { null } ?: names[g.name.uppercase()]?.name ?: g.name
            val c = ui.card()
            val head = LinearLayout(a).apply { gravity = android.view.Gravity.CENTER_VERTICAL }
            val icon = File(g, "icon.jpg")
            if (icon.exists()) {
                val bmp = try { BitmapFactory.decodeFile(icon.path, BitmapFactory.Options().apply { inSampleSize = 2 }) } catch (_: Exception) { null }
                if (bmp != null) head.addView(ImageView(a).apply { setImageBitmap(bmp); scaleType = ImageView.ScaleType.CENTER_CROP; clipToOutline = true; background = ui.shape(Theme.field, 8) }, LinearLayout.LayoutParams(ui.dp(56), ui.dp(56)).apply { rightMargin = ui.dp(12) })
            }
            val body = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL }
            body.addView(ui.text(shown, 16f, Theme.fg, true))
            body.addView(ui.sm(g.name + (names[g.name.uppercase()]?.let { "  [${it.status}]" } ?: "")))
            head.addView(body, LinearLayout.LayoutParams(0, android.view.ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            c.addView(head)
            val rw = first(File(g, "rewrite.txt"))
            val sh = first(File(g, "shaders.txt"))
            val notes = ArrayList<String>()
            if (!ready) notes.add("Not playable yet: the game's code is not rewritten" + if (rw.isNotEmpty()) " ($rw)" else " (see the Log tab)")
            else if (rw.isNotEmpty()) notes.add("Code: $rw")
            if (sh.isNotEmpty()) notes.add("Shaders: $sh")
            if (File(g, "aoc").isDirectory) notes.add("Add-on content: ${File(g, "aoc").list()?.size ?: 0}")
            c.addView(ui.sm(notes.joinToString("\n") + (if (notes.isEmpty()) "" else "\n") + Catalog.human(Catalog.size(g)) + " on this phone"), ui.lp(top = 8))
            c.addView(ui.row(8,
                ui.button("Play", true) { a.startActivity(Intent(a, PlayActivity::class.java).putExtra(PlayActivity.EXTRA_GAME, g.name)) }.also { it.isEnabled = ready; it.alpha = if (ready) 1f else 0.4f },
                ui.button("Delete") {
                    a.ask("Delete $shown from this phone? Its saves are deleted too. Port it again from your dump any time.", "Delete") {
                        Thread { g.deleteRecursively(); a.runOnUiThread { refresh() } }.start()
                    }
                }))
            list.addView(c)
        }
    }
}
