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
import android.os.ParcelFileDescriptor
import org.json.JSONObject
import java.io.File

/** Holds the job state the screen reads. */
object Ports {
    @Volatile var jobId: Int = 0
    @Volatile var title: String = ""
}

/** Runs one extraction as a foreground service, so it survives the screen turning off. The work itself is the Rust core's thread; this only keeps the process alive and shows progress. */
class PortService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val base = intent?.getStringExtra("base") ?: return stop(startId)
        val baseName = intent.getStringExtra("baseName") ?: "game.nsp"
        val update = intent.getStringExtra("update") ?: ""
        val updateName = intent.getStringExtra("updateName") ?: ""

        createChannel()
        startForeground(1, notification("Starting…"), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)

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
                val stem = baseName.substringBeforeLast('.')
                val out = File(filesDir, "ports/$stem").also { it.mkdirs() }
                val tmp = File(cacheDir, "decompressed")
                Ports.title = stem
                val id = Core.nativeExtract(File(filesDir, "prod.keys").path, "/proc/self/fd/${bfd.fd}", baseName, upath, updateName, out.path, tmp.path)
                Ports.jobId = id
                while (true) {
                    val s = JSONObject(Core.nativeStatus(id))
                    val state = s.optString("state")
                    val text = if (state == "running") "${s.optString("stage")} (${s.optLong("bytes") shr 20} MB)" else state
                    getSystemService(NotificationManager::class.java).notify(1, notification("$stem: $text"))
                    if (state != "running") break
                    Thread.sleep(1000)
                }
            } catch (e: Exception) {
                android.util.Log.e("nx2a", "port failed", e)
            } finally {
                fds.forEach { try { it.close() } catch (_: Exception) {} }
                stopForeground(STOP_FOREGROUND_DETACH)
                stopSelf(startId)
            }
        }.start()
        return START_NOT_STICKY
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
            .setContentTitle("Nx2Android Native")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setOngoing(true)
            .build()

    companion object {
        fun start(ctx: Context, base: Uri, baseName: String, update: Uri?, updateName: String) {
            val i = Intent(ctx, PortService::class.java)
                .putExtra("base", base.toString())
                .putExtra("baseName", baseName)
                .putExtra("update", update?.toString() ?: "")
                .putExtra("updateName", updateName)
            ctx.startForegroundService(i)
        }
    }
}
