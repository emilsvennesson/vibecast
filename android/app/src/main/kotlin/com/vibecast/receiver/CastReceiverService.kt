package com.vibecast.receiver

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import uniffi.vibecast_ffi.ReceiverHandle
import uniffi.vibecast_ffi.ReceiverObserver
import uniffi.vibecast_ffi.ServerConfig
import uniffi.vibecast_ffi.TxtEntry
import java.util.concurrent.Executors

/**
 * `connectedDevice` foreground service hosting the vibecast receiver.
 *
 * It owns the blocking [ReceiverHandle] lifecycle (driven off the main thread),
 * a [WifiManager.WifiLock] to keep the radio awake while serving, and — as the
 * [ReceiverObserver] — the `NsdManager` service registration (re-registered on
 * certificate rotation). The Rust core advertises nothing itself
 * (`advertise_mdns = false`); discovery is entirely this layer's job.
 */
class CastReceiverService :
    Service(),
    ReceiverObserver {
    private val worker = Executors.newSingleThreadExecutor()

    // NsdManager dispatches its RegistrationListener via a Handler, so all NSD
    // operations must run on a Looper thread. The observer callbacks that drive
    // them arrive on Rust/Tokio worker threads, so marshal onto the main looper.
    private val mainHandler = Handler(Looper.getMainLooper())
    private lateinit var nsdManager: NsdManager

    // Written on the worker thread (startReceiver) and read on the main thread
    // (onDestroy); @Volatile gives the cross-thread visibility that would
    // otherwise be missing, so onDestroy never misses a started handle.
    @Volatile
    private var handle: ReceiverHandle? = null

    @Volatile
    private var wifiLock: WifiManager.WifiLock? = null
    private var registrationListener: NsdManager.RegistrationListener? = null

    @Volatile
    private var instanceName: String? = null

    @Volatile
    private var castPort: Int = 0

    override fun onCreate() {
        super.onCreate()
        nsdManager = getSystemService(Context.NSD_SERVICE) as NsdManager
        createNotificationChannel()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(
        intent: Intent?,
        flags: Int,
        startId: Int,
    ): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        startForegroundNotification()
        acquireWifiLock()
        worker.execute { startReceiver() }
        return START_STICKY
    }

    private fun startReceiver() {
        if (handle != null) return
        val certs = Provisioning.certsPath(this)
        if (certs == null) {
            ReceiverState.update("Error", "certs.json not provisioned in ${filesDir.absolutePath}")
            Log.e(TAG, "certs.json missing; provision via adb (see Provisioning.kt)")
            stopSelf()
            return
        }
        val settings = Settings(this)
        val config =
            ServerConfig(
                dataDir = filesDir.absolutePath,
                certsPath = certs,
                friendlyName = settings.friendlyName,
                model = settings.model,
                bindHost = "0.0.0.0",
                castPort = settings.castPort,
                eurekaHttpPort = settings.eurekaHttpPort,
                eurekaHttpsPort = settings.eurekaHttpsPort,
                playerPort = settings.playerPort,
                deviceId = null,
                appsConfigJson = null,
            )
        ReceiverState.update("Starting…")
        val newHandle = ReceiverHandle()
        handle = newHandle
        try {
            newHandle.start(config, this)
        } catch (error: Exception) {
            // Clear the handle so the `handle != null` guard above doesn't wedge
            // a later retry; the failed handle never started, so nothing to stop.
            handle = null
            newHandle.close()
            ReceiverState.update("Error", error.message ?: error.toString())
            Log.e(TAG, "receiver start failed", error)
            stopSelf()
        }
    }

    // --- ReceiverObserver: invoked from Rust/Tokio worker threads ---

    override fun onStarted(
        castPort: UShort,
        eurekaHttpPort: UShort,
        instanceName: String,
        txt: List<TxtEntry>,
    ) {
        mainHandler.post { registerService(instanceName, castPort.toInt(), txt) }
        ReceiverState.update("Running", "cast=$castPort eureka=$eurekaHttpPort · $instanceName")
        Log.i(TAG, "receiver started: $instanceName cast=$castPort")
    }

    override fun onTxtChanged(txt: List<TxtEntry>) {
        mainHandler.post {
            val name = instanceName ?: return@post
            Log.i(TAG, "TXT changed (cert rotation); re-registering NSD")
            unregisterService()
            registerService(name, castPort, txt)
        }
    }

    override fun onStopped() {
        ReceiverState.update("Stopped")
        Log.i(TAG, "receiver stopped")
    }

    override fun onError(message: String) {
        ReceiverState.update("Error", message)
        Log.e(TAG, "receiver error: $message")
    }

    // --- NsdManager registration ---

    private fun registerService(
        name: String,
        port: Int,
        txt: List<TxtEntry>,
    ) {
        instanceName = name
        castPort = port
        val info =
            NsdServiceInfo().apply {
                serviceName = name
                serviceType = SERVICE_TYPE
                setPort(port)
                txt.forEach { entry -> setAttribute(entry.key, entry.value) }
            }
        val listener =
            object : NsdManager.RegistrationListener {
                override fun onServiceRegistered(info: NsdServiceInfo) {
                    // NsdManager may append " (2)" on a name clash; log the final name.
                    Log.i(TAG, "NSD registered as ${info.serviceName}")
                }

                override fun onRegistrationFailed(
                    info: NsdServiceInfo,
                    errorCode: Int,
                ) {
                    Log.e(TAG, "NSD registration failed: $errorCode")
                    ReceiverState.update(ReceiverState.status, "NSD registration failed ($errorCode)")
                }

                override fun onServiceUnregistered(info: NsdServiceInfo) {
                    Log.i(TAG, "NSD unregistered")
                }

                override fun onUnregistrationFailed(
                    info: NsdServiceInfo,
                    errorCode: Int,
                ) {
                    Log.e(TAG, "NSD unregistration failed: $errorCode")
                }
            }
        registrationListener = listener
        nsdManager.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
    }

    private fun unregisterService() {
        registrationListener?.let { listener ->
            try {
                nsdManager.unregisterService(listener)
            } catch (error: IllegalArgumentException) {
                Log.w(TAG, "NSD listener was not registered", error)
            }
        }
        registrationListener = null
    }

    // --- Wi-Fi lock ---

    @Suppress("DEPRECATION") // WIFI_MODE_FULL_HIGH_PERF is the documented choice for LAN servers.
    private fun acquireWifiLock() {
        if (wifiLock != null) return
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        wifiLock =
            wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "vibecast:receiver").apply {
                setReferenceCounted(false)
                acquire()
            }
    }

    // --- Notification ---

    private fun createNotificationChannel() {
        val channel =
            NotificationChannel(
                CHANNEL_ID,
                getString(R.string.channel_name),
                NotificationManager.IMPORTANCE_LOW,
            )
        (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .createNotificationChannel(channel)
    }

    private fun startForegroundNotification() {
        val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        val stopIntent =
            PendingIntent.getService(
                this,
                0,
                Intent(this, CastReceiverService::class.java).setAction(ACTION_STOP),
                flags,
            )
        val contentIntent =
            PendingIntent.getActivity(
                this,
                0,
                Intent(this, MainActivity::class.java),
                flags,
            )
        val notification =
            NotificationCompat
                .Builder(this, CHANNEL_ID)
                .setContentTitle(getString(R.string.app_name))
                .setContentText(getString(R.string.notification_running))
                .setSmallIcon(R.drawable.ic_launcher)
                .setContentIntent(contentIntent)
                .addAction(0, getString(R.string.action_stop), stopIntent)
                .setOngoing(true)
                .build()
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, foregroundServiceType())
    }

    private fun foregroundServiceType(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
        } else {
            0
        }

    override fun onDestroy() {
        // Cancel any queued (re-)registration before tearing down NSD.
        mainHandler.removeCallbacksAndMessages(null)
        unregisterService()
        val stopping = handle
        handle = null
        val lock = wifiLock
        wifiLock = null
        worker.execute {
            try {
                stopping?.stop()
            } catch (error: Exception) {
                Log.e(TAG, "receiver stop failed", error)
            }
            stopping?.close()
            // Release the Wi-Fi lock only after shutdown completes, so the radio
            // stays up for the receiver's cooperative teardown (clean TCP close,
            // app on_stop / DRM-release traffic).
            lock?.let { if (it.isHeld) it.release() }
        }
        worker.shutdown()
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        super.onDestroy()
    }

    companion object {
        private const val TAG = "vibecast"
        private const val CHANNEL_ID = "vibecast_receiver"
        private const val NOTIFICATION_ID = 1
        private const val SERVICE_TYPE = "_googlecast._tcp"
        const val ACTION_STOP = "com.vibecast.receiver.STOP"

        fun start(context: Context) {
            context.startForegroundService(Intent(context, CastReceiverService::class.java))
        }

        fun stop(context: Context) {
            context.startService(
                Intent(context, CastReceiverService::class.java).setAction(ACTION_STOP),
            )
        }
    }
}
