package org.nx2a.nativeport

import android.content.Context
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.*

/** The look of the app: the dark theme of the nx2android phone app (colours, cards, red buttons, tabs with a red underline), drawn natively. */
object Theme {
    val bg = Color.parseColor("#0d0d10")
    val card = Color.parseColor("#17171c")
    val line = Color.parseColor("#2a2a33")
    val fg = Color.parseColor("#ececf1")
    val mut = Color.parseColor("#9a9aa8")
    val red = Color.parseColor("#e5383b")
    val ok = Color.parseColor("#3ecf8e")
    val field = Color.parseColor("#101014")
    val grey = Color.parseColor("#2a2a33")
    val opt = Color.parseColor("#777777")
    val code = Color.parseColor("#08080a")
    val bannerBg = Color.parseColor("#3a1517")
}

class Ui(val c: Context) {
    val d = c.resources.displayMetrics.density
    fun dp(v: Int) = (v * d).toInt()

    fun shape(fill: Int, radius: Int, stroke: Int = 0) = GradientDrawable().apply {
        setColor(fill)
        cornerRadius = radius * d
        if (stroke != 0) setStroke(maxOf(1, d.toInt()), stroke)
    }

    fun lp(w: Int = ViewGroup.LayoutParams.MATCH_PARENT, h: Int = ViewGroup.LayoutParams.WRAP_CONTENT, top: Int = 0, bottom: Int = 0, weight: Float = 0f) =
        LinearLayout.LayoutParams(w, h, weight).apply { topMargin = dp(top); bottomMargin = dp(bottom) }

