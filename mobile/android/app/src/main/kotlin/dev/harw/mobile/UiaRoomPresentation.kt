package dev.harw.mobile

import java.util.Collections

private const val ROOM_PREFIX = "uia-local-v1:"
private const val MAX_TURNS = 128
private const val MAX_ROOM_BYTES = 256 * 1024
private const val MAX_USER_BYTES = 8 * 1024
private const val MAX_ANSWER_BYTES = 16 * 1024

@JvmInline
internal value class UiaRoomId private constructor(val wire: String) {
    companion object {
        fun parse(wire: String): UiaRoomId? {
            if (!wire.startsWith(ROOM_PREFIX) || wire.length != ROOM_PREFIX.length + 32) return null
            val id = wire.substring(ROOM_PREFIX.length)
            if (id.any { it !in '0'..'9' && it !in 'a'..'f' } || id.all { it == '0' }) return null
            return UiaRoomId(wire)
        }
    }
}

/** Identity-only reference, not an unlock credential. Owner constructs a new instance after each real
 * authorize/open. Core must continue to enforce the vault permission, not trust a Kotlin object. */
internal class UiaRoomUnlockLease(val roomId: UiaRoomId)

/** Deliberately not a data class so debug output never includes transcript content. */
internal class UiaCommittedPair(
    val user: String,
    val reply: String,
    val hasLibraryImage: Boolean = false,
    val canPreviewLibraryImage: Boolean = false,
)

/** Ordinal in this already authenticated snapshot, not a Library identifier. */
internal class UiaRoomPhotoPreviewRequest(
    val lease: UiaRoomUnlockLease,
    val roomRevision: Long,
    val ordinal: Int,
) {
    fun isFor(state: UiaRoomState): Boolean =
        state is UiaRoomState.Open && state.snapshot.lease === lease &&
            state.snapshot.revision == roomRevision &&
            ordinal in 1..state.snapshot.pairs.size &&
            state.snapshot.pairs[ordinal - 1].canPreviewLibraryImage
}

internal class UiaRoomImageDraft(
    val lease: UiaRoomUnlockLease,
    val roomRevision: Long,
    val selection: PrivateLibraryAssistantImage,
) {
    fun isFor(state: UiaRoomState.Open): Boolean =
        state.snapshot.lease === lease && state.snapshot.revision == roomRevision
}

internal class UiaRoomSnapshot private constructor(
    val lease: UiaRoomUnlockLease,
    val revision: Long,
    val pairs: List<UiaCommittedPair>,
) {
    companion object {
        /** Construct only from an authorized native read or durable CAS result, never from drafts. */
        fun fromCore(
            lease: UiaRoomUnlockLease,
            revision: Long,
            pairs: List<UiaCommittedPair>,
        ): UiaRoomSnapshot? {
            if (revision !in 0L..MAX_TURNS.toLong() || revision != pairs.size.toLong()) return null
            val frozen = ArrayList(pairs)
            var size = 0
            for (pair in frozen) {
                if (pair.user.isBlank() || pair.reply.isBlank() ||
                    (pair.canPreviewLibraryImage && !pair.hasLibraryImage)
                ) return null
                size += roomTextBytes(pair.user, MAX_USER_BYTES) ?: return null
                size += roomTextBytes(pair.reply, MAX_ANSWER_BYTES) ?: return null
                if (size > MAX_ROOM_BYTES) return null
            }
            return UiaRoomSnapshot(lease, revision, Collections.unmodifiableList(frozen))
        }
    }
}

internal enum class UiaRoomIssue { Vault, LocalModel, Reopen, Reconciliation }

internal enum class UiaRoomRetryable { Incomplete, Inference, Busy, Cancelled, PhotoModel, PhotoChanged, PhotoInvalid, Interrupted, AttemptUnknown, ForegroundDenied }

/** Distinguishes two attempts even when neither has advanced the sealed revision. */
internal class UiaRoomAttempt

internal sealed interface UiaRoomWork {
    data object Idle : UiaRoomWork
    class Processing(val attempt: UiaRoomAttempt) : UiaRoomWork
    data class Failed(val issue: UiaRoomRetryable) : UiaRoomWork
}

internal sealed interface UiaRoomState {
    data object Locked : UiaRoomState
    data object Opening : UiaRoomState
    data class Unavailable(val issue: UiaRoomIssue) : UiaRoomState

    /** A sealed snapshot plus volatile work status. Processing never contains provisional text. */
    class Open(val snapshot: UiaRoomSnapshot, val work: UiaRoomWork) : UiaRoomState {
        val permitsSend: Boolean get() = work !is UiaRoomWork.Processing
    }
}

internal class UiaRoomSend internal constructor(
    val lease: UiaRoomUnlockLease,
    val expectedRevision: Long,
    val text: String,
    val image: UiaRoomImageDraft? = null,
) {
    /** UI defensiveness only. The owner/admission controller repeats this check atomically. */
    fun isFor(state: UiaRoomState): Boolean =
        state is UiaRoomState.Open && state.permitsSend &&
            state.snapshot.lease === lease && state.snapshot.revision == expectedRevision &&
            (image == null || image.isFor(state))
}

internal class UiaRoomCancel internal constructor(
    val lease: UiaRoomUnlockLease,
    val expectedRevision: Long,
    val attempt: UiaRoomAttempt,
) {
    fun isFor(state: UiaRoomState): Boolean =
        state is UiaRoomState.Open &&
            state.snapshot.lease === lease && state.snapshot.revision == expectedRevision &&
            (state.work as? UiaRoomWork.Processing)?.attempt === attempt
}

internal fun prepareUiaRoomCancel(state: UiaRoomState): UiaRoomCancel? {
    if (state !is UiaRoomState.Open || state.work !is UiaRoomWork.Processing) return null
    return UiaRoomCancel(state.snapshot.lease, state.snapshot.revision, state.work.attempt)
}

internal fun prepareUiaRoomSend(
    state: UiaRoomState, draft: String, image: UiaRoomImageDraft? = null,
): UiaRoomSend? {
    if (state !is UiaRoomState.Open || !state.permitsSend || !uiaRoomAcceptsDraft(draft) ||
        (image != null && !image.isFor(state))
    ) return null
    return UiaRoomSend(state.snapshot.lease, state.snapshot.revision, draft.trim(), image)
}

internal fun uiaRoomAcceptsDraft(draft: String): Boolean =
    draft.isNotBlank() && roomTextBytes(draft, MAX_USER_BYTES) != null

/** Mirrors the native byte constraints without allocating another plaintext byte array. */
private fun roomTextBytes(text: String, max: Int): Int? {
    if (text.length > max) return null
    var offset = 0
    var bytes = 0
    while (offset < text.length) {
        val ch = text[offset++]
        if (Character.isISOControl(ch) && ch != '\n' && ch != '\r' && ch != '\t') return null
        bytes += when {
            Character.isHighSurrogate(ch) -> {
                if (offset == text.length || !Character.isLowSurrogate(text[offset++])) return null
                4
            }
            Character.isLowSurrogate(ch) -> return null
            ch.code <= 0x7F -> 1
            ch.code <= 0x7FF -> 2
            else -> 3
        }
        if (bytes > max) return null
    }
    return bytes
}
