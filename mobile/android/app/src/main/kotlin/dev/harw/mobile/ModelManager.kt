package dev.harw.mobile

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import kotlin.coroutines.coroutineContext
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.isActive
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.CatalogModel
import uniffi.harw_mobile_ffi.LocalModel
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ModelProgress
import uniffi.harw_mobile_ffi.ProjectorAttach
import uniffi.harw_mobile_ffi.ProjectorInfo
import uniffi.harw_mobile_ffi.VisionAddon
import uniffi.harw_mobile_ffi.attachProjector
import uniffi.harw_mobile_ffi.defaultProjectorFor
import uniffi.harw_mobile_ffi.detachProjector
import uniffi.harw_mobile_ffi.modelProjector
import uniffi.harw_mobile_ffi.suggestProjectors
import uniffi.harw_mobile_ffi.activateLocalModel
import uniffi.harw_mobile_ffi.activeLocalModel
import uniffi.harw_mobile_ffi.deleteModel
import uniffi.harw_mobile_ffi.finalizeModel
import uniffi.harw_mobile_ffi.localModels
import uniffi.harw_mobile_ffi.modelCatalog
import uniffi.harw_mobile_ffi.visionCatalog
import uniffi.harw_mobile_ffi.visionModels

internal const val StorageMarginBytes = 100L * 1024 * 1024
internal const val MaxRedirects = 5

internal const val AllowedUrlPrefix = "https://huggingface.co/"
private const val CopyBufferBytes = 256 * 1024
private const val ProgressStepBytes = 512L * 1024
private const val ConnectTimeoutMs = 15_000
private const val ReadTimeoutMs = 30_000
private const val NetworkCheckStepBytes = 8L * 1024 * 1024
private const val CancelledText = "model check cancelled"

internal enum class ResumeDecision { Append, Restart, Complete, Fail }

internal fun rangeHeader(offset: Long): String? = if (offset <= 0L) null else "bytes=$offset-"

internal fun resumeDecision(status: Int, contentRange: String?, offset: Long, size: Long): ResumeDecision {
    return when (status) {
        206 -> if (contentRangeMatches(contentRange, offset, size)) ResumeDecision.Append else ResumeDecision.Fail
        200 -> ResumeDecision.Restart
        416 -> if (offset == size && size > 0L) ResumeDecision.Complete else ResumeDecision.Fail
        else -> ResumeDecision.Fail
    }
}

private fun contentRangeMatches(header: String?, offset: Long, size: Long): Boolean {
    if (header == null) return false
    val text = header.trim()
    if (!text.startsWith("bytes ")) return false
    val spec = text.removePrefix("bytes ").trim()
    val slash = spec.indexOf('/')
    if (slash < 0) return false
    val range = spec.substring(0, slash)
    val total = spec.substring(slash + 1).trim()
    val dash = range.indexOf('-')
    if (dash < 0) return false
    val start = range.substring(0, dash).trim().toLongOrNull() ?: return false
    if (start != offset) return false
    if (total == "*") return true
    val totalValue = total.toLongOrNull() ?: return false
    return totalValue == size
}

internal fun hasRoom(freeBytes: Long, sizeBytes: Long, haveBytes: Long): Boolean {
    val needed = (sizeBytes - haveBytes).coerceAtLeast(0L)
    return freeBytes >= needed + StorageMarginBytes
}

internal fun isRedirect(status: Int): Boolean =
    status == 301 || status == 302 || status == 303 || status == 307 || status == 308

internal fun resolveRedirect(current: URL, location: String?): URL? {
    if (location.isNullOrBlank()) return null
    val next = try {
        URL(current, location.trim())
    } catch (e: java.net.MalformedURLException) {
        return null
    }
    if (next.protocol != "https") return null
    if (next.userInfo != null) return null
    if (next.host.isNullOrEmpty()) return null
    return next
}

private fun unquoteTag(header: String): String {
    var text = header.trim()
    if (text.startsWith("W/")) text = text.removePrefix("W/")
    return text.trim().removeSurrounding("\"").lowercase()
}

internal fun linkedEtagMatches(header: String?, sha256: String): Boolean? {
    if (header == null) return null
    return unquoteTag(header) == sha256.lowercase()
}

internal fun linkedSizeMatches(header: String?, size: Long): Boolean? {
    if (header == null) return null
    return unquoteTag(header).toLongOrNull() == size
}

