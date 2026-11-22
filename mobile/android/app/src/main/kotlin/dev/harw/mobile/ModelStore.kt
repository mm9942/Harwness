package dev.harw.mobile

import android.content.Context
import android.os.Environment
import android.os.StatFs
import android.util.Log
import java.io.File
import java.security.SecureRandom
import kotlinx.coroutines.sync.Mutex

/** Storage places for model files, in the order of the on-device decision (S-DOCS). */
internal enum class ModelLocation(val key: String) {
    DocumentsHidden("documents-hidden"), // <Documents>/.harw/models (default)
    DocumentsVisible("documents-visible"), // <Documents>/harw/models (fallback, visible)
    AppPrivate("app-private"), // <filesDir>/models (fallback)
}

internal const val ModelsSubdir = "models"
internal const val HiddenAppDir = ".harw"
internal const val VisibleAppDir = "harw"
internal const val ModelStorePrefs = "harw_models"
internal const val PrefLocation = "location"
internal const val PrefWifiOnly = "wifi_only" // default true, used by ModelDownloadJob
internal const val ProbePrefix = ".harw-probe-"

private const val StoreTag = "HarwModelStore"
private const val ProbeBytes = 16

/** Adjust after S-DOCS: order in which locations are tried. */
internal val LocationOrder: List<ModelLocation> =
    listOf(
        ModelLocation.DocumentsHidden,
        ModelLocation.DocumentsVisible,
        ModelLocation.AppPrivate,
    )

/** Unknown or null key gives null. */
internal fun locationFromKey(key: String?): ModelLocation? =
    ModelLocation.entries.firstOrNull { it.key == key }

internal fun modelsDirFor(location: ModelLocation, documentsRoot: File, filesDir: File): File =
    when (location) {
        ModelLocation.DocumentsHidden -> File(File(documentsRoot, HiddenAppDir), ModelsSubdir)
        ModelLocation.DocumentsVisible -> File(File(documentsRoot, VisibleAppDir), ModelsSubdir)
        ModelLocation.AppPrivate -> File(filesDir, ModelsSubdir)
    }

internal data class ProbeResult(val location: ModelLocation, val ok: Boolean, val detail: String)

internal class ModelStore(context: Context) {
    private val app: Context = context.applicationContext
    private val prefs = app.getSharedPreferences(ModelStorePrefs, Context.MODE_PRIVATE)

    private val documentsRoot: File
        get() = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOCUMENTS)

    /** Stored choice, otherwise [select]. */
    val location: ModelLocation
        get() = locationFromKey(prefs.getString(PrefLocation, null)) ?: select()

    fun modelsDir(): File = dirFor(location).also { it.mkdirs() }

    fun dirFor(location: ModelLocation): File =
        modelsDirFor(location, documentsRoot, app.filesDir)

    /** Creates the folder, writes 16 random bytes to a probe file, reads them back, deletes it. */
    fun probe(location: ModelLocation): ProbeResult {
        val dir = dirFor(location)
        var probe: File? = null
        return try {
            if (!dir.isDirectory && !dir.mkdirs() && !dir.isDirectory) {
                return ProbeResult(location, false, "cannot create folder")
            }
            val file = File(dir, ProbePrefix + System.currentTimeMillis())
            probe = file
            val data = ByteArray(ProbeBytes).also { SecureRandom().nextBytes(it) }
            file.outputStream().use { it.write(data) }
            val back = file.inputStream().use { it.readBytes() }
            if (back.contentEquals(data)) {
                ProbeResult(location, true, "ok")
            } else {
                ProbeResult(location, false, "read back differs")
            }
        } catch (e: Exception) {
            ProbeResult(location, false, e.javaClass.simpleName)
        } finally {
            try {
                probe?.delete()
            } catch (_: Exception) {
            }
        }
    }

    /**
     * First writable place in [LocationOrder]; saved. A stored choice is never switched
     * silently: it stays while it is writable or still holds model files.
     */
    fun select(): ModelLocation {
        val stored = locationFromKey(prefs.getString(PrefLocation, null))
        if (stored != null && (probe(stored).ok || holdsModels(dirFor(stored)))) return stored
        val chosen =
            LocationOrder.firstOrNull { probe(it).ok }
                ?: ModelLocation.AppPrivate.also { dirFor(it).mkdirs() }
        prefs.edit().putString(PrefLocation, chosen.key).apply()
        debug("model location: ${chosen.key}")
        return chosen
    }

    fun freeBytes(): Long {
        val dir = modelsDir()
        return try {
            StatFs(dir.path).availableBytes
        } catch (_: Exception) {
            0L
        }
    }

    /** Private copy folder, for mmap EACCES on the shared location. */
    fun privateFallbackDir(): File = File(app.filesDir, ModelsSubdir)

    fun wifiOnly(): Boolean = prefs.getBoolean(PrefWifiOnly, true)

    fun setWifiOnly(value: Boolean) {
        prefs.edit().putBoolean(PrefWifiOnly, value).apply()
    }

    /** Any complete or partial model file (not probe leftovers, not sidecar JSON). */
    private fun holdsModels(dir: File): Boolean {
        val names = try {
            dir.list()
        } catch (_: Exception) {
            null
        } ?: return false
        return names.any { n ->
            !n.startsWith(ProbePrefix) &&
                (n.endsWith(".gguf", ignoreCase = true) ||
                    n.endsWith(".part") ||
                    n.endsWith(".gguf.part"))
        }
    }

    private fun debug(message: String) {
        val debuggable = (app.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0
        if (debuggable) Log.d(StoreTag, message)
    }
}

/** One writer per model folder in the process: download, import, delete. */
internal object ModelDirLock {
    val mutex: Mutex = Mutex()
}
