package com.vibecast.receiver

import android.content.Context

/**
 * User settings backed by [android.content.SharedPreferences].
 *
 * Ports default to alternates so the receiver coexists with the platform's
 * built-in Cast receiver (which owns :8009/:8008/:8443).
 */
class Settings(
    context: Context,
) {
    private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    val friendlyName: String
        get() = prefs.getString(KEY_NAME, DEFAULT_NAME) ?: DEFAULT_NAME

    val model: String
        get() = prefs.getString(KEY_MODEL, DEFAULT_MODEL) ?: DEFAULT_MODEL

    val castPort: UShort
        get() = prefs.getInt(KEY_CAST_PORT, DEFAULT_CAST_PORT).toUShort()

    val eurekaHttpPort: UShort
        get() = prefs.getInt(KEY_EUREKA_HTTP_PORT, DEFAULT_EUREKA_HTTP_PORT).toUShort()

    val eurekaHttpsPort: UShort
        get() = prefs.getInt(KEY_EUREKA_HTTPS_PORT, DEFAULT_EUREKA_HTTPS_PORT).toUShort()

    val playerPort: UShort
        get() = prefs.getInt(KEY_PLAYER_PORT, DEFAULT_PLAYER_PORT).toUShort()

    fun setFriendlyName(name: String) {
        prefs.edit().putString(KEY_NAME, name).apply()
    }

    private companion object {
        const val PREFS = "vibecast"
        const val KEY_NAME = "friendly_name"
        const val KEY_MODEL = "model"
        const val KEY_CAST_PORT = "cast_port"
        const val KEY_EUREKA_HTTP_PORT = "eureka_http_port"
        const val KEY_EUREKA_HTTPS_PORT = "eureka_https_port"
        const val KEY_PLAYER_PORT = "player_port"

        const val DEFAULT_NAME = "vibecast (Android)"
        const val DEFAULT_MODEL = "Chromecast"

        // Alternate ports to coexist with the system Cast receiver.
        const val DEFAULT_CAST_PORT = 9009
        const val DEFAULT_EUREKA_HTTP_PORT = 9008
        const val DEFAULT_EUREKA_HTTPS_PORT = 9443
        const val DEFAULT_PLAYER_PORT = 8010
    }
}