internal sealed interface DownloadResult {
    data class Done(val model: LocalModel) : DownloadResult
    data class NoSpace(val neededBytes: Long, val freeBytes: Long) : DownloadResult
    data class Failed(val message: String) : DownloadResult
    data object Cancelled : DownloadResult
    data object WaitingForNetwork : DownloadResult
}

internal class ModelManager(private val store: ModelStore) {
    fun modelsDir(): File = store.modelsDir()

    fun catalog(): List<CatalogModel> = modelCatalog()

    /** Optional image-understanding files (vision projectors) of catalog models. */
    fun visionAddons(): List<VisionAddon> = visionCatalog()

    /** Everything the downloader accepts: models and their vision projectors. */
    fun downloadable(): List<CatalogModel> = modelCatalog() + visionCatalog().map { it.file }

    suspend fun localVision(): List<LocalModel> = withContext(Dispatchers.IO) {
        visionModels(store.modelsDir().path)
    }

    suspend fun projector(modelId: String): ProjectorInfo? = withContext(Dispatchers.IO) {
        modelProjector(store.modelsDir().path, modelId)
    }

    suspend fun projectorSuggestions(modelId: String): List<ProjectorInfo> = withContext(Dispatchers.IO) {
        suggestProjectors(store.modelsDir().path, modelId)
    }

    suspend fun defaultProjector(modelId: String): VisionAddon? = withContext(Dispatchers.IO) {
        defaultProjectorFor(store.modelsDir().path, modelId)
    }

    suspend fun attachProjectorFile(modelId: String, fileName: String): ProjectorAttach = withContext(Dispatchers.IO) {
        ModelDirLock.mutex.withLock { attachProjector(store.modelsDir().path, modelId, fileName) }
    }

    suspend fun detachProjectorOf(modelId: String) = withContext(Dispatchers.IO) {
        ModelDirLock.mutex.withLock { detachProjector(store.modelsDir().path, modelId) }
    }

    suspend fun local(): List<LocalModel> = withContext(Dispatchers.IO) {
        localModels(store.modelsDir().path)
    }

    /** Id of the model running on this device when it is the active provider, else null. */
    suspend fun activeLocal(filesDir: String): String? = withContext(Dispatchers.IO) {
        activeLocalModel(filesDir)
    }

    /** Makes the model the active provider ("On this device") for standalone mode. */
    suspend fun useLocal(filesDir: String, modelId: String) {
        withContext(Dispatchers.IO) {
            ModelDirLock.mutex.withLock {
                activateLocalModel(filesDir, store.modelsDir().path, modelId)
            }
        }
    }

    suspend fun delete(model: CatalogModel) = delete(model.id)

    suspend fun delete(modelId: String) {
        withContext(Dispatchers.IO) {
            ModelDirLock.mutex.withLock {
                deleteModel(store.modelsDir().path, modelId)
            }
        }
    }

    suspend fun download(
        model: CatalogModel,
        onProgress: (Long, Long) -> Unit,
        onVerify: (Long, Long) -> Unit = { _, _ -> },
        networkAllowed: () -> Boolean = { true },
    ): DownloadResult {
        if (!model.url.startsWith(AllowedUrlPrefix)) {
            return DownloadResult.Failed("download address is not allowed")
        }
        val size = model.sizeBytes.toLong()
        return withContext(Dispatchers.IO) {
            ModelDirLock.mutex.withLock {
                try {
                    val dir = store.modelsDir()
                    val part = File(dir, model.fileName + ".part")
                    var offset = if (part.exists()) part.length() else 0L
                    if (offset > size) {
                        part.delete()
                        offset = 0L
                    }
                    if (offset < size) {
                        val free = store.freeBytes()
                        if (!hasRoom(free, size, offset)) {
                            return@withLock DownloadResult.NoSpace(size - offset + StorageMarginBytes, free)
                        }
                        val outcome = fetch(model, part, offset, size, onProgress, networkAllowed)
                        if (outcome != null) return@withLock outcome
                    }
                    if (part.length() != size) {
                        return@withLock DownloadResult.Failed("download incomplete")
                    }
                    verify(dir, model, onVerify)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: IOException) {
                    DownloadResult.Failed(e.message ?: "network error")
                }
            }
        }
    }

