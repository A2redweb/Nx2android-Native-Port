package org.nx2a.nativeport

import android.app.ActivityManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import org.json.JSONObject
import java.io.File

/**
 * Runs one port as a foreground service, so it survives the screen turning off: extract the dumps, write the game's switch files, rewrite its code, build its shaders.
 * The work itself is the Rust core's; this walks the four stages, keeps [Ports] (what the Log tab shows) and the notification up to date.
 */
class PortService : Service() {
    /** Translator processes: the player's choice, at most the cores, and at most one per ~1.5 GB of memory the system says is free: a process killed for memory loses the whole port. */
    private fun translatorThreads(want: Int): Int {
        val mi = ActivityManager.MemoryInfo()
        getSystemService(ActivityManager::class.java).getMemoryInfo(mi)
        val byMem = (mi.availMem / (1536L shl 20)).toInt()
        return minOf(want, Runtime.getRuntime().availableProcessors(), byMem).coerceAtLeast(1)
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        intent?.getStringExtra("shadersOf")?.let { return rebuildShaders(it, intent.getIntExtra("jobs", 3), startId) }
        val base = intent?.getStringExtra("base") ?: return stop(startId)
        if (Ports.running) {
            Ports.say("A port is already running.")
            return stop(startId)
        }
        val baseName = intent.getStringExtra("baseName") ?: "game.nsp"
        val update = intent.getStringExtra("update") ?: ""
        val updateName = intent.getStringExtra("updateName") ?: ""
        val dlc = intent.getStringArrayListExtra("dlc") ?: arrayListOf()
        val dlcNames = intent.getStringArrayListExtra("dlcNames") ?: arrayListOf()
        val opt = PortOptions(intent.getStringExtra("appName") ?: "", intent.getBooleanExtra("touch", false), intent.getBooleanExtra("fps60", false), intent.getIntExtra("jobs", 3), intent.getBooleanExtra("noMenu", false), intent.getBooleanExtra("mtReplay", false))

        createChannel()
        // how the last process ended (a kill for memory shows here), for the log a bug report needs
        try {
            getSystemService(ActivityManager::class.java).getHistoricalProcessExitReasons(packageName, 0, 3).forEach {
                android.util.Log.i("nx2a", "previous process ${it.processName}: reason=${it.reason} (${it.description}) rss=${it.rss shr 20} MB")
            }
        } catch (_: Exception) {}
        startForeground(1, notification("Starting…"), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        val stem = baseName.substringBeforeLast('.')
        Ports.begin(stem)

        val wake = getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nx2a:port").apply { setReferenceCounted(false); acquire(6 * 60 * 60 * 1000L) }
        Thread {
            val fds = ArrayList<ParcelFileDescriptor>()
            val out = File(filesDir, "ports/$stem").also { it.mkdirs() }
            var ok = false
            var gameDir: File? = null
            try {
                Ports.say("Port of $baseName" + (if (updateName.isNotEmpty()) " with $updateName" else "") + (if (dlc.isNotEmpty()) " and ${dlc.size} add-on dump(s)" else ""))
                if (!File(filesDir, "prod.keys").isFile) throw Failure("prod.keys is not set: open the Setup tab and choose it")
                val bfd = contentResolver.openFileDescriptor(Uri.parse(base), "r") ?: throw Failure("cannot open $baseName")
                fds.add(bfd)
                var upath = ""
                if (update.isNotEmpty()) {
                    val ufd = contentResolver.openFileDescriptor(Uri.parse(update), "r") ?: throw Failure("cannot open $updateName")
                    fds.add(ufd)
                    upath = "/proc/self/fd/${ufd.fd}"
                }
                val dlcLines = StringBuilder()
                dlc.forEachIndexed { i, u ->
                    val d = contentResolver.openFileDescriptor(Uri.parse(u), "r") ?: throw Failure("cannot open ${dlcNames.getOrNull(i)}")
                    fds.add(d)
                    dlcLines.append("/proc/self/fd/${d.fd}\t${dlcNames.getOrNull(i) ?: "dlc.nsp"}\n")
                }
                val titleId = extract(bfd, baseName, upath, updateName, out, dlcLines.toString())
                val id = install(out, titleId, stem, opt)
                val dst = File(filesDir, "data/$id")
                gameDir = dst
                step(2, 3, "Rewrite the game's code", notify = stem) {
                    File(dst, "mods").deleteRecursively()   // an older port's code must not outlive a failed rewrite
                    val rw = Core.nativeRewrite(File(filesDir, "games").path, id, dst.path)
                    File(dst, "rewrite.txt").writeText(rw + "\n")
                    rw.split("; ").filter { it.contains("[RED]") }.forEach { Ports.say(it.substringAfter("ok: ")) }
                    rw
                }
                step(3, 3, "Build the shaders", notify = stem) {
                    File(dst, "shaders.dmsv").delete()
                    val xlate = File(applicationInfo.nativeLibraryDir, "libdmsh_xlate.so")
                    val sh = if (xlate.isFile) Core.nativeBuildShaders(File(filesDir, "games").path, id, dst.path, xlate.path, translatorThreads(opt.jobs))
                             else "error: the shader translator is not part of this build"
                    File(dst, "shaders.txt").writeText(sh + "\n")
                    if (!sh.startsWith("error")) File(dst, "captured.built").writeText("${captured(dst)}\n")
                    sh
                }
                Ports.result = File(dst, "name.txt").takeIf { it.exists() }?.readText()?.trim() ?: stem
                Ports.stage = "finished"
                Ports.say("Done: ${Ports.result}. Open the Games tab to play it.")
                ok = true
            } catch (e: Failure) {
                fail(e.message ?: "failed")
            } catch (e: Exception) {
                android.util.Log.e("nx2a", "port failed", e)
                fail("${e.javaClass.simpleName}: ${e.message}")
            } finally {
                fds.forEach { try { it.close() } catch (_: Exception) {} }
                gameDir?.takeIf { it.isDirectory }?.let { try { File(it, "port.log").writeText(Ports.text()) } catch (_: Exception) {} }
                if (!ok) out.deleteRecursively()
                if (wake.isHeld) wake.release()
                Ports.running = false
                getSystemService(NotificationManager::class.java).notify(1, notification(if (ok) "$stem: done" else "$stem: stopped"))
                stopForeground(STOP_FOREGROUND_DETACH)
                stopSelf(startId)
            }
        }.start()
        return START_NOT_STICKY
    }

    /** Only the shader step, for a game already ported: translates in what the game captured while it ran (captured_shaders/). */
    private fun rebuildShaders(id: String, jobs: Int, startId: Int): Int {
        if (Ports.running) { Ports.say("A port is already running."); return stop(startId) }
        createChannel()
        startForeground(1, notification("Starting…"), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        val dst = File(filesDir, "data/$id")
        val shown = File(dst, "name.txt").takeIf { it.exists() }?.readText()?.trim()?.ifEmpty { null } ?: id
        Ports.begin(shown)
        val wake = getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nx2a:shaders").apply { setReferenceCounted(false); acquire(2 * 60 * 60 * 1000L) }
        Thread {
            var ok = false
            try {
                Ports.say("Shaders of $shown: the game's own and the ones it captured while you played (${File(dst, "captured_shaders").list()?.size ?: 0} files)")
                val xlate = File(applicationInfo.nativeLibraryDir, "libdmsh_xlate.so")
                if (!xlate.isFile) throw Failure("the shader translator is not part of this build")
                val used = captured(dst)
                step(1, 1, "Build the shaders", notify = shown) {
                    val old = File(dst, "shaders.dmsv"); val bak = File(dst, "shaders.dmsv.old")
                    if (old.exists()) old.renameTo(bak)
                    val sh = Core.nativeBuildShaders(File(filesDir, "games").path, id, dst.path, xlate.path, translatorThreads(jobs))
                    if (sh.startsWith("error")) { File(dst, "shaders.dmsv").delete(); bak.renameTo(old) } else bak.delete()   // a failed build keeps the bundle that worked
                    File(dst, "shaders.txt").writeText(sh + "\n")
                    if (!sh.startsWith("error")) { File(dst, "captured.built").writeText("$used\n"); File(dst, "shaders_live.dmsv").delete() }   // the bundle holds them now
                    sh
                }
                Ports.stage = "finished"
                Ports.say("Done. Start the game again to use them; play further and run this again to add more.")
                ok = true
            } catch (e: Failure) { fail(e.message ?: "failed") }
            catch (e: Exception) { android.util.Log.e("nx2a", "shader build failed", e); fail("${e.javaClass.simpleName}: ${e.message}") }
            finally {
                if (wake.isHeld) wake.release()
                Ports.running = false
                getSystemService(NotificationManager::class.java).notify(1, notification(if (ok) "$shown: shaders done" else "$shown: stopped"))
                stopForeground(STOP_FOREGROUND_DETACH)
                stopSelf(startId)
            }
        }.start()
        return START_NOT_STICKY
    }

    private class Failure(m: String) : Exception(m)

    private fun fail(m: String) {
        Ports.stage = if (Ports.cancelled) "cancelled" else "stopped"
        Ports.say("✗ " + if (Ports.cancelled) "cancelled" else m)
    }

    /** Stage 1: the Rust core reads the dumps and writes ExeFS and RomFS (and add-on content) into [out]; returns the title ID. */
    private fun extract(bfd: ParcelFileDescriptor, baseName: String, upath: String, updateName: String, out: File, dlc: String): String {
        Ports.stage(1, 3, "Extract the game")
        val tmp = File(cacheDir, "decompressed")
        val id = Core.nativeExtract(File(filesDir, "prod.keys").path, "/proc/self/fd/${bfd.fd}", baseName, upath, updateName, out.path, tmp.path, dlc)
        Ports.jobId = id
        var lastStage = ""
        while (true) {
            val s = JSONObject(Core.nativeStatus(id))
            val state = s.optString("state")
            val total = s.optLong("total")
            if (total > 0) Ports.frac = s.optLong("bytes").toDouble() / total
            val stage = s.optString("stage")
            if (stage != lastStage && state == "running") { lastStage = stage; Ports.say(stage + "…") }
            Ports.stage = "Stage 1/3: Extract the game — $stage"
            getSystemService(NotificationManager::class.java).notify(1, notification("${Ports.title}: $stage (${(Ports.frac * 100).toInt()}%)"))
            if (state == "done") {
                Ports.end("Extract the game", true, "${s.optString("name")} ${s.optString("display_version")}, ${s.optInt("romfs_files")} RomFS files")
                if (s.optString("message").isNotEmpty()) Ports.say(s.optString("message"))
                return s.optString("title_id")
            }
            if (state == "failed") {
                Ports.end("Extract the game", false)
                throw Failure(s.optString("message"))
            }
            Thread.sleep(500)
        }
    }

    /** The extracted files go to the folder the game screen reads, data/<profile folder name>, with the switch files; the player's options are applied here. */
    private fun install(out: File, titleId: String, stem: String, opt: PortOptions): String {
        Ports.stage = "Stage 1/3: Extract the game — setting up the game's files"
        val games = File(filesDir, "games")
        val id = games.list()?.firstOrNull { it.equals(titleId, ignoreCase = true) } ?: titleId.uppercase()
        val dst = File(filesDir, "data/$id")
        dst.parentFile?.mkdirs()
        if (dst.exists()) { File(dst, "RomFS").deleteRecursively(); File(dst, "ExeFS").deleteRecursively() }
        dst.mkdirs()
        if (File(out, "aoc").isDirectory) File(dst, "aoc").deleteRecursively()
        for (n in listOf("RomFS", "ExeFS", "aoc", "control.nacp", "icon.jpg")) File(out, n).takeIf { it.exists() }?.renameTo(File(dst, n))
        val shown = opt.appName.trim().ifEmpty { File(out, "name.txt").takeIf { it.exists() }?.readText()?.trim() ?: stem }
        File(dst, "name.txt").writeText(shown)
        out.deleteRecursively()
        val err = Core.nativePrepare(games.path, id, dst.path, java.util.Locale.getDefault().toLanguageTag())
        if (err.isNotEmpty()) Ports.say("switch files: $err")
        applyOptions(games, id, dst, opt)
        buttonFont(dst)
        sharedFont(dst)
        if (!File(games, id).isDirectory) Ports.say("No game profile for $id: the files are extracted, but the game has no profile in this build, so it cannot start yet.")
        return id
    }

    private fun applyOptions(games: File, id: String, dst: File, opt: PortOptions) {
        if (opt.touch) {   // the pause menu's first-launch defaults of a game played by touch: the on-screen gamepad starts hidden
            val f = File(dst, "settings.txt")
            val old = if (f.exists()) f.readLines().filterNot { it.startsWith("overlay=") } else emptyList()
            f.writeText((old + "overlay=0").joinToString("\n") + "\n")
            Ports.say("Touch game: the on-screen gamepad starts hidden")
        }
        for ((on, row) in listOf(opt.noMenuButton to "DM_MENU_BUTTON=0", opt.mtReplay to "DM_VK_MT_REPLAY=1")) {
            val key = row.substringBefore('=') + "="
            val env = File(dst, "env.txt")
            val old = if (env.exists()) env.readLines().filterNot { it.startsWith(key) } else emptyList()
            env.writeText((old + if (on) listOf(row) else emptyList()).joinToString("\n") + "\n")
        }
        if (opt.fps60) {
            val cap = File(games, "$id/game.tsv").takeIf { it.exists() }?.readLines()?.firstOrNull { it.startsWith("fps_target\t") }?.substringAfter('\t')?.trim()
            if (cap != null && cap != "0") {
                val f = File(dst, "env.txt")
                val old = if (f.exists()) f.readLines().filterNot { it.startsWith("DM_FPS_TARGET=") } else emptyList()
                f.writeText((old + "DM_FPS_TARGET=60").joinToString("\n") + "\n")
                Ports.say("60 fps: the game profile's own frame-rate cap, at 60")
            } else {
                fpsConfig(id, dst)
            }
        }
    }

    /** The stand-in for the console's Nintendo extension font (controller-button icons in game text), drawn when the app was built; the host reads it through DM_SHARED_FONT_EXT. */
    private fun buttonFont(dst: File) {
        try {
            assets.open("nintendo_ext.ttf").use { i -> File(dst, "nintendo_ext.ttf").outputStream().use { i.copyTo(it) } }
        } catch (_: java.io.IOException) { Ports.say("[RED] this build of the app has no button-icon font: the button icons in game text draw as ?"); return }
        val env = File(dst, "env.txt")
        val old = if (env.exists()) env.readLines().filterNot { it.startsWith("DM_SHARED_FONT_EXT=") } else emptyList()
        env.writeText((old + "DM_SHARED_FONT_EXT=${File(dst, "nintendo_ext.ttf").path}").joinToString("\n") + "\n")
    }

    /** The player's own font (Chinese or Korean glyphs) for the console's shared fonts, instead of Android's Roboto: DM_SHARED_FONT. */
    private fun sharedFont(dst: File) {
        val src = File(filesDir, "shared_font.ttf")
        val own = File(dst, "standard.ttf")
        if (!src.isFile) { own.delete(); return }
        src.copyTo(own, overwrite = true)
        val env = File(dst, "env.txt")
        val old = if (env.exists()) env.readLines().filterNot { it.startsWith("DM_SHARED_FONT=") } else emptyList()
        env.writeText((old + "DM_SHARED_FONT=${own.path}").joinToString("\n") + "\n")
        Ports.say("System font: your own font is used for the console's shared fonts")
    }

    /** 60 fps for a game whose profile has no cap: the community's FPSLocker config of this build (downloaded once, kept), converted for the host. */
    private fun fpsConfig(id: String, dst: File) {
        val plan = Core.nativeFpsPlan(dst.path, id)
        if (!plan.startsWith("ok: ")) { Ports.say("60 fps: $plan"); return }
        val (bid, url) = plan.removePrefix("ok: ").split('\t')
        val cache = File(cacheDir, "fpslocker").apply { mkdirs() }
        val f = File(cache, "$bid.yaml")
        val none = File(cache, "$bid.none")
        var text: String? = if (f.isFile) f.readText() else null
        if (text == null && !none.exists()) {
            try {
                val c = java.net.URL(url).openConnection() as java.net.HttpURLConnection
                c.connectTimeout = 20000; c.readTimeout = 30000; c.setRequestProperty("User-Agent", "nx2android-native")
                when (c.responseCode) {
                    200 -> { text = c.inputStream.bufferedReader().readText(); f.writeText(text) }
                    404 -> none.writeText("")
                    else -> Ports.say("60 fps: the config server answered ${c.responseCode}")
                }
            } catch (e: Exception) { Ports.say("60 fps: could not download the community's FPSLocker config (${e.message}); is the phone online?"); return }
        }
        if (text == null) { Ports.say("60 fps: the community has no FPSLocker config for this build of the game ($bid): the game keeps its own rate"); return }
        val r = Core.nativeFpsConvert(dst.path, id, text, url)
        if (!r.startsWith("ok: ")) {
            File(dst, "fpslock.tsv").delete(); File(dst, "extra_cuts.tsv").delete()
            Ports.say("[RED] 60 fps: the community config cannot be applied (${r.removePrefix("error: ")}): the game keeps its own rate")
            return
        }
        val env = File(dst, "env.txt")
        val old = if (env.exists()) env.readLines().filterNot { it.startsWith("DM_FPS_TARGET=") || it.startsWith("DM_FPS_LOCKS=") || it.startsWith("DM_PRESENT_INTERVAL=") } else emptyList()
        env.writeText((old + "DM_FPS_TARGET=60" + "DM_FPS_LOCKS=${File(dst, "fpslock.tsv").path}" + "DM_PRESENT_INTERVAL=display").joinToString("\n") + "\n")
        Ports.say("60 fps: FPSLocker config applied (${r.removePrefix("ok: ")})")
    }

    private fun step(k: Int, n: Int, name: String, notify: String, work: () -> String) {
        if (Ports.cancelled) throw Failure("cancelled")
        Ports.stage(k, n, name)
        getSystemService(NotificationManager::class.java).notify(1, notification("$notify: $name"))
        val r = work()
        val bad = r.startsWith("error")
        Ports.end(name, !bad, r.removePrefix("ok: ").removePrefix("error: "))
        // a failed rewrite or shader build does not stop the port: the game is extracted; the log says what is missing
        if (bad) Ports.say("(the game's files are in the Games tab, but this step did not finish, so it may not start)")
    }

    private fun stop(startId: Int): Int {
        stopSelf(startId)
        return START_NOT_STICKY
    }

    private fun createChannel() {
        val ch = NotificationChannel("port", "Porting", NotificationManager.IMPORTANCE_LOW)
        getSystemService(NotificationManager::class.java).createNotificationChannel(ch)
    }

    private fun notification(text: String): Notification =
        Notification.Builder(this, "port")
            .setContentTitle("nx2android")
            .setContentText(text)
            .setSmallIcon(R.drawable.ic_stat_nx2a)
            .setOngoing(Ports.running)
            .build()

    companion object {
        private fun captured(dst: File) = dst.resolve("captured_shaders").list { _, n -> n.endsWith(".bin") }?.size ?: 0

        /**
         * Shaders the game saved while it ran (captured_shaders/) are translated in without the player doing anything: when the app is opened again and a game has more
         * saved than its last build used, the shader step runs for it. A build that adds nothing does not loop: the count it used is kept (captured.built).
         */
        fun autoShaders(ctx: Context, jobs: Int = 3): String? {
            if (Ports.running) return null
            val dirs = File(ctx.filesDir, "data").listFiles { f -> File(f, "ExeFS").isDirectory && File(f, "mods").isDirectory } ?: return null
            for (g in dirs.sortedBy { it.name }) {
                val now = captured(g)
                val built = File(g, "captured.built").takeIf { it.exists() }?.readText()?.trim()?.toIntOrNull() ?: 0
                if (now > built) {
                    shaders(ctx, g.name, jobs)
                    return File(g, "name.txt").takeIf { it.exists() }?.readText()?.trim() ?: g.name
                }
            }
            return null
        }
        fun shaders(ctx: Context, game: String, jobs: Int = 3) {
            ctx.startForegroundService(Intent(ctx, PortService::class.java).putExtra("shadersOf", game).putExtra("jobs", jobs))
        }

        fun start(ctx: Context, base: Uri, baseName: String, update: Uri?, updateName: String, dlc: List<Pair<Uri, String>>, opt: PortOptions) {
            val i = Intent(ctx, PortService::class.java)
                .putStringArrayListExtra("dlc", ArrayList(dlc.map { it.first.toString() }))
                .putStringArrayListExtra("dlcNames", ArrayList(dlc.map { it.second }))
                .putExtra("base", base.toString())
                .putExtra("baseName", baseName)
                .putExtra("update", update?.toString() ?: "")
                .putExtra("updateName", updateName)
                .putExtra("appName", opt.appName)
                .putExtra("touch", opt.touch)
                .putExtra("fps60", opt.fps60)
                .putExtra("jobs", opt.jobs).putExtra("noMenu", opt.noMenuButton).putExtra("mtReplay", opt.mtReplay)
            ctx.startForegroundService(i)
        }
    }
}
