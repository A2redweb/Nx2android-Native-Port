package org.nx2a.nativeport

/** What the player chose for one port (the Port tab's options). */
data class PortOptions(
    val appName: String = "",
    val touch: Boolean = false,
    val fps60: Boolean = false,
    /** translator processes at once: fewer use less memory */
    val jobs: Int = 3,
)

/** The running job's state, which the Log tab shows: the stage, its progress, and the log. One job at a time. */
object Ports {
    @Volatile var jobId: Int = 0
    @Volatile var title: String = ""
    @Volatile var running = false
    @Volatile var cancelled = false
    @Volatile var stage = "idle"
    @Volatile var frac = 0.0
    @Volatile var result = ""   // the name of the game that came out of the last successful port
    private val lines = ArrayList<String>()

    @Synchronized fun say(s: String) { lines.add(s) }
    @Synchronized fun lineCount() = lines.size
    @Synchronized fun linesFrom(i: Int): List<String> = if (i >= lines.size) emptyList() else ArrayList(lines.subList(i, lines.size))
    @Synchronized fun text() = lines.joinToString("\n")

    @Synchronized fun begin(name: String) {
        lines.clear()
        title = name
        running = true
        cancelled = false
        stage = "Starting"
        frac = 0.0
        result = ""
    }

    fun stage(k: Int, n: Int, name: String) {
        stage = "Stage $k/$n: $name"
        frac = 0.0
    }

    fun end(name: String, ok: Boolean, summary: String = "") {
        say((if (ok) "✓ " else "✗ ") + name + if (summary.isEmpty()) "" else " — $summary")
        frac = 0.0
    }

    fun cancel() {
        cancelled = true
        if (jobId != 0) Core.nativeCancel(jobId)
        say("Cancelling: the port stops at the next step…")
    }
}
