package org.nx2a.nativeport

import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import android.view.View
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import java.io.File

/** Port: the folder with your dumps, the games found in it (base, newest update, add-on content), the options, and the Port button. */
class PortTab(private val a: MainActivity, private val ui: Ui) : Tab {
    private class Dump(val uri: Uri, val name: String, val bytes: Long) {
        /** the title ID in the file name, as dumpers write it: [0100…] */
        var id: Long? = Regex("\\[([0-9A-Fa-f]{16})]").find(name)?.groupValues?.get(1)?.toULong(16)?.toLong()
        val version: Long = Regex("\\[v(\\d+)]", RegexOption.IGNORE_CASE).find(name)?.groupValues?.get(1)?.toLongOrNull() ?: -1
        /** 0 base, 1 update, 2 add-on content (base …000, update …800, add-on content base + 0x1000 + n) */
        val role: Int get() = when ((id ?: 0L) and 0xFFFL) { 0L -> 0; 0x800L -> 1; else -> 2 }
        val app: Long get() = (id ?: 0L) and 0x1FFFL.inv()
        val mb: Long get() = bytes shr 20
    }

    private class Entry(val base: Dump, val update: Dump?, val dlc: List<Dump>, val profile: Catalog.Game?)

    private val prefs = a.getSharedPreferences("nx2a", android.content.Context.MODE_PRIVATE)
    private val folder = ui.input("Pick the folder that holds your games").apply { isEnabled = false }
    private val insp = ui.sm()
    private val picks = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL }
    private val appName = ui.input("")
    private val touch = ui.check("Touch game (gamepad hidden)", prefs.getBoolean("opt_touch", false))
    private val fps60 = ui.check("60 fps", prefs.getBoolean("opt_fps60", false))
    private val noMenu = ui.check("No on-screen menu button (Back opens the pause menu)", prefs.getBoolean("opt_nomenu", false))
    private val mtReplay = ui.check("Multithreaded graphics replay (for a game that is slow on one thread)", prefs.getBoolean("opt_mt", false))
    private val jobs = ui.input("", number = true).apply { setText(prefs.getInt("opt_jobs", 3).toString()) }
    private val extra = ui.input("Optional: a separate update file").apply { isEnabled = false }
    private var extraUri: Uri? = null
    private var extraName = ""
    private val workInfo = ui.sm("Working files of earlier or stopped ports take space in the private storage of this app.")
    private val driverBox = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL }
    private val fontName = ui.input("The phone's own font").apply { isEnabled = false }
    private var entries: List<Entry> = emptyList()
    private var selected = -1
    private var loose: List<Dump> = emptyList()
    override val view: View

    init {
        val c1 = ui.card()
        c1.addView(ui.label("Game folder (base game, update and DLC: NSP / XCI / NSZ / XCZ)"))
        c1.addView(ui.fieldRow(folder, ui.button("Browse") { pickFolder() }.apply { (layoutParams as LinearLayout.LayoutParams).topMargin = 0 }))
        c1.addView(ui.button("Inspect") { inspect() })
        c1.addView(insp, ui.lp(top = 8))
        c1.addView(picks)

        val c2 = ui.card()
        c2.addView(ui.label("App name (optional)"))
        c2.addView(appName)
        c2.addView(touch)
        c2.addView(fps60)
        c2.addView(noMenu)
        c2.addView(mtReplay)
        c2.addView(ui.sm("60 fps lifts the frame-rate cap when the game's profile has one; otherwise the community's FPSLocker config of your game's build is downloaded (needs internet) and applied when it can be."), ui.lp(bottom = 4))
        c2.addView(disabled("Experimental (game not ported yet, may not start)", "Every game with a profile is already ported. A game without a profile is extracted only."))
        c2.addView(ui.label("Update file (optional): used instead of an update found in the folder"))
        c2.addView(ui.fieldRow(extra, ui.button("Browse") { pickExtra() }.apply { (layoutParams as LinearLayout.LayoutParams).topMargin = 0 }))
        c2.addView(ui.label("Vulkan driver for the games (a custom Turnip / Mesa build, .so or .zip)"))
        c2.addView(driverBox)
        c2.addView(ui.button("Add driver") { addDriver() })
        c2.addView(ui.label("System font (optional .ttf for games that draw Chinese or Korean text with the console's font)"))
        c2.addView(ui.fieldRow(fontName, ui.button("Choose") { pickFont() }.apply { (layoutParams as LinearLayout.LayoutParams).topMargin = 0 }))
        c2.addView(ui.button("Use the phone's font") { File(a.filesDir, "shared_font.ttf").delete(); showFont() })
        c2.addView(ui.label("Build jobs (lower uses less RAM)"))
        c2.addView(jobs)
        c2.addView(ui.button("Port", true) { port() })

        val c3 = ui.card()
        c3.addView(ui.text("Files", 15f, Theme.fg, true))
        c3.addView(ui.sm("Look through the private storage of this app (where the games are kept) and delete what you do not need."))
        c3.addView(ui.button("Open file manager") { FileManager(a, ui).open() })

        val c4 = ui.card()
        c4.addView(ui.text("Free space", 15f, Theme.fg, true))
        c4.addView(workInfo)
        c4.addView(ui.row(8, ui.button("Check") { checkWork() }, ui.button("Delete working files", true) { cleanWork() }))

        val col = ui.column(14)
        col.addView(c1); col.addView(c2); col.addView(c3); col.addView(c4)
        view = ui.scroll(col)
        renderDrivers(); showFont()
        prefs.getString("tree", null)?.let { folder.setText(Uri.parse(it).lastPathSegment ?: it); inspect() }
    }

    private fun disabled(label: String, why: String): View {
        val box = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL; alpha = 0.55f }
        box.addView(CheckBox(a).apply { text = label; isEnabled = false; setTextColor(Theme.fg); buttonTintList = android.content.res.ColorStateList.valueOf(Theme.opt) })
        box.addView(ui.sm(why))
        return box
    }

    private fun renderDrivers() {
        driverBox.removeAllViews()
        val cur = Drivers.chosen(a)?.id
        fun row(label: String, id: String?) {
            val line = LinearLayout(a).apply { orientation = LinearLayout.HORIZONTAL; gravity = android.view.Gravity.CENTER_VERTICAL }
            line.addView(ui.check(label, id == cur) { on -> if (on) { Drivers.choose(a, id); renderDrivers() } }.apply { layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f) })
            if (id != null) line.addView(ui.button("Remove") { Drivers.remove(a, id); renderDrivers() }.apply { layoutParams = LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT) })
            driverBox.addView(line)
        }
        row("The phone's own driver", null)
        Drivers.list(a).forEach { row(it.id, it.id) }
        driverBox.addView(ui.sm("A driver that does not load, or has no Vulkan 1.3 GPU, is skipped and the game uses the phone's own; the Log shows which was used."))
    }

    private fun showFont() { fontName.setText(if (File(a.filesDir, "shared_font.ttf").isFile) prefs.getString("font_name", "your font") else "") }

    private fun pickFont() {
        a.pickFile { uri ->
            val name = a.displayName(uri)
            if (!name.endsWith(".ttf", true)) { insp.text = "Choose a .ttf font (one font, not a collection)."; return@pickFile }
            Thread {
                try {
                    a.contentResolver.openInputStream(uri)!!.use { i -> File(a.filesDir, "shared_font.ttf").outputStream().use { i.copyTo(it) } }
                    prefs.edit().putString("font_name", name).apply()
                } catch (e: Exception) { a.runOnUiThread { insp.text = "Could not read the font: ${e.message}" } }
                a.runOnUiThread { showFont() }
            }.start()
        }
    }

    private fun addDriver() {
        a.pickFile { uri ->
            val name = a.displayName(uri)
            Thread {
                val msg = try { "Added " + Drivers.add(a, uri, name) } catch (e: Exception) { "Could not add the driver: ${e.message}" }
                a.runOnUiThread { renderDrivers(); insp.text = msg }
            }.start()
        }
    }

    private fun pickFolder() {
        a.pickTree { uri ->
            a.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            prefs.edit().putString("tree", uri.toString()).apply()
            folder.setText(uri.lastPathSegment ?: uri.toString())
            inspect()
        }
    }

    private fun pickExtra() {
        a.pickFile { uri ->
            extraUri = uri
            extraName = a.displayName(uri)
            extra.setText(extraName)
        }
    }

    /** Reads the folder and groups what is in it by game. */
    private fun inspect() {
        val tree = prefs.getString("tree", null)?.let { Uri.parse(it) }
        if (tree == null) { insp.text = "Choose the folder first."; return }
        insp.text = "reading…"
        Thread {
            val dumps = ArrayList<Dump>()
            var problem = ""
            try {
                val id = DocumentsContract.getTreeDocumentId(tree)
                val kids = DocumentsContract.buildChildDocumentsUriUsingTree(tree, id)
                val cols = arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_SIZE)
                a.contentResolver.query(kids, cols, null, null, null)?.use { c ->
                    while (c.moveToNext()) {
                        val name = c.getString(1) ?: continue
                        if (!Regex(".*\\.(nsp|nsz|xci|xcz)$", RegexOption.IGNORE_CASE).matches(name)) continue
                        dumps.add(Dump(DocumentsContract.buildDocumentUriUsingTree(tree, c.getString(0)), name, c.getLong(2)))
                    }
                }
            } catch (e: Exception) { problem = "Cannot read the folder: ${e.message}" }
            // a file whose name has no [title ID] (or a wrong one): the title is read from the file itself (tickets, content meta headers)
            val keys = File(a.filesDir, "prod.keys")
            if (keys.isFile) for (d in dumps) {
                try {
                    a.contentResolver.openFileDescriptor(d.uri, "r")?.use { pfd ->
                        val t = Core.nativeTitle(keys.path, "/proc/self/fd/${pfd.fd}", d.name)
                        if (t.isNotEmpty()) d.id = t.toULong(16).toLong()
                    }
                } catch (_: Exception) {}
            }
            val names = Catalog.byId(a)
            val found = ArrayList<Entry>()
            val used = HashSet<Dump>()
            for ((_, g) in dumps.filter { it.id != null }.groupBy { it.app }.toSortedMap()) {
                val base = g.firstOrNull { it.role == 0 } ?: continue
                used.addAll(g)
                found.add(Entry(base, g.filter { it.role == 1 }.maxWithOrNull(compareBy({ it.version }, { it.name })), g.filter { it.role == 2 }.sortedBy { it.name }, names[String.format("%016X", base.app)]))
            }
            // files whose name carries no title ID: each one a game of its own (the player can name an update file below)
            for (d in dumps.filter { it !in used }) found.add(Entry(d, null, emptyList(), null))
            a.runOnUiThread {
                entries = found
                selected = if (found.size == 1) 0 else -1
                insp.text = if (problem.isNotEmpty()) problem else if (found.isEmpty()) "No .nsp, .nsz, .xci or .xcz files in that folder (subfolders are not searched)." else "${found.size} game(s) found" + if (found.size > 1) ": tap the one to port" else ""
                render()
            }
        }.start()
    }

    private fun describe(e: Entry): String {
        val b = StringBuilder(e.profile?.name ?: e.base.name)
        b.append("\n").append(e.base.name).append(" (").append(e.base.mb).append(" MB)")
        if (e.update != null) b.append("\n+ update").append(if (e.update.version >= 0) " v${e.update.version}" else "").append(" (").append(e.update.mb).append(" MB)")
        if (e.dlc.isNotEmpty()) b.append("\n+ ").append(e.dlc.size).append(" add-on")
        b.append("\n").append(when {
            e.profile == null && e.base.id == null -> "the file name has no title ID: the game is found out when it is read"
            e.profile == null -> "no profile in this app: it can be extracted, but not started yet"
            else -> "profile: ${e.profile.status}"
        })
        return b.toString()
    }

    private fun render() {
        picks.removeAllViews()
        entries.forEachIndexed { i, e ->
            val on = i == selected
            val t = TextView(a).apply {
                text = describe(e)
                setTextColor(Theme.fg)
                textSize = 13f
                setPadding(ui.dp(10), ui.dp(8), ui.dp(10), ui.dp(8))
                background = ui.shape(Theme.field, 8, if (on) Theme.red else Theme.line)
                setOnClickListener { selected = i; render() }
            }
            picks.addView(t, ui.lp(top = 8))
        }
    }

    private fun options(): PortOptions {
        val j = (jobs.text.toString().toIntOrNull() ?: 3).coerceIn(1, 8)
        prefs.edit().putBoolean("opt_touch", touch.isChecked).putBoolean("opt_fps60", fps60.isChecked).putBoolean("opt_nomenu", noMenu.isChecked).putBoolean("opt_mt", mtReplay.isChecked).putInt("opt_jobs", j).apply()
        return PortOptions(appName.text.toString(), touch.isChecked, fps60.isChecked, j, noMenu.isChecked, mtReplay.isChecked)
    }

    private fun port() {
        if (Ports.running) { a.ask("A port is already running. See the Log tab.", "OK", false) {}; return }
        if (!File(a.filesDir, "prod.keys").isFile) { a.ask("Choose your prod.keys first (Setup tab).", "OK", false) {}; return }
        if (selected < 0 || selected >= entries.size) { a.ask(if (entries.isEmpty()) "Choose the folder with your games first." else "Tap the game to port in the list above.", "OK", false) {}; return }
        val e = entries[selected]
        val upd = extraUri ?: e.update?.uri
        val updName = if (extraUri != null) extraName else e.update?.name ?: ""
        PortService.start(a, e.base.uri, e.base.name, upd, updName, e.dlc.map { it.uri to it.name }, options())
        a.showTab("log")
    }

    private fun leftovers(): List<File> = listOf(File(a.filesDir, "ports"), File(a.cacheDir, "decompressed"))

    private fun checkWork() {
        Thread {
            val bytes = leftovers().sumOf { Catalog.size(it) }
            a.runOnUiThread { workInfo.text = if (bytes == 0L) "No leftover working files." else "${Catalog.human(bytes)} of working files from earlier or stopped ports" }
        }.start()
    }

    private fun cleanWork() {
        if (Ports.running) { workInfo.text = "A port is running: wait for it to end."; return }
        a.ask("Delete the working files of earlier and stopped ports? The ported games in the Games tab stay.", "Delete") {
            workInfo.text = "deleting…"
            Thread {
                val before = leftovers().sumOf { Catalog.size(it) }
                leftovers().forEach { it.deleteRecursively() }
                a.runOnUiThread { workInfo.text = "Freed ${Catalog.human(before)}" }
            }.start()
        }
    }
}
