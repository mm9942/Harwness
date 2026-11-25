package dev.harw.mobile

import android.content.Context
import android.os.Environment
import android.os.StatFs
import android.util.Log
import java.io.DataInputStream
import java.io.File
import java.io.IOException
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
internal const val PrefCustomDir = "custom_dir" // user-chosen models folder, overrides the location
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

/** Null means all three operations succeeded. A failed removal makes the folder ineligible. */
internal fun probeModelDirectory(
    dir: File,
    createProbe: (File) -> File = { File.createTempFile(ProbePrefix, null, it) },
): String? {
    val probe = try {
        if (!dir.isDirectory && !dir.mkdirs() && !dir.isDirectory) return "cannot create folder"
        createProbe(dir)
    } catch (e: Exception) {
        return e.javaClass.simpleName
    }

    val removed: Boolean
    val failure = try {
        val data = ByteArray(ProbeBytes).also { SecureRandom().nextBytes(it) }
        probe.outputStream().use { it.write(data) }
        val matches = DataInputStream(probe.inputStream()).use { input ->
            val bytes = ByteArray(ProbeBytes)
            input.readFully(bytes)
            bytes.contentEquals(data) && input.read() == -1
        }
        if (matches) null else "read back differs"
    } catch (e: Exception) {
        e.javaClass.simpleName
    } finally {
        removed = try {
            probe.delete()
        } catch (_: Exception) {
            false
        }
    }
    return if (removed) failure else "cannot remove model storage probe"
}

internal class ModelStore(context: Context) {
    private val app: Context = context.applicationContext
    private val prefs = app.getSharedPreferences(ModelStorePrefs, Context.MODE_PRIVATE)

    private val documentsRoot: File
        get() = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOCUMENTS)

    /** Stored choice, otherwise [select]. */
    val location: ModelLocation
        get() = locationFromKey(prefs.getString(PrefLocation, null)) ?: select()

    /** Folder chosen in the settings, `null` = the default place ([location]). */
    fun customDir(): File? =
        prefs.getString(PrefCustomDir, null)?.takeIf { it.isNotBlank() }?.let { File(it) }

    /** Saves [dir] as the models folder (`null` = back to the default). */
    fun setCustomDir(dir: File?) {
        prefs.edit().apply {
            if (dir == null) remove(PrefCustomDir) else putString(PrefCustomDir, dir.absolutePath)
        }.apply()
    }

    /** The default folder (Documents/.harw/models or its fallback), ignoring a custom choice. */
    fun defaultDir(): File = dirFor(location)

    /** Checks whether the model folder supports write, read-back and cleanup. */
    fun probeDir(dir: File): Boolean = probeModelDirectory(dir) == null

    fun modelsDir(): File = (customDir() ?: dirFor(location)).also { it.mkdirs() }

    /** Fail before network I/O if the actual target cannot support a complete write. */
    fun requireDownloadDir(): File {
        val dir = modelsDir()
        probeModelDirectory(dir)?.let { throw IOException(it) }
        return dir
    }

    fun dirFor(location: ModelLocation): File =
        modelsDirFor(location, documentsRoot, app.filesDir)

    fun probe(location: ModelLocation): ProbeResult {
        val failure = probeModelDirectory(dirFor(location))
        return ProbeResult(location, failure == null, failure ?: "ok")
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

    fun freeBytes(dir: File = modelsDir()): Long {
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
