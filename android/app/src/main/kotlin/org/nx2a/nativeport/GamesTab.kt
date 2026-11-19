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

    private fun icon(g: File): android.graphics.Bitmap? {
        val icon = File(g, "icon.jpg")
        return if (icon.exists()) try { BitmapFactory.decodeFile(icon.path, BitmapFactory.Options().apply { inSampleSize = 2 }) } catch (_: Exception) { null } else null
    }

    private fun shownName(g: File, names: Map<String, Catalog.Game>): String =
        File(g, "name.txt").takeIf { it.exists() }?.readText()?.trim()?.ifEmpty { null } ?: names[g.name.uppercase()]?.name ?: g.name

    /** the ported games: only their icons, three to a row; a tap opens the game's sheet */
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
        val cols = 3
        val gap = ui.dp(10)
        val side = ((a.resources.displayMetrics.widthPixels - ui.dp(28) - gap * (cols - 1)) / cols)
        for (chunk in dirs.chunked(cols)) {
            val row = LinearLayout(a).apply { orientation = LinearLayout.HORIZONTAL }
            for ((k, g) in chunk.withIndex()) {
                val shown = shownName(g, names)
                val bmp = icon(g)
                val tile: View = if (bmp != null) ImageView(a).apply {
                    setImageBitmap(bmp); scaleType = ImageView.ScaleType.CENTER_CROP; clipToOutline = true
                    background = ui.shape(Theme.field, 14, Theme.line)
                    outlineProvider = object : android.view.ViewOutlineProvider() {
                        override fun getOutline(v: View, o: android.graphics.Outline) { o.setRoundRect(0, 0, v.width, v.height, 14 * ui.d) }
                    }
                } else ui.text(shown, 13f, Theme.fg, true).apply { gravity = android.view.Gravity.CENTER; setPadding(ui.dp(6), ui.dp(6), ui.dp(6), ui.dp(6)); background = ui.shape(Theme.card, 14, Theme.line) }
                tile.contentDescription = shown
                tile.setOnClickListener { sheet(g, shown, names) }
                row.addView(tile, LinearLayout.LayoutParams(side, side).apply { if (k > 0) leftMargin = gap })
            }
            list.addView(row, ui.lp(bottom = 10))
        }
    }

    /** one action of the sheet: its button and what it does */
    private fun action(col: LinearLayout, label: String, what: String, primary: Boolean = false, enabled: Boolean = true, onClick: () -> Unit) {
        col.addView(ui.button(label, primary, onClick).also { it.isEnabled = enabled; it.alpha = if (enabled) 1f else 0.4f })
        col.addView(ui.sm(what), ui.lp(top = 4))
    }

    /** a game's sheet: what the game is, each action with its description, and the cheats with their switches */
    private fun sheet(g: File, shown: String, names: Map<String, Catalog.Game>) {
        val dlg = android.app.Dialog(a)
        dlg.requestWindowFeature(android.view.Window.FEATURE_NO_TITLE)
        val box = ui.column(16).apply { background = ui.shape(Theme.card, 14, Theme.line) }
        val sc = ui.scroll(box)
        fun build() {
            box.removeAllViews()
            val ready = File(g, "mods").isDirectory
            val head = LinearLayout(a).apply { gravity = android.view.Gravity.CENTER_VERTICAL }
            icon(g)?.let { head.addView(ImageView(a).apply { setImageBitmap(it); scaleType = ImageView.ScaleType.CENTER_CROP; clipToOutline = true; background = ui.shape(Theme.field, 8) }, LinearLayout.LayoutParams(ui.dp(64), ui.dp(64)).apply { rightMargin = ui.dp(12) }) }
            val body = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL }
            body.addView(ui.text(shown, 17f, Theme.fg, true))
            body.addView(ui.sm(g.name + (names[g.name.uppercase()]?.let { "  [${it.status}]" } ?: "")))
            head.addView(body, LinearLayout.LayoutParams(0, android.view.ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            box.addView(head)
            val rw = first(File(g, "rewrite.txt")); val sh = first(File(g, "shaders.txt"))
            val notes = ArrayList<String>()
            if (!ready) notes.add("Not playable yet: the game's code is not rewritten" + if (rw.isNotEmpty()) " ($rw)" else " (see the Log tab)")
            else if (rw.isNotEmpty()) notes.add("Code: $rw")
            if (sh.isNotEmpty()) notes.add("Shaders: $sh")
            if (File(g, "aoc").isDirectory) notes.add("Add-on content: ${File(g, "aoc").list()?.size ?: 0}")
            box.addView(ui.sm(notes.joinToString("\n") + (if (notes.isEmpty()) "" else "\n") + Catalog.human(Catalog.size(g)) + " on this phone"), ui.lp(top = 8, bottom = 4))
            action(box, "Play", "Starts the game. Its pause menu (the menu button on screen) has the gamepad, mode, saves and a Cheats tab.", true, ready) {
                dlg.dismiss(); a.startActivity(Intent(a, PlayActivity::class.java).putExtra(PlayActivity.EXTRA_GAME, g.name))
            }
            action(box, "Driver", "Which Vulkan driver draws this game: the phone's own, or a custom one you added in the Port tab. A custom driver fixes many crashes on Adreno.") { pickDriver(g.name, shown) }
            action(box, "Saves", "Import a save from Nx2android/exported saves/$shown, or from any folder. Saves are exported from the game's pause menu.") { saves(shown) }
            action(box, "Update shaders", "Some shaders are in no game file: the game saves them as it meets them. This translates those in, so the next start draws what was left out. It takes a few minutes.") { updateShaders(g, shown) }
            action(box, "Extra settings", "For testing: host switches as KEY=VALUE lines (for example DM_FPS_TARGET=30), applied the next time the game starts. Leave empty for the normal setup.") { extraSettings(g, shown) }
            action(box, "Log link", "Uploads this game's log to a public paste site and copies the link, so anyone you send it to can read what went wrong. It holds no keys.") { shareLog(g, shown) }
            action(box, "Delete", "Removes the game from this phone, with its saves. Port it again from your dump any time.") {
                a.ask("Delete $shown from this phone? Its saves are deleted too. Port it again from your dump any time.", "Delete") {
                    dlg.dismiss(); Thread { g.deleteRecursively(); a.runOnUiThread { refresh() } }.start()
                }
            }
            box.addView(ui.divider(), ui.lp(top = 14, bottom = 8))
            box.addView(ui.text("Cheats", 16f, Theme.fg, true))
            val cs = Cheats.list(g)
            box.addView(ui.sm(if (cs.isEmpty()) "Downloads the community's cheats for your version of the game. Switch the ones you want on here; they run while you play, and the pause menu's Cheats tab switches them too." else "${cs.count { !it.master }} cheats. On means the game applies it while you play."), ui.lp(top = 4))
            val status = ui.sm("")
            box.addView(ui.button(if (cs.isEmpty()) "Get cheats" else "Update cheats") {
                status.text = "Downloading…"
                Thread {
                    val r = try { Cheats.fetch(a, g, g.name, cs.isNotEmpty()) } catch (e: Exception) { "Failed: ${e.message}" }
                    a.runOnUiThread { build(); box.addView(ui.sm(r), ui.lp(top = 6)) }
                }.start()
            })
            box.addView(status, ui.lp(top = 4))
            for (c in cs.filter { !it.master }) {
                box.addView(ui.check(c.name, c.on) { v -> Cheats.setOn(g, c.name, v) })
            }
            box.addView(ui.button("Close") { dlg.dismiss() }, ui.lp(top = 14))
        }
        build()
        dlg.setContentView(sc)
        dlg.window?.apply {
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(android.graphics.Color.TRANSPARENT))
            val m = a.resources.displayMetrics
            setLayout((m.widthPixels * 0.92f).toInt().coerceAtMost(ui.dp(480)), (m.heightPixels * 0.88f).toInt())
            setDimAmount(0.6f)
        }
        dlg.show()
    }

    private fun extraSettings(g: File, shown: String) {
        val f = File(g, "env_user.txt")
        val dlg = android.app.Dialog(a)
        dlg.requestWindowFeature(android.view.Window.FEATURE_NO_TITLE)
        val box = ui.column(16).apply { background = ui.shape(Theme.card, 14, Theme.line) }
        box.addView(ui.text("Extra settings for $shown", 17f, Theme.fg, true), ui.lp(bottom = 8))
        val et = ui.input("KEY=VALUE, one per line").apply { setSingleLine(false); minLines = 4; gravity = android.view.Gravity.TOP; setText(if (f.isFile) f.readText() else "") }
        box.addView(et)
        box.addView(ui.row(8, ui.button("Cancel") { dlg.dismiss() }, ui.button("Save", true) {
            val t = et.text.toString().trim()
            if (t.isEmpty()) f.delete() else f.writeText(t + "\n")
            dlg.dismiss()
        }))
        dlg.setContentView(box)
        dlg.window?.apply {
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(android.graphics.Color.TRANSPARENT))
            setLayout((a.resources.displayMetrics.widthPixels * 0.9f).toInt().coerceAtMost(ui.dp(460)), android.view.ViewGroup.LayoutParams.WRAP_CONTENT)
            setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        }
        dlg.show()
    }

    private fun pickDriver(game: String, shown: String) {
        val drivers = Drivers.list(a)
        val ids = listOf<String?>(null, "system") + drivers.map { it.id }
        val labels = listOf("As for all games (Port tab)", "The phone's own driver") + drivers.map { it.id }
        val cur = ids.indexOf(Drivers.ownChoice(a, game)).coerceAtLeast(0)
        ui.dialog(title = "Vulkan driver for $shown", items = labels, selected = cur, onPick = { i -> Drivers.chooseFor(a, game, ids[i]) })
    }

    /** some of a game's shaders are in no game file: the game saves them the first time it needs them, and this translates them in */
    private fun updateShaders(g: File, shown: String) {
        val n = File(g, "captured_shaders").list()?.size ?: 0
        a.ask("Play $shown first; the game saves shaders it was missing as it meets them ($n saved so far). This translates them in, so the next start draws what was left out. It takes a few minutes; progress is in the Log tab.", "Update") {
            PortService.shaders(a, g.name)
            a.showTab("log")
        }
    }

    /** the game's log, uploaded; the link is copied */
    private fun shareLog(g: File, shown: String) {
        ui.dialog(title = "Copy a link to this game's log", message = "The log of $shown (its port and its output) is uploaded to a public paste site, and the link is copied. Anyone with the link can read it. It holds no keys, only what the app printed.",
            yes = "Upload", onYes = {
                Thread {
                    val r: Any = try { LogShare.upload(LogShare.gameLog(g, shown, a.getExternalFilesDir(null))) } catch (e: Exception) { Pair("", e.message) }
                    a.runOnUiThread {
                        if (r is String) {
                            a.getSystemService(android.content.ClipboardManager::class.java).setPrimaryClip(android.content.ClipData.newPlainText("log link", r))
                            ui.dialog(title = "Link copied", message = r, yes = "OK", no = null)
                        } else ui.dialog(title = "Upload failed", message = ((r as Pair<*, *>).second as? String) ?: "no network?", yes = "OK", no = null)
                    }
                }.start()
            })
    }

    /** the saves in Nx2android/exported saves/<game>: import one, or pick any folder to import from */
    private fun saves(shown: String) {
        if (!Saves.access()) {
            ui.dialog(title = "Saves of $shown", message = "Saves are exported to Nx2android/exported saves/$shown on your phone, where you can open them. Allow \"All files access\" for this app to use that folder (no root or Shizuku). Until then they stay in the app's own folder.",
                yes = "Allow access", onYes = { Saves.requestAccess(a) }, no = "Close")
            return
        }
        val dir = Saves.dirFor(shown)
        val found = Saves.list(dir)
        val fmt = java.text.SimpleDateFormat("d MMM yyyy HH:mm", java.util.Locale.getDefault())
        ui.dialog(title = "Saves of $shown",
            message = (if (found.isEmpty()) "No saves in ${dir.path} yet. Export one from the game's pause menu, or pick a save folder." else "Pick a save to import, or choose another folder. ${dir.path}"),
            items = found.map { "${it.name}${if (it.device) " (+device)" else ""} · ${fmt.format(java.util.Date(it.time))} · ${it.files} files, ${Catalog.human(it.bytes)}" },
            onPick = { i -> confirmImport(dir, found[i].name) },
            yes = "Select folder…", onYes = { pickSaveFolder(dir) }, no = "Close")
    }

    private fun confirmImport(dir: File, name: String) {
        a.ask("Import $name? The game's current save is backed up first. It is applied the next time you start the game.", "Import") {
            try { Saves.requestImport(dir, name) ; ui.dialog(message = "Done: $name will be imported when you start the game.", yes = "OK", no = null) }
            catch (e: Exception) { ui.dialog(message = "Could not set up the import: ${e.message}", yes = "OK", no = null) }
        }
    }

    private fun pickSaveFolder(dir: File) {
        a.pickTree { uri ->
            Thread {
                val stamp = java.text.SimpleDateFormat("yyyyMMdd-HHmmss", java.util.Locale.US).format(java.util.Date())
                val dst = File(dir, "$stamp-picked")
                val n = try { Saves.copyTree(a.contentResolver, uri, android.provider.DocumentsContract.getTreeDocumentId(uri), dst) } catch (e: Exception) { dst.deleteRecursively(); -1 }
                a.runOnUiThread {
                    if (n <= 0) { dst.deleteRecursively(); ui.dialog(message = "That folder has no files (or could not be read).", yes = "OK", no = null) }
                    else confirmImport(dir, dst.name)
                }
            }.start()
        }
    }
}
