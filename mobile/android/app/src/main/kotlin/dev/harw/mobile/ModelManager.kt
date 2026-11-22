package dev.harw.mobile

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import kotlin.coroutines.coroutineContext
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.CatalogModel
import uniffi.harw_mobile_ffi.LocalModel
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.deleteModel
import uniffi.harw_mobile_ffi.finalizeModel
import uniffi.harw_mobile_ffi.localModels
import uniffi.harw_mobile_ffi.modelCatalog

internal const val StorageMarginBytes = 64L * 1024 * 1024

private const val AllowedUrlPrefix = "https://huggingface.co/"
private const val CopyBufferBytes = 256 * 1024
private const val ProgressStepBytes = 512L * 1024

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

internal sealed interface DownloadResult {
    data class Done(val model: LocalModel) : DownloadResult
    data class NoSpace(val neededBytes: Long, val freeBytes: Long) : DownloadResult
    data class Failed(val message: String) : DownloadResult
    data object Cancelled : DownloadResult
}

internal class ModelManager(private val modelsDir: File) {
    fun catalog(): List<CatalogModel> = modelCatalog()

    suspend fun local(): List<LocalModel> = withContext(Dispatchers.IO) {
        localModels(modelsDir.path)
    }

    suspend fun delete(model: CatalogModel) {
        withContext(Dispatchers.IO) {
            deleteModel(modelsDir.path, model.id)
        }
    }

    suspend fun download(model: CatalogModel, onProgress: (Long, Long) -> Unit): DownloadResult {
        if (!model.url.startsWith(AllowedUrlPrefix)) {
            return DownloadResult.Failed("download address is not allowed")
        }
        val size = model.sizeBytes.toLong()
        val part = File(modelsDir, model.fileName + ".part")
        return withContext(Dispatchers.IO) {
            try {
                modelsDir.mkdirs()
                var offset = if (part.exists()) part.length() else 0L
                if (offset > size) {
                    part.delete()
                    offset = 0L
                }
                if (offset < size) {
                    val free = modelsDir.usableSpace
                    if (!hasRoom(free, size, offset)) {
                        return@withContext DownloadResult.NoSpace(size - offset + StorageMarginBytes, free)
                    }
                    val outcome = fetch(model, part, offset, size, onProgress)
                    if (outcome != null) return@withContext outcome
                }
                if (part.length() != size) {
                    return@withContext DownloadResult.Failed("download incomplete")
                }
                try {
                    DownloadResult.Done(finalizeModel(modelsDir.path, model.id))
                } catch (e: MobileException) {
                    DownloadResult.Failed(e.message ?: "model verification failed")
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: IOException) {
                DownloadResult.Failed(e.message ?: "network error")
            }
        }
    }

    // Returns null when the part file was filled (or already complete), else the final result.
    private suspend fun fetch(
        model: CatalogModel,
        part: File,
        offset: Long,
        size: Long,
        onProgress: (Long, Long) -> Unit,
    ): DownloadResult? {
        val connection = URL(model.url).openConnection() as HttpURLConnection
        try {
            connection.instanceFollowRedirects = true
            connection.connectTimeout = 15_000
            connection.readTimeout = 30_000
            connection.setRequestProperty("Accept-Encoding", "identity")
            val range = rangeHeader(offset)
            if (range != null) connection.setRequestProperty("Range", range)
            val status = connection.responseCode
            val decision = resumeDecision(status, connection.getHeaderField("Content-Range"), offset, size)
            val append = when (decision) {
                ResumeDecision.Append -> true
                ResumeDecision.Restart -> false
                ResumeDecision.Complete -> return null
                ResumeDecision.Fail -> return DownloadResult.Failed("server answered HTTP $status")
            }
            var done = if (append) offset else 0L
            var reported = done
            val buffer = ByteArray(CopyBufferBytes)
            FileOutputStream(part, append).use { out ->
                connection.inputStream.use { input ->
                    while (true) {
                        if (!coroutineContext.isActive) return DownloadResult.Cancelled
                        val read = input.read(buffer)
                        if (read < 0) break
                        out.write(buffer, 0, read)
                        done += read
                        if (done - reported >= ProgressStepBytes) {
                            reported = done
                            onProgress(done, size)
                        }
                    }
                    out.fd.sync()
                }
            }
            onProgress(done, size)
            return null
        } finally {
            connection.disconnect()
        }
    }
}
