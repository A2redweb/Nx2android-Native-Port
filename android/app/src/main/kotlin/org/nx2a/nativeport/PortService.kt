package org.nx2a.nativeport

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.IBinder
import android.app.ActivityManager
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import org.json.JSONObject
import java.io.File

/** Holds the job state the screen reads. */
object Ports {
    @Volatile var jobId: Int = 0
    @Volatile var title: String = ""
}

/** Runs one extraction as a foreground service, so it survives the screen turning off. The work itself is the Rust core's thread; this only keeps the process alive and shows progress. */
class PortService : Service() {
    /** One translator thread per ~1.5 GB of memory the system says is free, at most 4 and at most the cores: a thread killed for memory loses the whole port. */
    private fun translatorThreads(): Int {
        val mi = ActivityManager.MemoryInfo()
        getSystemService(ActivityManager::class.java).getMemoryInfo(mi)
        val byMem = (mi.availMem / (1536L shl 20)).toInt()
        return minOf(Runtime.getRuntime().availableProcessors(), 4, byMem).coerceAtLeast(1)
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val base = intent?.getStringExtra("base") ?: return stop(startId)
        val baseName = intent.getStringExtra("baseName") ?: "game.nsp"
        val update = intent.getStringExtra("update") ?: ""
        val updateName = intent.getStringExtra("updateName") ?: ""
        val dlc = intent.getStringArrayListExtra("dlc") ?: arrayListOf()
        val dlcNames = intent.getStringArrayListExtra("dlcNames") ?: arrayListOf()

        createChannel()
        // how the last process ended (a kill for memory shows here), for the log a bug report needs
        try {
            getSystemService(ActivityManager::class.java).getHistoricalProcessExitReasons(packageName, 0, 3).forEach {
                android.util.Log.i("nx2a", "previous process ${it.processName}: reason=${it.reason} (${it.description}) rss=${it.rss shr 20} MB")
            }
        } catch (_: Exception) {}
        startForeground(1, notification("Starting…"), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)

        val wake = getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nx2a:port").apply { setReferenceCounted(false); acquire(6 * 60 * 60 * 1000L) }
        Thread {
            val fds = ArrayList<ParcelFileDescriptor>()
            try {
                val bfd = contentResolver.openFileDescriptor(Uri.parse(base), "r") ?: error("cannot open $baseName")
                fds.add(bfd)
                var upath = ""
                if (update.isNotEmpty()) {
                    val ufd = contentResolver.openFileDescriptor(Uri.parse(update), "r") ?: error("cannot open $updateName")
                    fds.add(ufd)
                    upath = "/proc/self/fd/${ufd.fd}"
                }
                val dlcLines = StringBuilder()
                dlc.forEachIndexed { i, u ->
                    val d = contentResolver.openFileDescriptor(Uri.parse(u), "r") ?: error("cannot open ${dlcNames.getOrNull(i)}")
                    fds.add(d)
                    dlcLines.append("/proc/self/fd/${d.fd}\t${dlcNames.getOrNull(i) ?: "dlc.nsp"}\n")
                }
                val stem = baseName.substringBeforeLast('.')
                val out = File(filesDir, "ports/$stem").also { it.mkdirs() }
                val tmp = File(cacheDir, "decompressed")
                Ports.title = stem
                val id = Core.nativeExtract(File(filesDir, "prod.keys").path, "/proc/self/fd/${bfd.fd}", baseName, upath, updateName, out.path, tmp.path, dlcLines.toString())
                Ports.jobId = id
                while (true) {
                    val s = JSONObject(Core.nativeStatus(id))
                    val state = s.optString("state")
                    val text = if (state == "running") "${s.optString("stage")} (${s.optLong("bytes") shr 20} MB)" else state
                    getSystemService(NotificationManager::class.java).notify(1, notification("$stem: $text"))
                    if (state == "done") finish(out, s.optString("title_id"))
                    if (state != "running") break
                    Thread.sleep(1000)
                }
            } catch (e: Exception) {
                android.util.Log.e("nx2a", "port failed", e)
            } finally {
                fds.forEach { try { it.close() } catch (_: Exception) {} }
                if (wake.isHeld) wake.release()
                stopForeground(STOP_FOREGROUND_DETACH)
                stopSelf(startId)
            }
        }.start()
        return START_NOT_STICKY
    }

    /** Moves the extracted files to the folder the game screen reads: data/<profile folder name> (the profile's own spelling of the title ID). */
    private fun finish(out: File, titleId: String) {
        val id = File(filesDir, "games").list()?.firstOrNull { it.equals(titleId, ignoreCase = true) } ?: titleId.uppercase()
        val dst = File(filesDir, "data/$id")
        dst.parentFile?.mkdirs()
        if (dst.exists()) File(dst, "RomFS").deleteRecursively().also { File(dst, "ExeFS").deleteRecursively() }
        dst.mkdirs()
        if (File(out, "aoc").isDirectory) File(dst, "aoc").deleteRecursively()
        for (n in listOf("RomFS", "ExeFS", "aoc", "control.nacp", "icon.jpg")) File(out, n).takeIf { it.exists() }?.renameTo(File(dst, n))
        File(dst, "name.txt").writeText(File(out, "name.txt").takeIf { it.exists() }?.readText() ?: stemOf(out))
        out.deleteRecursively()
        val err = Core.nativePrepare(File(filesDir, "games").path, id, dst.path, java.util.Locale.getDefault().toLanguageTag())
        if (err.isNotEmpty()) android.util.Log.e("nx2a", "switch files: $err")
        File(dst, "mods").deleteRecursively()   // an older port's code must not outlive a failed rewrite
        val rw = Core.nativeRewrite(File(filesDir, "games").path, id, dst.path)
        File(dst, "rewrite.txt").writeText(rw + "\n")
        android.util.Log.i("nx2a", "rewrite: $rw")
        File(dst, "shaders.dmsv").delete()
        val xlate = File(applicationInfo.nativeLibraryDir, "libdmsh_xlate.so")
        val sh = if (xlate.isFile) Core.nativeBuildShaders(File(filesDir, "games").path, id, dst.path, xlate.path, translatorThreads())
                 else "error: the shader translator is not part of this build"
        File(dst, "shaders.txt").writeText(sh + "\n")
        android.util.Log.i("nx2a", "shaders: $sh")
    }

    private fun stemOf(out: File) = out.name

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
            .setContentTitle("Nx2Android Native")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setOngoing(true)
            .build()

    companion object {
        fun start(ctx: Context, base: Uri, baseName: String, update: Uri?, updateName: String, dlc: List<Pair<Uri, String>> = emptyList()) {
            val i = Intent(ctx, PortService::class.java)
                .putStringArrayListExtra("dlc", ArrayList(dlc.map { it.first.toString() }))
                .putStringArrayListExtra("dlcNames", ArrayList(dlc.map { it.second }))
                .putExtra("base", base.toString())
                .putExtra("baseName", baseName)
                .putExtra("update", update?.toString() ?: "")
                .putExtra("updateName", updateName)
            ctx.startForegroundService(i)
        }
    }
}
