package com.pyn.ime

import android.os.Process
import android.util.Log
import java.util.concurrent.LinkedBlockingDeque

/**
 * Single background thread that runs every engine call and InputConnection edit, in submission
 * order, so decoding never blocks the UI thread (touch handling and key feedback stay at frame rate).
 *
 * Letters are never dropped, but when another letter is already queued right behind the current
 * one, the current one runs with `decode = false`: it only extends the composition, and the next
 * letter decodes once for both. The engine refreshes stale candidates before anything that reads
 * them (space, picks, punctuation), so commits are unaffected.
 */
class EngineWorker {

    private class Job(val isLetter: Boolean, val run: (decode: Boolean) -> Unit)

    private val queue = LinkedBlockingDeque<Job>()
    @Volatile
    private var running = true

    private val thread = Thread({
        Process.setThreadPriority(Process.THREAD_PRIORITY_DISPLAY)
        while (running) {
            val job = try {
                queue.take()
            } catch (_: InterruptedException) {
                break
            }
            val decode = !(job.isLetter && queue.peekFirst()?.isLetter == true)
            try {
                job.run(decode)
            } catch (t: Throwable) {
                Log.e(TAG, "engine job failed", t)
            }
        }
    }, "pyn-engine").apply {
        isDaemon = true
        start()
    }

    /** Run [block] on the engine thread after everything already queued. */
    fun post(block: () -> Unit) {
        queue.put(Job(false) { block() })
    }

    /** A plain composing letter; [block] receives whether it should decode. */
    fun postLetter(block: (decode: Boolean) -> Unit) {
        queue.put(Job(true, block))
    }

    /** Finish queued work (e.g. saving the user dictionary), then stop the thread. */
    fun quitSafely() {
        queue.put(Job(false) { running = false })
    }

    companion object {
        private const val TAG = "PynEngineWorker"
    }
}
