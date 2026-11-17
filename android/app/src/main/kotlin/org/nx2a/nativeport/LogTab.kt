package org.nx2a.nativeport

import android.content.ClipData
import android.content.ClipboardManager
import android.view.View
import android.widget.ScrollView
import android.widget.TextView

/** Log: the running port's stage, progress and log; Cancel and Copy log. */
class LogTab(private val a: MainActivity, private val ui: Ui) : Tab {
    private val stage = ui.sm("idle")
    private val bar = Ui.Bar(ui)
    private val out = ui.mono()
    private val scroll: ScrollView
    private var shown = 0
    override val view: View

    init {
        val card = ui.card()
        card.addView(stage)
        card.addView(bar.view)
        card.addView(ui.row(8, ui.button("Cancel") { if (Ports.running) Ports.cancel() }, ui.button("Copy log") { copy() }))
        val col = ui.column(14)
        col.addView(card)
        col.addView(out, ui.lp(h = android.view.ViewGroup.LayoutParams.WRAP_CONTENT))
        scroll = ui.scroll(col)
        view = scroll
        tick()
    }

    override fun onShow() = tick()

    private fun copy() {
        a.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("nx2android log", Ports.text()))
        stage.text = stage.text.toString().substringBefore(" · log copied") + " · log copied"
    }

    override fun tick() {
        if (Ports.lineCount() < shown) { out.text = ""; shown = 0 }
        val more = Ports.linesFrom(shown)
        if (more.isNotEmpty()) {
            out.append((if (out.text.isEmpty()) "" else "\n") + more.joinToString("\n"))
            shown += more.size
            scroll.post { scroll.fullScroll(View.FOCUS_DOWN) }
        }
        stage.text = Ports.stage + if (Ports.running && Ports.frac > 0) "  ${(Ports.frac * 100).toInt()}%" else ""
        bar.set(Ports.frac)
    }
}
