package com.vibecast.receiver

import android.os.Bundle
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Status screen: shows receiver state from [ReceiverState] and starts/stops the
 * [CastReceiverService]. No media renderer is wired yet (server-only phase).
 */
class MainActivity : AppCompatActivity() {
    private lateinit var statusView: TextView
    private lateinit var detailView: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        statusView = findViewById(R.id.status)
        detailView = findViewById(R.id.detail)
        findViewById<Button>(R.id.start_button).setOnClickListener {
            CastReceiverService.start(this)
        }
        findViewById<Button>(R.id.stop_button).setOnClickListener {
            CastReceiverService.stop(this)
        }
    }

    override fun onStart() {
        super.onStart()
        ReceiverState.setListener { render() }
        render()
    }

    override fun onStop() {
        ReceiverState.setListener(null)
        super.onStop()
    }

    private fun render() {
        statusView.text = getString(R.string.status_format, ReceiverState.status)
        detailView.text = ReceiverState.detail
    }
}
