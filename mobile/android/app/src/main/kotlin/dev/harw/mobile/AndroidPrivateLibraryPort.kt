package dev.harw.mobile

import android.graphics.Bitmap

import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.withContext

internal interface PrivateLibraryAuthenticatedSession : VaultPinSession {
    fun library(): AndroidPrivateLibraryPort?
}

internal enum class PrivateLibrarySetup { Ready, New, Unavailable }

internal class AndroidPrivateLibraryPort(
    override val boundVault: VaultPinScope,
    private val native: (operation: Int, size: Int, direct: ByteBuffer) -> Long,
) : PrivateLibraryPort {
    override val lease = PrivateLibraryLease()
    private val live = MutableStateFlow(true)
    override val active = live.asStateFlow()
    override val localDraftSupported = false
    override val localCaptionSupported = true
    override val privateAssistantCaptionSupported = true
    override val explicitApiEditSupported = true

    fun revoke() { live.value = false }

    suspend fun setupStatus(): PrivateLibrarySetup = withContext(Dispatchers.IO) {
        if (!live.value) return@withContext PrivateLibrarySetup.Unavailable
        val frame = allocate()
        try {
            when (call(0, 0, frame)) {
                0L -> PrivateLibrarySetup.Ready
                -8L -> PrivateLibrarySetup.New
                -1L -> { revoke(); PrivateLibrarySetup.Unavailable }
                else -> PrivateLibrarySetup.Unavailable
            }
        } finally { wipe(frame) }
    }

    suspend fun provision() = command(1) {}

    override suspend fun snapshot(): PrivateLibrarySnapshot = request(2, {}) { frame ->
        require(frame.remaining() >= 10)
        val revision = frame.long.toULong()
        val count = frame.short.toInt() and 0xffff
        require(count <= 64 && (revision != 0uL || count == 0))
        val seen = HashSet<PrivateLibraryId>(count)
        val entries = List(count) {
            val id = readId(frame)
            require(seen.add(id))
            val name = readText(frame, 160)
            require(name.isNotEmpty())
            val caption = readText(frame, 4096)
            val description = if (caption.isEmpty()) PrivateLibraryDescription.Missing else PrivateLibraryDescription.Ready(caption)
            PrivateLibraryEntry(id, name, description)
        }
        require(!frame.hasRemaining())
        PrivateLibrarySnapshot(revision, entries)
    }

    override suspend fun preview(revision: ULong, id: PrivateLibraryId): ByteArray = request(
        3, { frame -> frame.putLong(revision.toLong()); writeId(frame, id) },
    ) { frame ->
        require(frame.remaining() in 4..180_000)
        ByteArray(frame.remaining()).also(frame::get)
    }

    /** No byte array or item ID leaves this private transport/rendering boundary. */
    suspend fun previewRoomPhoto(expected: UiaRoomPhotoPreviewRequest): Bitmap? = request(
        11, { frame ->
            require(expected.roomRevision in 1L..128L && expected.ordinal in 1..expected.roomRevision.toInt())
            frame.putLong(expected.roomRevision)
            frame.putShort(expected.ordinal.toShort())
        },
    ) { frame ->
        require(frame.remaining() in 4..180_000)
        val jpeg = ByteArray(frame.remaining())
        try {
            frame.get(jpeg)
            if (live.value) decodePrivateLibraryPreview(jpeg, sampleSize = 2) else null
        } finally {
            jpeg.fill(0)
        }
    }

    override suspend fun add(revision: ULong, name: String, jpeg: ByteArray) = command(4) { frame ->
        require(jpeg.size in 4..180_000)
        frame.putLong(revision.toLong())
        writeText(frame, name, 160)
        frame.putInt(jpeg.size).put(jpeg)
    }

    override suspend fun rename(revision: ULong, id: PrivateLibraryId, name: String) = command(5) { frame ->
        frame.putLong(revision.toLong())
        writeId(frame, id)
        writeText(frame, name, 160)
    }

    override suspend fun describe(revision: ULong, id: PrivateLibraryId, description: String?) = command(6) { frame ->
        frame.putLong(revision.toLong())
        writeId(frame, id)
        writeText(frame, description.orEmpty(), 4096)
    }

    override suspend fun remove(revision: ULong, id: PrivateLibraryId) = command(7) { frame ->
        frame.putLong(revision.toLong())
        writeId(frame, id)
    }

    override suspend fun captionChoice(): PrivateLibraryCaptionChoice = request(8, {}) { frame ->
        require(frame.remaining() >= 12)
        val revision = frame.long.toULong()
        val model = readText(frame, 384)
        require(model.isNotEmpty() && !frame.hasRemaining())
        PrivateLibraryCaptionChoice(model, revision)
    }

    override suspend fun captionChoices(): List<PrivateLibraryCaptionChoice> = request(12, {}) { frame ->
        require(frame.remaining() >= 2)
        val count = frame.short.toInt() and 0xffff
        require(count <= 256)
        List(count) { PrivateLibraryCaptionChoice(readText(frame, 384), 0uL) }
            .also { require(!frame.hasRemaining()) }
    }

    override suspend fun describeWithLocalModel(
        revision: ULong, id: PrivateLibraryId, choice: PrivateLibraryCaptionChoice,
    ): String = request(9, { frame ->
        frame.putLong(revision.toLong())
        writeId(frame, id)
        frame.putLong(choice.loadedRevision.toLong())
        writeText(frame, choice.modelId, 384)
    }) { frame ->
        require(frame.remaining() in 1..4096)
        val bytes = ByteArray(frame.remaining())
        try {
            frame.get(bytes)
            bytes.decodeToString(throwOnInvalidSequence = true).also { require(it.isNotBlank()) }
        } finally { bytes.fill(0) }
    }

    override suspend fun readSavedDescriptionForAssistant(
        revision: ULong, id: PrivateLibraryId,
    ): PrivateLibraryAssistantCaption = request(10, { frame ->
        frame.putLong(revision.toLong())
        writeId(frame, id)
    }) { frame ->
        require(frame.remaining() in 1..4096)
        val bytes = ByteArray(frame.remaining())
        try {
            frame.get(bytes)
            val text = bytes.decodeToString(throwOnInvalidSequence = true)
            require(text.isNotBlank())
            PrivateLibraryAssistantCaption(lease, boundVault, text)
        } finally { bytes.fill(0) }
    }

    override suspend fun prepareLocalDraft(revision: ULong, ids: List<PrivateLibraryId>): PrivateLibraryDraft =
        throw PrivateLibraryException(PrivateLibraryIssue.Unavailable)

    private suspend fun command(op: Int, encode: (ByteBuffer) -> Unit) = request(op, encode) {
        require(!it.hasRemaining())
    }

    private suspend fun <T> request(
        operation: Int, encode: (ByteBuffer) -> Unit, decode: (ByteBuffer) -> T,
    ): T = withContext(Dispatchers.IO) {
        if (!live.value) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
        val frame = allocate()
        try {
            try { encode(frame) } catch (_: IllegalArgumentException) {
                throw PrivateLibraryException(PrivateLibraryIssue.InvalidPhoto)
            }
            val size = frame.position()
            frame.position(0)
            val received = call(operation, size, frame)
            if (received < 0) throw status(received)
            if (!live.value) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
            if (received > FrameBytes) throw PrivateLibraryException(PrivateLibraryIssue.Unavailable)
            frame.limit(received.toInt())
            try { decode(frame) } catch (_: RuntimeException) {
                throw PrivateLibraryException(PrivateLibraryIssue.Unavailable)
            }
        } finally { wipe(frame) }
    }

    private fun call(op: Int, size: Int, frame: ByteBuffer): Long =
        try { native(op, size, frame) } catch (_: LinkageError) { -6L }

    private fun status(code: Long): PrivateLibraryException = PrivateLibraryException(when (code) {
        -1L -> { revoke(); PrivateLibraryIssue.Locked }
        -2L -> PrivateLibraryIssue.Changed
        -3L -> PrivateLibraryIssue.Full
        -4L -> PrivateLibraryIssue.InvalidPhoto
        -5L -> PrivateLibraryIssue.Interrupted
        -9L -> PrivateLibraryIssue.LocalModelUnavailable
        -10L -> PrivateLibraryIssue.CaptionIncomplete
        -11L -> PrivateLibraryIssue.DescriptionMissing
        else -> PrivateLibraryIssue.Unavailable
    })

    private fun allocate() = ByteBuffer.allocateDirect(FrameBytes).order(ByteOrder.BIG_ENDIAN)

    private fun wipe(frame: ByteBuffer) {
        frame.clear()
        while (frame.remaining() >= Long.SIZE_BYTES) frame.putLong(0L)
        while (frame.hasRemaining()) frame.put(0.toByte())
    }

    private fun readId(frame: ByteBuffer): PrivateLibraryId {
        require(frame.remaining() >= 16)
        return requireNotNull(PrivateLibraryId.fromNative(buildString(32) {
            repeat(16) {
                val value = frame.get().toInt() and 0xff
                append(Hex[value ushr 4])
                append(Hex[value and 0xf])
            }
        }))
    }

    private fun writeId(frame: ByteBuffer, id: PrivateLibraryId) {
        val hex = id.nativeValue()
        repeat(16) { index ->
            val high = Hex.indexOf(hex[2 * index])
            val low = Hex.indexOf(hex[2 * index + 1])
            require(high >= 0 && low >= 0)
            frame.put(((high shl 4) or low).toByte())
        }
    }

    private fun readText(frame: ByteBuffer, bound: Int): String {
        require(frame.remaining() >= 4)
        val size = frame.int
        require(size in 0..bound && size <= frame.remaining())
        val wire = ByteArray(size)
        return try {
            frame.get(wire)
            wire.decodeToString(throwOnInvalidSequence = true)
        } finally { wire.fill(0) }
    }

    private fun writeText(frame: ByteBuffer, value: String, bound: Int) {
        val wire = value.encodeToByteArray()
        try {
            require(wire.size <= bound)
            frame.putInt(wire.size).put(wire)
        } finally { wire.fill(0) }
    }

    private companion object {
        const val FrameBytes = 288 * 1024
        const val Hex = "0123456789abcdef"
    }
}
