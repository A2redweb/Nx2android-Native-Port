package org.nx2a.nativeport

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageInstaller
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL

/** Self-update from the repository's `nightly` pre-release (built by CI from every commit): checks which commit the newest APK was built from, downloads it and hands it to the system installer. */
object Updater {
    private const val API = "https://api.github.com/repos/A2redweb/Nx2android-Native-Port/releases/tags/nightly"

    class Info(val commit: String, val url: String, val size: Long)

    /** The newest build when it is not the one that is running; null when this is the newest. Throws when the check fails. */
    fun check(): Info? {
        val c = URL(API).openConnection() as HttpURLConnection
        c.connectTimeout = 15000
        c.readTimeout = 20000
        c.setRequestProperty("Accept", "application/vnd.github+json")
        c.setRequestProperty("User-Agent", "nx2android")
        if (c.responseCode != 200) error("GitHub answered ${c.responseCode}")
        val j = JSONObject(c.inputStream.bufferedReader().readText())
        val body = j.optString("body")
        val asset = j.getJSONArray("assets").let { a -> (0 until a.length()).map { a.getJSONObject(it) }.firstOrNull { it.optString("name").endsWith(".apk") } } ?: error("the release has no APK yet")
        val sha = Regex("[0-9a-f]{40}").find(body)?.value ?: error("the release does not say which commit it was built from")
        if (BuildConfig.COMMIT != "dev" && sha.startsWith(BuildConfig.COMMIT)) return null
        return Info(sha.take(7), asset.getString("browser_download_url"), asset.optLong("size"))
    }

    fun download(info: Info, dst: File, progress: (Double) -> Unit) {
        dst.parentFile?.mkdirs()
        val c = URL(info.url).openConnection() as HttpURLConnection
        c.connectTimeout = 15000
        c.readTimeout = 30000
        c.setRequestProperty("User-Agent", "nx2android")
        if (c.responseCode != 200) error("download answered ${c.responseCode}")
        val total = c.contentLengthLong.takeIf { it > 0 } ?: info.size
        var got = 0L
        c.inputStream.use { i -> dst.outputStream().use { o ->
            val b = ByteArray(1 shl 16)
            while (true) {
                val n = i.read(b)
                if (n < 0) break
                o.write(b, 0, n)
                got += n
                if (total > 0) progress(got.toDouble() / total)
            }
        } }
    }

    /** Gives the APK to the system installer; the player confirms there (the first time, Android also asks to allow installs from this app). */
    fun install(ctx: Context, apk: File, done: (String) -> Unit) {
        val installer = ctx.packageManager.packageInstaller
        val id = installer.createSession(PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL))
        val action = "${ctx.packageName}.INSTALL_RESULT"
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(c: Context, i: Intent) {
                when (i.getIntExtra(PackageInstaller.EXTRA_STATUS, -1)) {
                    PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                        @Suppress("DEPRECATION")
                        val next = i.getParcelableExtra<Intent>(Intent.EXTRA_INTENT)
                        if (next != null) c.startActivity(next.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                    }
                    PackageInstaller.STATUS_SUCCESS -> { done("installed"); try { c.unregisterReceiver(this) } catch (_: Exception) {} }
                    else -> { done(i.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "the installer refused the update"); try { c.unregisterReceiver(this) } catch (_: Exception) {} }
                }
            }
        }
        ctx.registerReceiver(receiver, IntentFilter(action), Context.RECEIVER_NOT_EXPORTED)
        installer.openSession(id).use { s ->
            s.openWrite("nx2android.apk", 0, apk.length()).use { o ->
                apk.inputStream().use { it.copyTo(o) }
                s.fsync(o)
            }
            val pending = PendingIntent.getBroadcast(ctx, id, Intent(action).setPackage(ctx.packageName), PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
            s.commit(pending.intentSender)
        }
    }
}