    fun column(pad: Int = 0) = LinearLayout(c).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(pad), dp(pad), dp(pad), dp(pad)) }

    fun row(gap: Int = 8, vararg views: View): LinearLayout = LinearLayout(c).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        clipChildren = false; clipToPadding = false
        views.forEachIndexed { i, v -> addView(v, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { topMargin = dp(8); if (i > 0) leftMargin = dp(gap) }) }
    }

    /** A row whose children keep their own width, the last one stretching (a field with its Browse button) */
    fun fieldRow(field: View, button: View): LinearLayout = LinearLayout(c).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        addView(field, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        addView(button, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { leftMargin = dp(8) })
    }

    fun card(): LinearLayout = column(12).apply {
        background = shape(Theme.card, 10, Theme.line)
        layoutParams = lp(bottom = 12)
    }

    fun text(s: CharSequence, size: Float = 15f, color: Int = Theme.fg, bold: Boolean = false) = TextView(c).apply {
        text = s
        setTextColor(color)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, size)
        if (bold) setTypeface(typeface, Typeface.BOLD)
    }

    /** the small grey text of the app */
    fun sm(s: CharSequence = "") = text(s, 12f, Theme.mut)

    fun label(s: String) = text(s, 13f, Theme.mut).apply { layoutParams = lp(top = 8, bottom = 4) }

    fun input(hint: String = "", number: Boolean = false) = EditText(c).apply {
        this.hint = hint
        setHintTextColor(Theme.opt)
        setTextColor(Theme.fg)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        background = shape(Theme.field, 8, Theme.line)
        setPadding(dp(10), dp(10), dp(10), dp(10))
        setSingleLine()
        if (number) inputType = InputType.TYPE_CLASS_NUMBER
    }

    /** primary = the red button, otherwise the grey one */
    fun button(label: String, primary: Boolean = false, onClick: () -> Unit) = Button(c).apply {
        text = label
        isAllCaps = false
        setTextColor(if (primary) Color.WHITE else Theme.fg)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        setTypeface(typeface, if (primary) Typeface.BOLD else Typeface.NORMAL)
        background = shape(if (primary) Theme.red else Theme.grey, 8)
        stateListAnimator = null
        minHeight = 0
        minimumHeight = dp(46)
        maxLines = 1
        ellipsize = android.text.TextUtils.TruncateAt.END
        setPadding(dp(14), dp(11), dp(14), dp(11))
        setOnClickListener { onClick() }
        layoutParams = lp(top = 8)
    }

    fun check(label: String, checked: Boolean = false, onChange: (Boolean) -> Unit = {}): CheckBox = CheckBox(c).apply {
        text = label
        isChecked = checked
        setTextColor(Theme.fg)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        buttonTintList = android.content.res.ColorStateList.valueOf(Theme.red)
        setOnCheckedChangeListener { _, v -> onChange(v) }
        layoutParams = lp(top = 4)
    }

    fun dot(color: Int) = View(c).apply {
        background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(color) }
        layoutParams = LinearLayout.LayoutParams(dp(10), dp(10)).apply { topMargin = dp(5); rightMargin = dp(8) }
    }

    fun mono(s: CharSequence = "") = TextView(c).apply {
        text = s
        typeface = Typeface.MONOSPACE
        setTextColor(Theme.fg)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
        setTextIsSelectable(true)
        background = shape(Theme.code, 8, Theme.line)
        setPadding(dp(10), dp(10), dp(10), dp(10))
    }

    fun divider() = View(c).apply { setBackgroundColor(Theme.line); layoutParams = lp(h = maxOf(1, d.toInt())) }

    /** the thin red progress bar */
    class Bar(val ui: Ui) {
        private val fill = View(ui.c).apply { setBackgroundColor(Theme.red) }
        val view = FrameLayout(ui.c).apply {
            background = ui.shape(Theme.field, 6)
            addView(fill, FrameLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT))
            layoutParams = ui.lp(h = ui.dp(8), top = 8, bottom = 8)
        }
        fun set(frac: Double) {
            view.post {
                val w = (view.width * frac.coerceIn(0.0, 1.0)).toInt()
                fill.layoutParams = FrameLayout.LayoutParams(w, ViewGroup.LayoutParams.MATCH_PARENT)
            }
        }
    }

    fun scroll(content: View) = ScrollView(c).apply {
        isFillViewport = false
        addView(content, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
    }

    /**
     * A box in the app's own look (dark card, red primary button), not the system's: a [title], a [message], a pick list ([items], [selected], picking one calls [onPick] and closes),
     * and buttons: [yes] (red) and [no] (grey; null leaves it out).
     */
    fun dialog(title: String? = null, message: String? = null, items: List<String> = emptyList(), selected: Int = -1, onPick: (Int) -> Unit = {},
               yes: String? = null, onYes: () -> Unit = {}, no: String? = "Cancel"): android.app.Dialog {
        val dlg = android.app.Dialog(c)
        dlg.requestWindowFeature(android.view.Window.FEATURE_NO_TITLE)
        val box = column(16).apply { background = shape(Theme.card, 14, Theme.line) }
        if (title != null) box.addView(text(title, 17f, Theme.fg, true), lp(bottom = 8))
        if (message != null) box.addView(text(message, 14f, Theme.fg), lp(bottom = 8))
        items.forEachIndexed { i, label ->
            val on = i == selected
            box.addView(TextView(c).apply {
                text = label
                setTextColor(Theme.fg)
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
                setPadding(dp(12), dp(12), dp(12), dp(12))
                background = shape(Theme.field, 8, if (on) Theme.red else Theme.line)
                setOnClickListener { dlg.dismiss(); onPick(i) }
            }, lp(top = 6))
        }
        val buttons = LinearLayout(c).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.END }
        if (no != null) buttons.addView(button(no) { dlg.dismiss() }.apply { layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { topMargin = dp(8); marginEnd = if (yes != null) dp(8) else 0 } })
        if (yes != null) buttons.addView(button(yes, true) { dlg.dismiss(); onYes() }.apply { layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { topMargin = dp(8) } })
        box.addView(buttons, lp(top = 8))
        dlg.setContentView(box)
        dlg.window?.apply {
            setBackgroundDrawable(android.graphics.drawable.ColorDrawable(Color.TRANSPARENT))
            setLayout((c.resources.displayMetrics.widthPixels * 0.88f).toInt().coerceAtMost(dp(460)), ViewGroup.LayoutParams.WRAP_CONTENT)
            setDimAmount(0.6f)
        }
        dlg.show()
        return dlg
    }
}