    private suspend fun verify(dir: File, model: CatalogModel, onVerify: (Long, Long) -> Unit): DownloadResult {
        val job = coroutineContext[Job]
        val progress = object : ModelProgress {
            override fun onProgress(doneBytes: ULong, totalBytes: ULong): Boolean {
                onVerify(doneBytes.toLong(), totalBytes.toLong())
                return job?.isActive ?: true
            }
        }
        return try {
            DownloadResult.Done(finalizeModel(dir.path, model.id, progress))
        } catch (e: MobileException) {
            val text = e.message ?: "model verification failed"
            if (text.contains(CancelledText)) DownloadResult.Cancelled else DownloadResult.Failed(text)
        }
    }

    // Returns null when the part file was filled (or already complete), else the final result.
    // Every (re)start begins at model.url; signed CDN addresses are never remembered.
    private suspend fun fetch(
        model: CatalogModel,
        part: File,
        offset: Long,
        size: Long,
        onProgress: (Long, Long) -> Unit,
        networkAllowed: () -> Boolean,
    ): DownloadResult? {
        if (!networkAllowed()) return DownloadResult.WaitingForNetwork
        var current = URL(model.url)
        var hops = 0
        while (true) {
            val connection = current.openConnection() as HttpURLConnection
            try {
                connection.instanceFollowRedirects = false
                connection.connectTimeout = ConnectTimeoutMs
                connection.readTimeout = ReadTimeoutMs
                connection.setRequestProperty("Accept-Encoding", "identity")
                val range = rangeHeader(offset)
                if (range != null) connection.setRequestProperty("Range", range)
                val status = connection.responseCode
                if (hops == 0) {
                    if (linkedEtagMatches(connection.getHeaderField("X-Linked-Etag"), model.sha256) == false ||
                        linkedSizeMatches(connection.getHeaderField("X-Linked-Size"), size) == false
                    ) {
                        return DownloadResult.Failed("server file does not match the catalog")
                    }
                }
                if (isRedirect(status)) {
                    val next = resolveRedirect(current, connection.getHeaderField("Location"))
                        ?: return DownloadResult.Failed("redirect is not allowed")
                    if (hops >= MaxRedirects) return DownloadResult.Failed("too many redirects")
                    hops += 1
                    current = next
                    continue
                }
                val decision = resumeDecision(status, connection.getHeaderField("Content-Range"), offset, size)
                val append = when (decision) {
                    ResumeDecision.Append -> true
                    ResumeDecision.Restart -> false
                    ResumeDecision.Complete -> return null
                    ResumeDecision.Fail -> return DownloadResult.Failed("server answered HTTP $status")
                }
                if (status == 200) {
                    val length = connection.getHeaderField("Content-Length")?.trim()?.toLongOrNull()
                    if (length != null && length != size) {
                        return DownloadResult.Failed("server size does not match the catalog")
                    }
                }
                return copyBody(connection, part, append, offset, size, onProgress, networkAllowed)
            } finally {
                connection.disconnect()
            }
        }
    }

    private suspend fun copyBody(
        connection: HttpURLConnection,
        part: File,
        append: Boolean,
        offset: Long,
        size: Long,
        onProgress: (Long, Long) -> Unit,
        networkAllowed: () -> Boolean,
    ): DownloadResult? {
        var done = if (append) offset else 0L
        var reported = done
        var sinceCheck = 0L
        val buffer = ByteArray(CopyBufferBytes)
        FileOutputStream(part, append).use { out ->
            connection.inputStream.use { input ->
                while (true) {
                    if (!coroutineContext.isActive) {
                        out.flush()
                        out.fd.sync()
                        return DownloadResult.Cancelled
                    }
                    val read = input.read(buffer)
                    if (read < 0) break
                    out.write(buffer, 0, read)
                    done += read
                    sinceCheck += read
                    if (done - reported >= ProgressStepBytes) {
                        reported = done
                        onProgress(done, size)
                    }
                    if (sinceCheck >= NetworkCheckStepBytes) {
                        sinceCheck = 0L
                        if (!networkAllowed()) {
                            out.flush()
                            out.fd.sync()
                            onProgress(done, size)
                            return DownloadResult.WaitingForNetwork
                        }
                    }
                }
                out.flush()
                out.fd.sync()
            }
        }
        onProgress(done, size)
        return null
    }
}
