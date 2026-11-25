package dev.harw.mobile

import android.content.Context
import android.util.AtomicFile
import org.json.JSONArray
import org.json.JSONObject
import uniffi.harw_mobile_ffi.MobileToolState
import uniffi.harw_mobile_ffi.SessionCursor
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind
import uniffi.harw_mobile_ffi.UserTranscriptPresentation
import java.io.File
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec
import uniffi.harw_mobile_ffi.KeystoreException

/**
 * Frühere Fassungen einer Nachricht ("Bearbeiten" und "Neu generieren"): Vor dem Ersetzen wird der bisherige
 * Verlauf ab der Nachricht (samt Antworten) gemerkt, damit man ihn per Pfeiltasten ansehen kann.
 * Die Fassungen sind nur eine Ansicht auf dem Telefon; der Server kennt nur die aktuelle.
 */
internal data class ChatVersion(
    val tail: List<TranscriptEntry>,
    val beforeDurable: ULong? = null,
    val captureComplete: Boolean = true,
)

internal object ChatVersions {
    private const val MAX_VERSIONS_PER_MESSAGE = 20
    private const val MAX_FILE_BYTES = 2_000_000

    private val ioLock = Any()
    private val random = SecureRandom()
    private const val IV_BYTES = 12
    private const val TAG_BYTES = 16
    private const val CACHE_VERSION: Byte = 1

    private data class CacheTarget(val file: AtomicFile, val keySlot: String, val aad: ByteArray)

    private fun cacheTarget(
        context: Context, scope: SessionHomeScope, sessionId: String, pending: Boolean = false,
    ): CacheTarget {
        // The old filename was only a session ID and cannot prove which host/profile wrote it.
        // Leave legacy caches untouched and never make them visible in a different context.
        val domain = if (pending) "harw/android/chat-rewrite-pending/v1" else "harw/android/chat-variants/v2"
        val owner = "$domain\n${scope.backend.name}\n${scope.profileId}"
        val binding = ("$owner\n$sessionId").encodeToByteArray()
        val name = digest(binding)
        val scopeKey = "variants-" + digest(owner.encodeToByteArray()).take(48)
        val dir = File(context.noBackupFilesDir, if (pending) "chat-rewrite-pending-v1" else "chat-variants-v2")
        return CacheTarget(AtomicFile(File(dir, "$name.aes")), scopeKey, binding)
    }

