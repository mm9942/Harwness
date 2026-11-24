package dev.harw.mobile

import android.app.Application
import java.nio.ByteBuffer
import java.nio.ByteOrder

private const val FrameBytes = 89
private const val SnapshotBytes = 256 * 1024 + 9 * 128 + 56
private const val InputBytes = 8192 + 4
private const val PhotoInputBytes = InputBytes + 8 + 16
private const val RoomLibrary = "harw_mobile_ffi"
private const val NativeRecovery = -2L
private const val NativeUncertain = -5L

internal enum class ParkedRoomPending { None, Pending, Unavailable }

/**
 * Dormant, single-process adapter for the registry-selected vault. Construct only in the
 * authenticated profile owner, never from a route, Intent, application startup or UI flag.
 * On profile switch/disconnect/relock the owner must hide content and close the adapter,
 * in addition to relocking its controller. Native I/O runs on a worker.
 */
internal class AndroidUiaRoomPinNative private constructor(
    private val app: HarwApplication,
    private val scope: VaultPinScope,
) : UiaRoomPinNative, AutoCloseable {
    private val guard = Any()
    private val loadNative by lazy { System.loadLibrary(RoomLibrary) }
    private var closed = false
    private var opening = false
    private var current: NativeLease? = null

    override fun provisionNew(frame: ByteBuffer): VaultPinProof = open(frame, UiaRoomPinMode.ProvisionNew)

    override fun resumeEnrollment(frame: ByteBuffer): VaultPinProof = open(frame, UiaRoomPinMode.ResumeEnrollment)

    override fun openExisting(frame: ByteBuffer): VaultPinProof = open(frame, UiaRoomPinMode.OpenExisting)

    override fun openExistingBiometric(frame: ByteBuffer): VaultPinProof =
        open(frame, UiaRoomPinMode.OpenExistingBiometric)

    /** Worker-only; the selected scope's platform ledger exclusion must also be held. */
    fun canRestartUncommitted(): Boolean = uncommittedNative(retire = false)

    /** With a positive result, all subsequent opens of the old scope are fenced natively. */
    fun retireUncommitted(): Boolean = uncommittedNative(retire = true)

    private fun uncommittedNative(retire: Boolean): Boolean = synchronized(guard) {
        if (closed || opening || current != null) return@synchronized false
        try {
            loadNative
            val most = scope.vaultId.mostSignificantBits
            val least = scope.vaultId.leastSignificantBits
            val generation = scope.keyGeneration
            val code = if (retire) retireUncommittedNative(app, most, least, generation)
                else canRestartUncommittedNative(app, most, least, generation)
            code == 1
        } catch (_: LinkageError) {
            false
        } catch (_: Exception) {
            false
        }
    }

    /** Fixed pre-authentication status; never returns native tokens, IDs or conversation data. */
    fun pendingParkedResponse(): ParkedRoomPending {
        try {
            loadNative
        } catch (_: LinkageError) {
            return ParkedRoomPending.Unavailable
        } catch (_: Exception) {
            return ParkedRoomPending.Unavailable
        }
        val version = try {
            parkRoomVersionNative()
        } catch (_: UnsatisfiedLinkError) {
            return ParkedRoomPending.None // Native without parking cannot own a parked attempt.
        } catch (_: Exception) {
            return ParkedRoomPending.Unavailable
        }
        if (version !in 1..2) return ParkedRoomPending.Unavailable
        return try {
            when (hasPendingParkedResponseNative(scope.vaultId.mostSignificantBits, scope.vaultId.leastSignificantBits,
                scope.keyGeneration)) {
                0 -> ParkedRoomPending.None
                1 -> ParkedRoomPending.Pending
                else -> ParkedRoomPending.Unavailable
            }
        } catch (_: LinkageError) {
            ParkedRoomPending.Unavailable
        } catch (_: Exception) {
            ParkedRoomPending.Unavailable
        }
    }

    override fun close() {
        val previous = synchronized(guard) {
            closed = true
            current
        }
        // An in-flight JNI result will see the close fence and revoke its own late token.
        previous?.revoke()
    }

    private fun open(frame: ByteBuffer, mode: UiaRoomPinMode): VaultPinProof {
        if (!frame.isDirect || frame.isReadOnly || frame.capacity() != FrameBytes) {
            // This is not one of our writable frames. The upstream owner is still responsible.
            return VaultPinProof.Unavailable
        }
        try {
            if (!matchesSelectedScope(frame)) return VaultPinProof.Unavailable
            synchronized(guard) {
                if (closed || opening || current != null) return VaultPinProof.Unavailable
                opening = true
            }
            return openReserved(frame, mode)
        } finally {
            frame.clear()
            while (frame.hasRemaining()) frame.put(0.toByte())
        }
    }

    private fun openReserved(frame: ByteBuffer, mode: UiaRoomPinMode): VaultPinProof {
        var unowned = 0L
        try {
            loadNative
            val most = scope.vaultId.mostSignificantBits
            val least = scope.vaultId.leastSignificantBits
            val generation = scope.keyGeneration
            if (synchronized(guard) { closed }) return VaultPinProof.Unavailable
            val code = when (mode) {
                UiaRoomPinMode.ProvisionNew -> provisionNewNative(app, most, least, generation, frame)
                UiaRoomPinMode.ResumeEnrollment -> resumeEnrollmentNative(app, most, least, generation, frame)
                UiaRoomPinMode.OpenExisting -> openExistingNative(app, most, least, generation, frame)
                UiaRoomPinMode.OpenExistingBiometric -> openExistingBiometricNative(app, most, least, generation, frame)
            }
            if (code <= 0) return denied(code, mode)
            unowned = code

            val lease = NativeLease(code, bindRoomCalls())
            val grant = VaultPinProof.Granted(lease)
            return synchronized(guard) {
                if (closed) {
                    VaultPinProof.Unavailable
                } else {
                    current = lease
                    unowned = 0L
                    grant
                }
            }
        } catch (_: LinkageError) {
            // In a default library these symbols are absent: no alternate transport or room.
            return VaultPinProof.Unavailable
        } finally {
            try {
                if (unowned > 0) revokeOwnedToken(unowned)
            } finally {
                synchronized(guard) { opening = false }
            }
        }
    }

    private fun matchesSelectedScope(frame: ByteBuffer): Boolean {
        if (frame.position() != 0 || frame.limit() != FrameBytes) return false
        val header = frame.duplicate().order(ByteOrder.BIG_ENDIAN)
        return header.get(0) == 1.toByte() &&
            header.getLong(1) == scope.vaultId.mostSignificantBits &&
            header.getLong(9) == scope.vaultId.leastSignificantBits &&
            header.getLong(17) == scope.keyGeneration
    }

    private fun denied(code: Long, mode: UiaRoomPinMode): VaultPinProof {
        if (mode == UiaRoomPinMode.ProvisionNew || mode == UiaRoomPinMode.ResumeEnrollment) {
            val stage = when (code) {
                -10L -> VaultPinEnrollmentStage.NativeApp
                -11L -> VaultPinEnrollmentStage.NativeScope
                -12L -> VaultPinEnrollmentStage.NativeRoomDirectory
                -13L -> VaultPinEnrollmentStage.NativeRoomWrite
                -14L -> VaultPinEnrollmentStage.NativeRoomVerify
                else -> VaultPinEnrollmentStage.NativeBridge
            }
            return VaultPinProof.EnrollmentFailed(stage)
        }
        return when (code) {
            NativeRecovery, NativeUncertain -> VaultPinProof.RecoveryRequired(VaultPinRecovery.DamagedVault)
            // PIN has already been checked; no other native result means wrong PIN.
            else -> VaultPinProof.Unavailable
        }
    }

    private fun revokeOwnedToken(token: Long) {
        var confirmed = false
        try {
            confirmed = revokeNative(token, scope.vaultId.mostSignificantBits, scope.vaultId.leastSignificantBits, scope.keyGeneration) == 1
        } catch (_: LinkageError) {
            // Treat an unproven revoke exactly like an unrecognized owned token.
        } catch (_: Exception) {
            // Do not expose native or loader diagnostics across this boundary.
        } finally {
            if (!confirmed) synchronized(guard) { closed = true }
        }
        // Revoke's normal contract is synchronous/no-throw. Report invariant failure so
        // VaultPinController also stops trusting this owner instead of claiming a verified lock.
        check(confirmed) { "UIA room revocation could not be verified" }
    }

    // Capture strictly private JNI calls in this owning class. The inner NativeLease
    // only sees selected-scope closures, which avoids public JVM synthetic forwarders
    // to raw-token native entry points.
    private class RoomCalls(
        val read: (Long, ByteBuffer) -> Long,
        val reserve: (Long, Long) -> Long,
        val run: (Long, Long, Long, ByteBuffer) -> Long,
        val cancel: (Long, Long) -> Long,
        val revoke: (Long) -> Unit,
        val park: (Long, Long) -> Long,
        val cancelParked: (Long, Long) -> Long,
        val parkVersion: () -> Int,
        val library: (Long, Int, Int, ByteBuffer) -> Long,
        val memory: (Long, Long, Long, Int, Int, ByteBuffer) -> Long,
        val cloudWork: (Long, String, String, Int, ByteBuffer) -> Long,
    )

    private fun bindRoomCalls(): RoomCalls {
        val most = scope.vaultId.mostSignificantBits
        val least = scope.vaultId.leastSignificantBits
        val generation = scope.keyGeneration
        return RoomCalls(
            read = { handle, frame -> readRoomNative(handle, most, least, generation, frame) },
            reserve = { handle, attempt -> reserveRoomNative(handle, most, least, generation, attempt) },
            run = { handle, attempt, revision, frame ->
                runRoomNative(app, handle, most, least, generation, attempt, revision, frame)
            },
            cancel = { handle, attempt -> cancelRoomNative(handle, most, least, generation, attempt) },
            revoke = { handle -> revokeOwnedToken(handle) },
            park = { handle, attempt -> parkRoomNative(handle, most, least, generation, attempt) },
            cancelParked = { handle, attempt -> cancelParkedRoomNative(handle, most, least, generation, attempt) },
            parkVersion = { parkRoomVersionNative() },
            library = { handle, operation, length, frame ->
                libraryNative(app, handle, most, least, generation, operation, length, frame)
            },
            cloudWork = { handle, profile, ticket, action, frame ->
                cloudWorkNative(app, handle, most, least, generation, profile, ticket, action, frame)
            },
            memory = { handle, roomRevision, memoryRevision, ordinal, operation, frame ->
                memoryNative(handle, most, least, generation, roomRevision, memoryRevision, ordinal, operation, frame)
            },
        )
    }

    private data class ParkedRoomAttempt(val handle: Long, val attempt: Long)

    private inner class NativeLease(private var token: Long, private val calls: RoomCalls) :
        UiaRoomChatSession, PrivateLibraryAuthenticatedSession, UiaNativeChat {
        override val scope: VaultPinScope get() = this@AndroidUiaRoomPinNative.scope
        private var connection: UiaRoomChatConnection? = null
        private var libraryPort: AndroidPrivateLibraryPort? = null
        // Not a read handle: native removes it from the reader registry before
        // installing it here. This exact pair can only address native Stop.
        private var parkedAttempt: ParkedRoomAttempt? = null

        override fun library(): AndroidPrivateLibraryPort? = synchronized(this) {
            if (token == 0L) return@synchronized null
            libraryPort ?: AndroidPrivateLibraryPort(scope) { operation, size, frame ->
                val owned = tokenNow()
                if (owned == 0L) -1L else calls.library(owned, operation, size, frame).let { result ->
                    if (tokenNow() == owned) result
                    else if (operation in 1..7 && operation !in 2..3) -5L else -1L
                }
            }.also { libraryPort = it }
        }


        override fun cloudWork(profile: String): ManualCloudWorkPort? = synchronized(this) {
            if (token == 0L) return@synchronized null
            ManualCloudWorkPort { ticket, action, frame ->
                val current = tokenNow()
                if (current == 0L) -1L else {
                    val status = calls.cloudWork(current, profile, ticket, action, frame)
                    if (tokenNow() == current) status else -1L
                }
            }
        }

        override fun memory(): UiaManualMemoryPort? = synchronized(this) {
            if (token == 0L) return@synchronized null
            UiaManualMemoryPort { roomRevision, memoryRevision, ordinal, operation, frame ->
                val owned = tokenNow()
                if (owned == 0L) -1L else {
                    val status = calls.memory(owned, roomRevision, memoryRevision, ordinal, operation, frame)
                    if (tokenNow() == owned) status else -1L
                }
            }
        }

        private fun tokenNow(): Long = synchronized(this) { token }

        override fun chat(): UiaRoomChatConnection? = synchronized(this) {
            if (token == 0L) null else connection ?: UiaRoomChatConnection(this, vaultScope = scope).also { connection = it }
        }

        override fun revoke() {
            val (owned, oldConnection, oldLibrary) = synchronized(this) {
                if (token == 0L) return
                val previous = token
                token = 0L
                val view = connection
                connection = null
                Triple(previous, view, libraryPort.also { libraryPort = null })
            }
            // Drop UI authority immediately; do not hold the lease monitor during
            // native I/O or call the UI while holding it.
            oldConnection?.onRevoked()
            oldLibrary?.revoke()
            try {
                calls.revoke(owned)
            } finally {
                synchronized(guard) { if (current === this) current = null }
            }
        }

        override fun snapshot(): UiaNativeRead {
            val owned = tokenNow()
            if (owned == 0L) return UiaNativeRead.Failure(-1L)
            val frame = ByteBuffer.allocateDirect(SnapshotBytes).order(ByteOrder.BIG_ENDIAN)
            try {
                val count = calls.read(owned, frame)
                if (tokenNow() != owned) return UiaNativeRead.Failure(-1L)
                if (count !in 55L..SnapshotBytes.toLong()) return UiaNativeRead.Failure(count)
                frame.limit(count.toInt())
                val id = UiaRoomId.parse(readRoomText(frame, 45)) ?: return UiaNativeRead.Failure(-2L)
                val revision = frame.long
                val turns = frame.short.toInt()
                if (turns !in 0..128 || revision != turns.toLong()) return UiaNativeRead.Failure(-2L)
                val pairs = List(turns) {
                    UiaCommittedPair(readRoomText(frame, boundedField(frame, 8192)), readRoomText(frame, boundedField(frame, 16384)))
                }
                val interrupted = if (!frame.hasRemaining()) UiaPriorAttempt.None else when (
                    frame.get(frame.limit() - 1).toInt() and 0xff
                ) {
                    0xA1 -> UiaPriorAttempt.Interrupted
                    0xA2 -> UiaPriorAttempt.Unknown
                    else -> UiaPriorAttempt.None
                }
                if (interrupted != UiaPriorAttempt.None) frame.limit(frame.limit() - 1)
                val rendered = if (!frame.hasRemaining()) pairs else {
                    if (frame.remaining() != turns) return UiaNativeRead.Failure(-2L)
                    pairs.map { pair ->
                        when (frame.get().toInt()) {
                            0 -> pair
                            1 -> UiaCommittedPair(pair.user, pair.reply, hasLibraryImage = true)
                            2 -> UiaCommittedPair(
                                pair.user, pair.reply, hasLibraryImage = true, canPreviewLibraryImage = true,
                            )
                            else -> return UiaNativeRead.Failure(-2L)
                        }
                    }.also {
                        if (it.none(UiaCommittedPair::hasLibraryImage)) return UiaNativeRead.Failure(-2L)
                    }
                }
                return if (tokenNow() != owned) UiaNativeRead.Failure(-1L)
                else UiaNativeRead.Snapshot(id, revision, rendered, interrupted)
            } finally { wipeRoomFrame(frame) }
        }

        override fun reserve(attempt: Long): Boolean {
            val owned = tokenNow()
            if (owned == 0L) return false
            return calls.reserve(owned, attempt) == 1L && tokenNow() == owned
        }

        override fun run(attempt: Long, revision: Long, text: String): Long =
            runInput(attempt, revision, text, null)

        override fun runPhoto(
            attempt: Long, revision: Long, text: String, image: PrivateLibraryAssistantImage,
        ): Long = runInput(attempt, revision, text, image)

        private fun runInput(
            attempt: Long, revision: Long, text: String, image: PrivateLibraryAssistantImage?,
        ): Long {
            val owned = tokenNow()
            if (owned == 0L) return -1L
            val frame = ByteBuffer.allocateDirect(if (image == null) InputBytes else PhotoInputBytes)
                .order(ByteOrder.BIG_ENDIAN)
            try {
                // Consume the existing reservation with a zero-length frame on
                // a stale port; no photo or text is sent and no attempt is stranded.
                if (image != null && synchronized(this) {
                        libraryPort?.let { it.lease === image.lease && it.active.value } != true ||
                            image.vault != scope
                    }
                ) {
                    calls.run(owned, attempt, revision, frame)
                    return if (tokenNow() == owned) -11L else -1L
                }
                val bytes = text.encodeToByteArray()
                try {
                    if (bytes.size in 1..8192) frame.putInt(bytes.size).put(bytes)
                } finally { bytes.fill(0) }
                if (image != null) {
                    val opaque = image.id.nativeValue()
                    frame.position(InputBytes)
                    frame.putLong(image.catalogueRevision.toLong())
                    repeat(16) { index ->
                        val offset = index * 2
                        val high = opaque[offset].digitToInt(16)
                        val low = opaque[offset + 1].digitToInt(16)
                        frame.put(((high shl 4) or low).toByte())
                    }
                }
                val result = calls.run(owned, attempt, revision, frame)
                return if (tokenNow() != owned) -1L else result
            } finally {
                wipeRoomFrame(frame)
                synchronized(this) {
                    if (parkedAttempt == ParkedRoomAttempt(owned, attempt)) parkedAttempt = null
                }
            }
        }

        override fun supportsParking(photo: Boolean): Boolean {
            if (tokenNow() == 0L) return false
            // v1 is text only; v2 can park the exact locally admitted photo.
            // An unknown ABI must never widen photo or text continuation.
            return when (calls.parkVersion()) {
                1 -> !photo
                2 -> true
                else -> false
            }
        }

        override fun park(attempt: Long): Boolean {
            val (read, library) = synchronized(this) {
                val owned = token
                if (owned == 0L || calls.park(owned, attempt) != 1L) return false
                token = 0L
                parkedAttempt = ParkedRoomAttempt(owned, attempt)
                val view = connection.also { connection = null }
                view to libraryPort.also { libraryPort = null }
            }
            read?.onRevoked(parked = true)
            library?.revoke()
            synchronized(guard) { if (current === this) current = null }
            return true
        }

        override fun cancel(attempt: Long) = synchronized(this) {
            val owned = token
            val hidden = parkedAttempt
            if (owned > 0L) calls.cancel(owned, attempt)
            else if (hidden != null && hidden.attempt == attempt) calls.cancelParked(hidden.handle, attempt)
            Unit
        }
    }

    private fun boundedField(frame: ByteBuffer, maximum: Int): Int =
        frame.int.also { require(it in 1..maximum && it <= frame.remaining()) }

    private fun readRoomText(frame: ByteBuffer, length: Int): String {
        val bytes = ByteArray(length)
        return try {
            frame.get(bytes)
            bytes.decodeToString(throwOnInvalidSequence = true)
        } finally { bytes.fill(0) }
    }

    private external fun cloudWorkNative(
        app: Application, token: Long, most: Long, least: Long, generation: Long,
        localProfile: String, ticket: String, action: Int, frame: ByteBuffer,
    ): Long

    private external fun memoryNative(
        token: Long, most: Long, least: Long, generation: Long,
        roomRevision: Long, memoryRevision: Long, ordinal: Int, operation: Int, frame: ByteBuffer,
    ): Long

    private external fun libraryNative(
        app: Application,
        token: Long,
        most: Long,
        least: Long,
        generation: Long,
        operation: Int,
        length: Int,
        frame: ByteBuffer,
    ): Long

    private fun wipeRoomFrame(frame: ByteBuffer) {
        frame.clear()
        while (frame.hasRemaining()) frame.put(0.toByte())
    }

    // Exact instance ABI consumed only by vault-uia-jni; never serialize a token or frame.
    private external fun provisionNewNative(
        app: Application, most: Long, least: Long, generation: Long, frame: ByteBuffer,
    ): Long

    private external fun openExistingBiometricNative(
        app: Application, most: Long, least: Long, generation: Long, frame: ByteBuffer,
    ): Long

    private external fun canRestartUncommittedNative(app: Application, most: Long, least: Long, generation: Long): Int
    private external fun retireUncommittedNative(app: Application, most: Long, least: Long, generation: Long): Int

    private external fun resumeEnrollmentNative(
        app: Application, most: Long, least: Long, generation: Long, frame: ByteBuffer,
    ): Long

    private external fun openExistingNative(
        app: Application, most: Long, least: Long, generation: Long, frame: ByteBuffer,
    ): Long

    private external fun revokeNative(handle: Long, most: Long, least: Long, generation: Long): Int

    private external fun readRoomNative(handle: Long, most: Long, least: Long, generation: Long, frame: ByteBuffer): Long
    private external fun parkRoomVersionNative(): Int
    private external fun hasPendingParkedResponseNative(most: Long, least: Long, generation: Long): Int
    private external fun parkRoomNative(handle: Long, most: Long, least: Long, generation: Long, attempt: Long): Long
    private external fun cancelParkedRoomNative(handle: Long, most: Long, least: Long, generation: Long, attempt: Long): Long
    private external fun reserveRoomNative(handle: Long, most: Long, least: Long, generation: Long, attempt: Long): Long
    private external fun cancelRoomNative(handle: Long, most: Long, least: Long, generation: Long, attempt: Long): Long
    private external fun runRoomNative(app: Application, handle: Long, most: Long, least: Long, generation: Long, attempt: Long, revision: Long, input: ByteBuffer): Long

    companion object {
        /** The profile owner supplies a store obtained for its authenticated selected vault. */
        fun forSelectedStore(app: HarwApplication, store: VaultPinDiskStateStore): AndroidUiaRoomPinNative {
            require(app.applicationContext === app && !app.isDeviceProtectedStorage && app.packageName == "dev.harw.mobile")
            val scope = store.scope
            require(scope.vaultId.mostSignificantBits != 0L || scope.vaultId.leastSignificantBits != 0L)
            return AndroidUiaRoomPinNative(app, scope)
        }
    }
}
