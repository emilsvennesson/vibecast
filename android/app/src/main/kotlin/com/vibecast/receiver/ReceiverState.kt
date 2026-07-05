package com.vibecast.receiver

import android.os.Handler
import android.os.Looper

/**
 * Minimal shared UI state between [CastReceiverService] and [MainActivity].
 *
 * The service updates it from Rust/Tokio worker threads (via the observer);
 * updates are marshalled to the main thread before notifying the listener.
 */
object ReceiverState {
    private val mainHandler = Handler(Looper.getMainLooper())

    @Volatile
    var status: String = "Stopped"
        private set

    @Volatile
    var detail: String = ""
        private set

    private var listener: (() -> Unit)? = null

    fun setListener(callback: (() -> Unit)?) {
        listener = callback
    }

    fun update(
        status: String,
        detail: String = this.detail,
    ) {
        this.status = status
        this.detail = detail
        mainHandler.post { listener?.invoke() }
    }
}
