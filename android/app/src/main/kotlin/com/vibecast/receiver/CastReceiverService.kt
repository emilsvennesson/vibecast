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
import java.net.Inet4Address
import java.net.NetworkInterface
import java.net.SocketException
import java.util.concurrent.Executors

/**
 * `connectedDevice` foreground service hosting the vibecast server.
 *
 * It owns the blocking [ReceiverHandle] lifecycle (driven off the main thread),
 * a [WifiManager.WifiLock] to keep the radio awake while serving, and — as the
 * [ReceiverObserver] — one `NsdManager` service registration **per player** that
 * registers over the bridge (re-registered on certificate rotation, torn down
 * when the player disconnects). The Rust core advertises nothing itself
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

    // Per-player NSD registrations, keyed by player id. Touched only on the main
    // looper (registerPlayer / unregisterPlayer are always posted there).
    private val registrations = HashMap<String, PlayerRegistration>()

    private class PlayerRegistration(
        val listener: NsdManager.RegistrationListener,
        val name: String,
        val instanceName: String,
        val castPort: Int,
    )

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
                model = settings.model,
                bindHost = "0.0.0.0",
                playerPort = settings.playerPort,
                // Report this device's LAN address (bindHost is the wildcard, so
                // the Rust routing heuristic can't be trusted on a multi-interface
                // phone); null falls back to that heuristic.
                localIp = localIpAddress(),
                appsConfigJson = null,
            )
        ReceiverState.update("Starting…")
        val newHandle = ReceiverHandle()
        handle = newHandle
        try {
            newHandle.start(config, this)
            // start() is blocking; returning without error means the bridge is
            // listening. No Cast device exists until a player registers.
            mainHandler.post { refreshRunningState() }
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

    override fun onPlayerStarted(
        playerId: String,
        name: String,
        instanceName: String,
        castPort: UShort,
        eurekaHttpPort: UShort,
        txt: List<TxtEntry>,
    ) {
        mainHandler.post {
            registerPlayer(playerId, name, instanceName, castPort.toInt(), txt)
            refreshRunningState()
        }
        Log.i(TAG, "player started: $name id=$playerId cast=$castPort")
    }

    override fun onPlayerTxtChanged(
        playerId: String,
        txt: List<TxtEntry>,
    ) {
        mainHandler.post {
            val existing = registrations[playerId] ?: return@post
            Log.i(TAG, "TXT changed (cert rotation); re-registering NSD for $playerId")
            registerPlayer(playerId, existing.name, existing.instanceName, existing.castPort, txt)
        }
    }

    override fun onPlayerStopped(playerId: String) {
        mainHandler.post {
            unregisterPlayer(playerId)
            refreshRunningState()
        }
        Log.i(TAG, "player stopped: $playerId")
    }

    /** Reflect the running server + current player count in the UI. */
    private fun refreshRunningState() {
        val detail =
            if (registrations.isEmpty()) {
                "Waiting for players…"
            } else {
                val names = registrations.values.joinToString(", ") { it.name }
                "${registrations.size} player(s): $names"
            }
        ReceiverState.update("Running", detail)
    }

    override fun onError(message: String) {
        ReceiverState.update("Error", message)
        Log.e(TAG, "receiver error: $message")
    }

    // --- NsdManager registration (one service per player) ---

    private fun registerPlayer(
        playerId: String,
        name: String,
        instanceName: String,
        port: Int,
        txt: List<TxtEntry>,
    ) {
        // Re-registration (cert rotation): drop the previous registration first.
        unregisterPlayer(playerId)
        val info =
            NsdServiceInfo().apply {
                serviceName = instanceName
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
        registrations[playerId] = PlayerRegistration(listener, name, instanceName, port)
        nsdManager.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
    }

    private fun unregisterPlayer(playerId: String) {
        registrations.remove(playerId)?.let { registration ->
            try {
                nsdManager.unregisterService(registration.listener)
            } catch (error: IllegalArgumentException) {
                Log.w(TAG, "NSD listener was not registered", error)
            }
        }
    }

    // --- Networking ---

    /**
     * This device's site-local IPv4 address, reported to senders as the eureka
     * `ip_address`. Returns `null` (letting the Rust core fall back to its
     * routed-interface heuristic) if none can be resolved.
     */
    private fun localIpAddress(): String? =
        try {
            NetworkInterface
                .getNetworkInterfaces()
                .asSequence()
                .filter { it.isUp && !it.isLoopback }
                .flatMap { it.inetAddresses.asSequence() }
                .filterIsInstance<Inet4Address>()
                .firstOrNull { it.isSiteLocalAddress }
                ?.hostAddress
        } catch (error: SocketException) {
            Log.w(TAG, "failed to resolve local IP; using core heuristic", error)
            null
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
        registrations.keys.toList().forEach { unregisterPlayer(it) }
        ReceiverState.update("Stopped")
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