    private fun digest(bytes: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    private fun key(context: Context, slot: String, create: Boolean): ByteArray {
        val store = AndroidPlatformKeystore(context)
        val key = try {
            store.unwrap(slot)
        } catch (missing: KeystoreException.Missing) {
            if (!create) throw missing
            val generated = ByteArray(32).also(random::nextBytes)
            try {
                store.wrap(slot, generated.copyOf())
                generated
            } catch (error: Exception) {
                generated.fill(0)
                throw error
            }
        }
        if (key.size != 32) {
            key.fill(0)
            throw IllegalStateException("Invalid chat variant cache key")
        }
        return key
    }

    private fun readEncrypted(context: Context, target: CacheTarget): ByteArray {
        val bytes = target.file.openRead().use { stream ->
            val limit = MAX_FILE_BYTES + IV_BYTES + TAG_BYTES + 2
            val buffer = ByteArray(limit)
            var size = 0
            while (size < limit) {
                val count = stream.read(buffer, size, limit - size)
                if (count <= 0) break
                size += count
            }
            buffer.copyOf(size)
        }
        if (bytes.size <= 1 + IV_BYTES + TAG_BYTES || bytes.size > 1 + IV_BYTES + TAG_BYTES + MAX_FILE_BYTES ||
            bytes[0] != CACHE_VERSION
        ) throw IllegalStateException("Invalid chat cache envelope")
        val secret = key(context, target.keySlot, create = false)
        return try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, SecretKeySpec(secret, "AES"), GCMParameterSpec(128, bytes, 1, IV_BYTES))
            cipher.updateAAD(target.aad)
            cipher.doFinal(bytes, 1 + IV_BYTES, bytes.size - 1 - IV_BYTES)
        } finally {
            secret.fill(0)
        }
    }

    /** Key for the previous chat boundary, not for the prompt text. */
    fun anchorOf(entries: List<TranscriptEntry>, index: Int): String = entries.getOrNull(index - 1)?.id.orEmpty()

    fun load(context: Context, scope: SessionHomeScope, sessionId: String): Map<String, List<ChatVersion>> =
        synchronized(ioLock) {
            try {
                val target = cacheTarget(context, scope, sessionId)
                val plain = readEncrypted(context, target)
                try {
                    val root = JSONObject(plain.toString(Charsets.UTF_8))
                    root.keys().asSequence().associateWith { anchor ->
                        val versions = root.getJSONArray(anchor)
                        (0 until versions.length()).map { v ->
                            val saved = versions.get(v)
                            val revision = (saved as? JSONObject)?.optString("before_durable")
                                ?.toULongOrNull()?.takeIf { it > 0uL }
                            // Legacy encrypted variants remain viewable, but lack an exact archive boundary.
                            val tail = if (saved is JSONArray) saved else (saved as JSONObject).getJSONArray("tail")
                            ChatVersion((0 until tail.length()).map { entryOf(tail.getJSONObject(it)) }, revision)
                        }
                    }
                } finally {
                    plain.fill(0)
                }
            } catch (_: Exception) {
                emptyMap()
            }
        }

    data class Saved(val variants: Map<String, List<ChatVersion>>, val persisted: Boolean)

    fun append(
        context: Context,
        scope: SessionHomeScope,
        sessionId: String,
        current: Map<String, List<ChatVersion>>,
        anchor: String,
        version: ChatVersion,
    ): Saved = synchronized(ioLock) {
        // Merge with the persisted in-scope state to avoid losing another window's successful edit.
        val merged = current + load(context, scope, sessionId)
        val existing = merged[anchor].orEmpty()
        val next = if (existing.lastOrNull() == version) merged
        else merged + (anchor to (existing + version).takeLast(MAX_VERSIONS_PER_MESSAGE))
        var plain: ByteArray? = null
        try {
            val root = JSONObject()
            next.forEach { (key, versions) ->
                root.put(key, JSONArray().also { arr ->
                    versions.forEach { v ->
                        val tail = JSONArray().also { rows -> v.tail.forEach { rows.put(jsonOf(it)) } }
                        arr.put(JSONObject().put("tail", tail).put("before_durable", v.beforeDurable?.toString()))
                    }
                })
            }
            val bytes = root.toString().toByteArray(Charsets.UTF_8)
            plain = bytes
            if (bytes.size > MAX_FILE_BYTES) return@synchronized Saved(next, persisted = false)
            val target = cacheTarget(context, scope, sessionId)
            writeEncrypted(context, target, bytes)
            Saved(next, persisted = true)
        } catch (_: Exception) {
            Saved(next, persisted = false)
        } finally {
            plain?.fill(0)
        }
    }

    private fun writeEncrypted(context: Context, target: CacheTarget, bytes: ByteArray) {
        require(bytes.size <= MAX_FILE_BYTES)
        val secret = key(context, target.keySlot, create = true)
        val iv = ByteArray(IV_BYTES).also(random::nextBytes)
        val encrypted = try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(secret, "AES"), GCMParameterSpec(128, iv))
            cipher.updateAAD(target.aad)
            cipher.doFinal(bytes)
        } finally {
            secret.fill(0)
        }
        val output = target.file.startWrite()
        try {
            output.write(byteArrayOf(CACHE_VERSION))
            output.write(iv)
            output.write(encrypted)
            target.file.finishWrite(output)
        } catch (error: Exception) {
            target.file.failWrite(output)
            throw error
        }
    }

    /** Failure to decrypt an existing reservation is not the same as absence. */
    sealed interface Pending {
        data object Missing : Pending
        data object Unreadable : Pending
        data class Found(val attempt: DraftAttempt, val candidate: RewriteVersionCandidate) : Pending
    }

    fun readPending(context: Context, owner: SessionHomeScope, sessionId: String): Pending =
        synchronized(ioLock) {
            val target = cacheTarget(context, owner, sessionId, pending = true)
            try {
                val plain = readEncrypted(context, target)
                try {
                    val row = JSONObject(plain.toString(Charsets.UTF_8))
                    val head = SessionCursor(
                        row.getString("generation").toUInt(),
                        row.getString("durable").toULong(),
                        row.getString("live").toUInt(),
                    )
                    val id = row.getString("id")
                    val item = row.getString("target")
                    val text = row.getString("text")
                    val tail = row.getJSONArray("tail")
                    val entries = (0 until tail.length()).map { entryOf(tail.getJSONObject(it)) }
                    require(id.length in 1..128 && isDurableRewriteItemId(item) &&
                        entries.firstOrNull()?.id == item && text.encodeToByteArray().size <= 256 * 1024)
                    Pending.Found(
                        DraftAttempt(text, head, id, rewindBefore = item),
                        RewriteVersionCandidate(row.getString("anchor"), ChatVersion(
                            entries, head.durable, captureComplete = row.optBoolean("capture_complete", true),
                        ), id),
                    )
                } finally {
                    plain.fill(0)
                }
            } catch (_: java.io.FileNotFoundException) {
                if (target.file.baseFile.exists() || File(target.file.baseFile.path + ".bak").exists())
                    Pending.Unreadable else Pending.Missing
            } catch (_: Exception) {
                Pending.Unreadable
            }
        }

    /** Reserve before sending; only an explicit replay can use this saved operation. */
    fun reservePending(
        context: Context, owner: SessionHomeScope, sessionId: String, pending: Pending.Found,
    ): Boolean = synchronized(ioLock) {
        if (readPending(context, owner, sessionId) != Pending.Missing) return@synchronized false
        var bytes: ByteArray? = null
        try {
            val (attempt, candidate) = pending
            val json = JSONObject().put("id", attempt.id).put("target", attempt.rewindBefore)
                .put("text", attempt.text).put("generation", attempt.head.generation.toString())
                .put("durable", attempt.head.durable.toString()).put("live", attempt.head.live.toString())
                .put("anchor", candidate.anchor)
                .put("tail", JSONArray().also { out -> candidate.previous.tail.forEach { out.put(jsonOf(it)) } })
            var encoded = json.toString().encodeToByteArray()
            if (encoded.size > MAX_FILE_BYTES) {
                // Safety journal still fits: keep the target identity and exact
                // cursor, but do not later label an incomplete transcript as a version.
                encoded.fill(0)
                val first = candidate.previous.tail.first()
                json.put("capture_complete", false)
                    .put("tail", JSONArray().put(jsonOf(first.copy(text = "", images = emptyList(), userPresentation = null))))
                encoded = json.toString().encodeToByteArray()
            }
            bytes = encoded
            writeEncrypted(context, cacheTarget(context, owner, sessionId, pending = true), encoded)
            true
        } catch (_: Exception) {
            false
        } finally {
            bytes?.fill(0)
        }
    }

    fun removePending(context: Context, owner: SessionHomeScope, sessionId: String, id: String?): Boolean =
        synchronized(ioLock) {
            when (val current = readPending(context, owner, sessionId)) {
                Pending.Missing -> return@synchronized true
                is Pending.Found -> if (current.attempt.id != id) return@synchronized false
                Pending.Unreadable -> if (id != null) return@synchronized false
            }
            val target = cacheTarget(context, owner, sessionId, pending = true)
            target.file.delete()
            readPending(context, owner, sessionId) == Pending.Missing
        }

    private fun jsonOf(e: TranscriptEntry) = JSONObject()
        .put("id", e.id).put("kind", e.kind.name).put("text", e.text)
        .put("tool", e.toolState?.name ?: JSONObject.NULL)
        .put("images", JSONArray(e.images))
        .apply {
            e.userPresentation?.let {
                put("userPresentation", JSONObject().put("text", it.text).put("imageCount", it.imageCount.toLong()))
            }
        }

    private fun entryOf(o: JSONObject) = TranscriptEntry(
        id = o.getString("id"),
        kind = TranscriptKind.valueOf(o.getString("kind")),
        text = o.getString("text"),
        toolState = if (o.isNull("tool")) null else MobileToolState.valueOf(o.getString("tool")),
        images = o.optJSONArray("images")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList(),
        userPresentation = o.optJSONObject("userPresentation")?.let { data ->
            val count = data.getLong("imageCount")
            require(count in 0L..UInt.MAX_VALUE.toLong())
            UserTranscriptPresentation(text = data.getString("text"), imageCount = count.toUInt())
        },
    )
}

