package org.nx2a.nativeport

import android.os.Build
import java.io.File
import java.net.HttpURLConnection
import java.net.URL

/** A game's log as a link: the port's log and the game's own output, uploaded to a public paste site so anyone with the link can read it. */
object LogShare {
    private const val MAX = 400_000

    fun gameLog(dir: File, shown: String, ext: File?): String {
        val sb = StringBuilder()
        sb.append("nx2android native · build ").append(BuildConfig.COMMIT).append('\n')
        sb.append("game: ").append(shown).append(" (").append(dir.name).append(")\n")
        sb.append("device: ").append(Build.MANUFACTURER).append(' ').append(Build.MODEL).append(", Android ").append(Build.VERSION.RELEASE).append('\n')
        sb.append("SoC: ").append(if (Build.VERSION.SDK_INT >= 31) "${Build.SOC_MANUFACTURER} ${Build.SOC_MODEL}" else Build.HARDWARE).append(", board ").append(Build.BOARD).append(", ").append(Build.FINGERPRINT).append('\n')
        for (f in listOf("env.txt", "env_user.txt")) File(dir, f).takeIf { it.isFile }?.let { sb.append("\n== ").append(f).append(" ==\n").append(it.readText().trim()).append('\n') }
        // the lines that say what the graphics ran on and what the host complained about, from the whole log (its start, which the tail below can cut, holds the GPU and driver)
        File(dir, "log.txt").takeIf { it.isFile }?.let { f ->
            val key = Regex("GPU |DM_VK_DRIVER|Vulkan [0-9]|driver|\\[RED\\]|not in the bundle|skipped \\(no|features|custom driver|memory pools in the last|longframe")
            val seen = LinkedHashSet<String>()
            val out = StringBuilder()
            for (l in f.readLines()) if (key.containsMatchIn(l)) {
                val k = l.replace(Regex("[0-9a-f]{16}"), "#").replace(Regex("[0-9.]+"), "0")
                if (seen.size < 300 && seen.add(k)) out.append(l.take(300)).append('\n')
            }
            sb.append("\n== key lines of the game's log (the GPU and driver, every red line, skipped draws; repeats left out) ==\n").append(out.toString().ifBlank { "(none: the game has not run since the app was installed)\n" })
        }
        for (f in listOf("rewrite.txt", "shaders.txt")) File(dir, f).takeIf { it.isFile }?.let { sb.append("\n== ").append(f).append(" ==\n").append(it.readText().trim()).append('\n') }
        File(dir, "port.log").takeIf { it.isFile }?.let { sb.append("\n== port log ==\n").append(it.readText().trim()).append('\n') }
        val fmt = java.text.SimpleDateFormat("d MMM HH:mm:ss", java.util.Locale.US)
        for (f in listOf("log.txt", "log.prev.txt")) File(dir, f).takeIf { it.isFile }?.let {   // the host's own log: the last two runs of any game
            sb.append("\n== game output: ").append(f).append(" (written ").append(fmt.format(java.util.Date(it.lastModified()))).append(") ==\n").append(it.readText().trim().takeLast(120_000)).append('\n')
        }
        // the shaders a crash was in (the host names them), so the crash can be looked at without the game
        File(dir, "log.txt").takeIf { it.isFile }?.let { f ->
            val keys = Regex("graphics pipeline, shaders ([0-9a-f]{16}) ([0-9a-f]{16}) ([0-9a-f]{16})|compute pipeline, shader ([0-9a-f]{16})").findAll(f.readText()).flatMap { m -> m.groupValues.drop(1).filter { it.isNotEmpty() } }.distinct().take(6).toList()
            if (keys.isNotEmpty()) try { sb.append("\n== shaders of the crash (SPIR-V, base64) ==\n").append(Core.nativeShaderDump(dir.path, keys.joinToString(" "))) } catch (e: Throwable) { sb.append("(shader dump failed: ${e.message})\n") }
        }
        sb.append("\n== system log, crash lines only ==\n").append(logcat())
        return if (sb.length > MAX) sb.substring(0, 20_000) + "\n…cut…\n" + sb.substring(sb.length - (MAX - 20_000)) else sb.toString()
    }

    private fun logcat(): String = try {
        val p = ProcessBuilder("logcat", "-d", "-t", "4000").redirectErrorStream(true).start()
        val t = p.inputStream.bufferedReader().readText()
        p.waitFor()
        val keep = Regex("Fatal signal|SIGSEGV|SIGABRT|AndroidRuntime|FATAL|dmhost|libdm|nx2a|backtrace|#\\d\\d pc ")
        t.lineSequence().filter { keep.containsMatchIn(it) }.joinToString("\n").takeLast(60_000).ifBlank { "(none)" }
    } catch (e: Exception) { "(logcat not readable: ${e.message})" }

    /** uploads [text] and returns the link; tries paste.rs, then dpaste.org */
    fun upload(text: String): String {
        val errs = ArrayList<String>()
        try { return post("https://paste.rs", text.toByteArray(), "text/plain; charset=utf-8").trim().also { require(it.startsWith("http")) { it.take(80) } } } catch (e: Exception) { errs.add("paste.rs: ${e.message}") }
        try {
            val body = "content=" + java.net.URLEncoder.encode(text, "UTF-8") + "&syntax=text&expiry_days=30"
            val u = post("https://dpaste.org/api/", body.toByteArray(), "application/x-www-form-urlencoded").trim().trim('"')
            require(u.startsWith("http")) { u.take(80) }
            return if (u.endsWith("/raw")) u else "$u/raw"
        } catch (e: Exception) { errs.add("dpaste.org: ${e.message}") }
        throw Exception(errs.joinToString("; "))
    }

    private fun post(url: String, body: ByteArray, type: String): String {
        val c = URL(url).openConnection() as HttpURLConnection
        c.requestMethod = "POST"; c.doOutput = true; c.connectTimeout = 15000; c.readTimeout = 30000
        c.setRequestProperty("Content-Type", type); c.setRequestProperty("User-Agent", "nx2android-native")
        c.outputStream.use { it.write(body) }
        val code = c.responseCode
        val txt = (if (code < 400) c.inputStream else c.errorStream)?.bufferedReader()?.readText() ?: ""
        if (code >= 400) throw Exception("HTTP $code")
        return txt
    }
}
