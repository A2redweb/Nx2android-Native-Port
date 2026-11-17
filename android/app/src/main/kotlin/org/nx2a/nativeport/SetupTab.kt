package org.nx2a.nativeport

import android.app.ActivityManager
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.PowerManager
import android.os.StatFs
import android.provider.Settings
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import org.json.JSONObject
import java.io.File

/** Setup: what the app needs and whether it has it, and your prod.keys. Nothing to download: the porter, the translator and the profiles are all in the app. */
class SetupTab(private val a: MainActivity, private val ui: Ui) : Tab {
    private class Check(val name: String, val ok: Boolean, val required: Boolean, val detail: String, val fix: String = "")

    private val checks = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL }
    private val keysField = ui.input("prod.keys from your own Switch").apply { isEnabled = false }
    private val keysMsg = ui.sm()
    override val view: View

    init {
        val c1 = ui.card()
        c1.addView(checks)
        c1.addView(ui.row(8, ui.button("Re-check") { refresh() }, ui.button("Allow background work") { allowBackground() }))
        c1.addView(ui.sm("Everything the app needs is built in: the Rust porter, the shader translator and the profiles of ${Catalog.games(a).size} games. There is nothing to download first. Keep about 6 GB free plus the size of each game (a compressed .nsz or .xcz is read as it is, with no big temporary copy)."), ui.lp(top = 8))
        val c2 = ui.card()
        c2.addView(ui.label("prod.keys (from your own Switch)"))
        c2.addView(ui.fieldRow(keysField, ui.button("Browse") { chooseKeys() }.apply { (layoutParams as LinearLayout.LayoutParams).topMargin = 0 }))
        c2.addView(keysMsg, ui.lp(top = 8))
        c2.addView(ui.sm("The keys are copied into the app's private folder, which only this app can read. They never leave the phone."), ui.lp(top = 8))
        val col = ui.column(14)
        col.addView(c1)
        col.addView(c2)
        view = ui.scroll(col)
        refresh()
    }

    override fun onShow() = refresh()

    private fun chooseKeys() {
        a.pickFile { uri ->
            try {
                val dst = File(a.filesDir, "prod.keys")
                a.contentResolver.openInputStream(uri)?.use { i -> dst.outputStream().use { o -> i.copyTo(o) } } ?: error("cannot read the file")
                val j = JSONObject(Core.nativeKeys(dst.path))
                if (j.has("error")) { dst.delete(); keysMsg.text = j.getString("error") } else keysMsg.text = "saved"
            } catch (e: Exception) { keysMsg.text = "could not save: ${e.message}" }
            refresh()
        }
    }

    private fun allowBackground() {
        try { a.startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${a.packageName}"))) }
        catch (_: Exception) { a.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)) }
    }

    private fun doctor(): List<Check> {
        val out = ArrayList<Check>()
        val keys = File(a.filesDir, "prod.keys")
        if (!keys.exists()) out.add(Check("prod.keys", false, true, "not chosen yet", "choose your prod.keys below"))
        else {
            val j = JSONObject(Core.nativeKeys(keys.path))
            if (j.has("error")) out.add(Check("prod.keys", false, true, j.getString("error"), "choose the file again"))
            else if (!j.getBoolean("header_key")) out.add(Check("prod.keys", false, true, "${j.getInt("keys")} keys, but header_key is missing", "dump the keys again with a current Lockpick_RCM"))
            else out.add(Check("prod.keys", true, true, "${j.getInt("keys")} keys (${j.getInt("master_keys")} master keys)"))
        }
        val n = Catalog.games(a).size
        val err = Catalog.error(a)
        out.add(Check("Game profiles", n > 0, true, if (n > 0) "$n games with an Android profile" else (err ?: "none found"), "reinstall the app"))
        val dmhost = File(a.applicationInfo.nativeLibraryDir, "libdmhost.so")
        out.add(Check("Game host", dmhost.isFile, true, if (dmhost.isFile) "libdmhost.so, ${Catalog.human(dmhost.length())}" else "libdmhost.so is missing", "reinstall the app"))
        val xl = File(a.applicationInfo.nativeLibraryDir, "libdmsh_xlate.so")
        out.add(Check("Shader translator", xl.isFile && xl.canExecute(), true, if (xl.isFile) "libdmsh_xlate.so, ${Catalog.human(xl.length())}" else "not part of this build", "update the app: the shaders of many games cannot be built without it"))
        // Android's feature flag is what the phone maker declared and often says 1.1 for a driver that offers 1.3: the game host checks the real driver when a game starts
        val pm = a.packageManager
        val vk = listOf(0x00403000 to "1.3", 0x00402000 to "1.2", 0x00401000 to "1.1").firstOrNull { pm.hasSystemFeature(PackageManager.FEATURE_VULKAN_HARDWARE_VERSION, it.first) }?.second
        out.add(Check("Vulkan", vk != null, true, when (vk) { null -> "this phone does not report Vulkan"; "1.3" -> "the phone reports Vulkan 1.3"; else -> "the phone reports Vulkan $vk (the games need a 1.3 driver; the declared level often lags the real one, so the host checks it when a game starts)" }, "the games need a Vulkan GPU driver"))
        val free = StatFs(a.filesDir.path).availableBytes
        out.add(Check("Free space", free >= 6_000_000_000L, false, Catalog.human(free) + " free", "about 6 GB plus the size of each game"))
        val mi = ActivityManager.MemoryInfo()
        a.getSystemService(ActivityManager::class.java).getMemoryInfo(mi)
        out.add(Check("Memory", mi.totalMem >= 7_000_000_000L, false, Catalog.human(mi.totalMem) + " of RAM", "most games want 8 GB or more"))
        val bat = a.getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(a.packageName)
        out.add(Check("Background work", bat, false, if (bat) "the app is not battery-restricted" else "battery use is optimised: a long port may be stopped", "tap Allow background work (or set the app's battery usage to Unrestricted)"))
        return out
    }

    private fun refresh() {
        checks.removeAllViews()
        for (c in doctor()) {
            val body = LinearLayout(a).apply { orientation = LinearLayout.VERTICAL }
            body.addView(ui.text(c.name, 15f, Theme.fg, true))
            body.addView(ui.sm(c.detail + if (!c.ok && c.fix.isNotEmpty()) "\n→ ${c.fix}" else ""))
            val row = LinearLayout(a).apply { setPadding(0, ui.dp(6), 0, ui.dp(6)) }
            row.addView(ui.dot(if (c.ok) Theme.ok else if (c.required) Theme.red else Theme.opt))
            row.addView(body, LinearLayout.LayoutParams(0, android.view.ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            checks.addView(row)
            checks.addView(ui.divider())
        }
        val f = File(a.filesDir, "prod.keys")
        keysField.setText(if (f.exists()) "saved in the app (${Catalog.human(f.length())})" else "")
    }
}