/** Captured before sending; retained only in this authenticated window scope. */
internal data class RewriteVersionCandidate(
    val anchor: String, val previous: ChatVersion, val requestId: String,
)

/** Queue admission is not proof the replacement committed and reached the model. */
internal data class AwaitingChatRewrite(
    val anchor: String,
    val beforeDurable: ULong,
    val previous: ChatVersion,
    val expectedReplacementId: String,
    val requestId: String,
) {
    fun observedOn(entries: List<TranscriptEntry>, durable: ULong): Boolean {
        val priorFirst = previous.tail.firstOrNull() ?: return false
        if (durable <= beforeDurable || entries.any { it.id == priorFirst.id }) return false
        val nextIndex = if (anchor.isEmpty()) 0 else {
            val preceding = entries.withIndex().singleOrNull { it.value.id == anchor } ?: return false
            preceding.index + 1
        }
        return entries.getOrNull(nextIndex)?.let {
            it.kind == TranscriptKind.USER && it.id == expectedReplacementId &&
                entries.count { row -> row.id == expectedReplacementId } == 1
        } == true
    }
}

/** Zustand und Aktionen der Fassungen für die Unterhaltungsansicht. */
internal class VersionsUi(
    val versions: Map<String, List<ChatVersion>>,
    val viewing: Map<String, Int>,
    val onView: (anchor: String, index: Int?) -> Unit,
    val onEdit: (entry: TranscriptEntry, anchor: String, text: String) -> Unit,
)

