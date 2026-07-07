package com.vibecast.receiver

import android.content.Context
import java.io.File

/**
 * Certificate provisioning.
 *
 * The harvested device-auth manifest (`certs.json`) is **never** bundled in the
 * APK — it is provisioned out-of-band into the app's private files dir. During
 * development, push it in with:
 *
 * ```sh
 * adb exec-out run-as com.vibecast.receiver \
 *     tee files/certs.json < ~/.vibecast/certs.json > /dev/null
 * ```
 */
object Provisioning {
    private const val CERTS_FILE = "certs.json"

    /** Absolute path to `certs.json` in the files dir, or null if not present. */
    fun certsPath(context: Context): String? {
        val file = File(context.filesDir, CERTS_FILE)
        return if (file.exists() && file.length() > 0) file.absolutePath else null
    }
}