/** Zeigt, falls eine ältere Fassung gewählt ist, deren Verlauf statt des aktuellen ab dieser Nachricht. Liefert (Liste, ab welchem Index nur Ansicht). */
internal fun applyViewing(entries: List<TranscriptEntry>, ui: VersionsUi): Pair<List<TranscriptEntry>, Int> {
    for ((i, entry) in entries.withIndex()) {
        if (entry.kind != TranscriptKind.USER) continue
        val anchor = ChatVersions.anchorOf(entries, i)
        val old = ui.viewing[anchor] ?: continue
        val version = ui.versions[anchor]?.getOrNull(old) ?: continue
        return (entries.take(i) + version.tail) to i
    }
    return entries to Int.MAX_VALUE
}

/** Merge only adjacent replays of the same item; identical prompts can be distinct fork boundaries. */
internal fun dedupeUserRuns(entries: List<TranscriptEntry>): List<TranscriptEntry> {
    if (entries.size < 2) return entries
    val out = ArrayList<TranscriptEntry>(entries.size)
    for (entry in entries) {
        val last = out.lastOrNull()
        if (entry.kind == TranscriptKind.USER && last != null && last.kind == TranscriptKind.USER &&
            entry.id.isNotEmpty() && last == entry
        ) continue
        out.add(entry)
    }
    return out
}
