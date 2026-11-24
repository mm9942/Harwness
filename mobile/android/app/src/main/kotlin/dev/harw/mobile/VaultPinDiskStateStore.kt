package dev.harw.mobile

import java.io.IOException
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.FileAlreadyExistsException
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.NoSuchFileException
import java.nio.file.Path
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.PosixFileAttributes
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest
import java.util.UUID

private const val StoreFolder = "harw-vault-pin-v1"
private const val StateFile = "state.bin"
private const val LockFile = "state.lock"
private const val JournalFile = "state.pending"
private const val NextFile = "state.next"
private const val PinFile = "pin.v1"
private const val PinJournalFile = "pin.pending"
private const val PinNextFile = "pin.next"
private const val NativeRoomDirectory = "uia-room-v1"
private const val NativeRoomManifest = "enrollment-v1.cge"
private const val NativeRoomStagedManifest = "enrollment-pending-v1.cge"
private const val NativeRoomLeaseLock = ".session-lock"
private const val NativeEnrollmentRetired = ".uia-enrollment-retired-v1"
private const val BioFile = "biometric.v1"
private const val BioClaimFile = "biometric.pending"
private const val BioNextFile = "biometric.next"
private const val BioCancelledFile = "biometric.cancelled"
private const val BioRevokedFile = "biometric.revoked"
private const val BioCleanupFile = "biometric.cleanup-confirmed"
private const val BioClaimBytes = 16
private const val Magic = 0x4850494e // HPIN
private const val FormatVersion: Byte = 1
private const val PayloadBytes = 55
private const val DigestBytes = 32
private const val EncodedBytes = PayloadBytes + DigestBytes
private val DirectoryMode = PosixFilePermissions.fromString("rwx------")
private val FileMode = PosixFilePermissions.fromString("rw-------")
private val ProcessLock = Any()

/** Platform must truly force directory changes or throw, never silently emulate success. */
internal fun interface VaultPinDirectorySync {
    fun force(directory: Path)
}

/**
 * Metadata and separately encrypted PIN slot; never plaintext keys or PINs.
 * Use an app-private, credential-protected, non-backed-up root.
 * The digest detects unintended corruption, not someone restoring/replacing a valid snapshot.
 * The stable lock is never replaced, and all cooperating processes must use this store.
 */
internal class VaultPinDiskStateStore private constructor(
    private val parent: Path,
    val scope: VaultPinScope,
    private val durability: VaultPinDirectorySync,
) : VaultPinStateStore {
    private val namespace = parent.resolve(StoreFolder)
    private val directory = namespace.resolve(scope.vaultId.toString())

    override fun read(): VaultPinRecord = locked { load() }

    override fun replace(expected: VaultPinRecord, next: VaultPinRecord): Boolean = locked {
        // A stale platform controller may still see the old record. Leave it
        // readable for idempotent restart proof, but never advance it again.
        if (attributes(directory.resolve(NativeEnrollmentRetired)) != null) return@locked false
        if (load() != expected) return@locked false
        if (next.scope != scope || expected.revision == Long.MAX_VALUE || next.revision != expected.revision + 1) {
            unavailable()
        }
        val valid = when (expected.registration) {
            VaultPinRegistration.Fresh -> next.registration === VaultPinRegistration.Provisioning
            VaultPinRegistration.Provisioning ->
                next.registration == VaultPinRegistration.Ready() || next.registration is VaultPinRegistration.Recovering
            is VaultPinRegistration.Recovering ->
                next.registration == VaultPinRegistration.Ready() || next.registration is VaultPinRegistration.Recovering
            is VaultPinRegistration.Ready -> next.registration is VaultPinRegistration.Ready
        }
        if (!valid) unavailable()
        if (next.registration is VaultPinRegistration.Recovering && recoveryBlocker() != null) return@locked false
        install(next)
        true
    }

    /**
     * Persist an exclusive claim before hardware provisioning and keep the same
     * cross-process lock through publication. A crash/exception can only strand
     * this claim; another process must not retry or replace the alias.
     * The callback returns owned, already authenticated-encrypted PIN slot bytes.
     */
    fun installNewUiaPinSlot(seal: () -> ByteArray): UiaRoomPinSlot = locked {
        val record = load()
        checkPhase(record, UiaRoomPinMode.ProvisionNew)
        requireNoPinJournal(UiaRoomPinMode.ProvisionNew)
        val target = directory.resolve(PinFile)
        if (attributes(target) != null) unavailable()
        val journal = directory.resolve(PinJournalFile)
        val next = directory.resolve(PinNextFile)
        createPrivateFile(journal)
        durability.force(directory)
        val ciphertext = seal()
        try {
            if (ciphertext.size != VaultPinSlotBytes || ciphertext[0] != 1.toByte()) unavailable()
            createPrivateFile(next, ciphertext)
        } finally {
            ciphertext.fill(0)
        }
        Files.move(next, target, ATOMIC_MOVE)
        durability.force(directory)
        Files.delete(journal)
        durability.force(directory)
        UiaRoomPinSlot(record, readPinCiphertext(UiaRoomPinMode.ProvisionNew))
    }

    /**
     * Offer charged PIN reauthentication only for a clean PIN slot and one intact
     * native anchor, committed or staged. No metadata grants access: native must
     * authenticate the manifest and the exact already-written room before either
     * journal can publish Ready.
     */
    fun enrollmentResumeBlocker(): VaultPinEnrollmentStage? = locked {
        val registration = load().registration
        if (registration !== VaultPinRegistration.Provisioning && registration !is VaultPinRegistration.Recovering) {
            unavailable()
        }
        recoveryBlocker()
    }

    /**
     * Pin the exact pre-enrollment ledger across native proof/retirement and
     * the profile map change. No charged attempt, committed PIN authorization,
     * final ledger journal or biometric state qualifies.
     */
    fun <T : Any> withUncommittedEnrollment(block: () -> T?): T? = locked {
        val record = load() // Reject any state journal, including uncertain final publication.
        if (scope.keyGeneration != 1L || record.revision != 1L ||
            record.registration !== VaultPinRegistration.Provisioning ||
            recoveryBlocker() == null
        ) return@locked null
        if (listOf(BioFile, BioClaimFile, BioNextFile, BioCancelledFile, BioRevokedFile, BioCleanupFile)
                .any { attributes(directory.resolve(it)) != null }
        ) return@locked null

        val pin = directory.resolve(PinFile)
        val claim = directory.resolve(PinJournalFile)
        val next = directory.resolve(PinNextFile)
        val published = attributes(pin)
        val staged = attributes(next)
        if (attributes(claim) != null) verifyFile(claim, 0)
        if (staged != null && (
                attributes(claim) == null || published != null || !staged.isRegularFile ||
                    staged.permissions() != FileMode || staged.size() !in 0L..VaultPinSlotBytes.toLong()
                )
        ) return@locked null
        if (published != null) {
            verifyFile(pin, VaultPinSlotBytes.toLong())
            val encrypted = readExact(pin, VaultPinSlotBytes)
            try { if (encrypted[0] != 1.toByte()) return@locked null }
            finally { encrypted.fill(0) }
        }
        block()
    }

    private fun recoveryBlocker(allowStagedAnchor: Boolean = true): VaultPinEnrollmentStage? {
        try {
            readPinCiphertext(UiaRoomPinMode.ResumeEnrollment).fill(0)
        } catch (_: IOException) {
            return VaultPinEnrollmentStage.SlotStorage
        }
        val room = directory.resolve(NativeRoomDirectory)
        try {
            verifyDirectory(room)
        } catch (_: IOException) {
            return VaultPinEnrollmentStage.NativeRoomDirectory
        }
        try {
            val committed = attributes(room.resolve(NativeRoomManifest))
            val staged = attributes(room.resolve(NativeRoomStagedManifest))
            if ((committed != null && staged != null) || (staged != null && !allowStagedAnchor)) {
                return VaultPinEnrollmentStage.NativeRoomWrite
            }
            val current = committed ?: staged ?: return VaultPinEnrollmentStage.NativeRoomWrite
            if (!current.isRegularFile || current.permissions() != FileMode || current.size() !in 41L..8192L) {
                return VaultPinEnrollmentStage.NativeRoomVerify
            }
            // A staged manifest alone does not establish an existing genesis.
            // Native still requires the signed, decryptable room at revision 0
            // and will not create a missing record or this existing lease lock.
            if (staged != null) verifyFile(room.resolve(NativeRoomLeaseLock), 0)
        } catch (_: IOException) {
            return VaultPinEnrollmentStage.NativeRoomVerify
        }
        return null
    }

    /**
     * Restart-only replay of a recognized interrupted enrollment ledger
     * write. No session, PIN proof, or key is produced here. Do not abandon
     * any prepared charge or accept an unrecognized/partial write. A normal PIN attempt
     * must still pass its durable throttle and authenticate the native room.
     */
    private fun recoverFinalEnrollmentJournal() = locked {
        val pending = directory.resolve(JournalFile)
        val staged = directory.resolve(NextFile)
        val hasPending = attributes(pending) != null
        val hasStage = attributes(staged) != null
        if (!hasPending && !hasStage) {
            loadInstalled()
            return@locked
        }
        if (!hasPending || scope.keyGeneration != 1L) unavailable()
        verifyFile(pending, 0)
        val current = loadInstalled()
        val previous = current.registration
        if (previous === VaultPinRegistration.Fresh) unavailable()
        val next = if (hasStage) {
            verifyFile(staged, EncodedBytes.toLong())
            decode(readExact(staged, EncodedBytes), scope)
        } else null
        val requiresPublishedAnchor = previous is VaultPinRegistration.Ready || next?.registration is VaultPinRegistration.Ready
        if (recoveryBlocker(allowStagedAnchor = !requiresPublishedAnchor) != null) unavailable()
        if (next != null) {
            val admissible = when (val prepared = next.registration) {
                is VaultPinRegistration.Ready -> prepared.delay == null && when (previous) {
                    VaultPinRegistration.Provisioning, is VaultPinRegistration.Recovering -> true
                    is VaultPinRegistration.Ready -> previous.delay != null
                    VaultPinRegistration.Fresh -> false
                }
                is VaultPinRegistration.Recovering -> when (previous) {
                    VaultPinRegistration.Provisioning -> prepared.delay.strike == 1
                    is VaultPinRegistration.Recovering -> preparesRetry(previous.delay, prepared.delay)
                    else -> false
                }
                else -> false
            }
            if (!admissible || current.revision == Long.MAX_VALUE || next.revision != current.revision + 1L) unavailable()
            FileChannel.open(staged, WRITE, NOFOLLOW_LINKS).use { it.force(true) }
            Files.move(staged, directory.resolve(StateFile), ATOMIC_MOVE, REPLACE_EXISTING)
        }
        // No stage: rename either already published, or never took place.
        // Preserve whichever exact record is installed, including its charge.
        // No backend authentication can have started on an uninstalled charge.
        durability.force(directory)
        Files.delete(pending)
        durability.force(directory)
    }

    private fun preparesRetry(previous: VaultPinDelay, next: VaultPinDelay): Boolean {
        // Match the only two transitions the controller's charged retry can write.
        val rebased = next.started.boot != previous.started.boot ||
            next.started.elapsedMillis < previous.started.elapsedMillis
        if (rebased) return next.strike == previous.strike
        val charged = (previous.strike + 1).coerceAtMost(MaxVaultPinStrikes)
        return next.strike == charged &&
            next.started.elapsedMillis - previous.started.elapsedMillis >= previous.durationMillis
    }

    fun readExistingUiaPinSlot(mode: UiaRoomPinMode = UiaRoomPinMode.OpenExisting): UiaRoomPinSlot = locked {
        check(mode != UiaRoomPinMode.ProvisionNew)
        val record = load()
        checkPhase(record, mode)
        UiaRoomPinSlot(record, readPinCiphertext(mode))
    }

    /**
     * Hold the stable Java lock through the synchronous native call and verify the exact
     * reserved record and ciphertext. Native may read state but must not open/lock/close
     * the same lock inode; POSIX locks are process-owned and could otherwise be released.
     */
    fun <T> withNativeRoomAuthority(
        slot: UiaRoomPinSlot,
        mode: UiaRoomPinMode,
        biometricCiphertext: ByteArray? = null,
        block: () -> T,
    ): T = locked {
        if (mode == UiaRoomPinMode.OpenExistingBiometric) {
            if (biometricCiphertext == null) unavailable()
            val currentBiometric = readBiometricCiphertext()
            try {
                if (!MessageDigest.isEqual(currentBiometric, biometricCiphertext)) unavailable()
            } finally { currentBiometric.fill(0) }
        } else if (biometricCiphertext != null) unavailable()
        val record = load()
        if (record != slot.record) unavailable()
        checkPhase(record, mode)
        val current = readPinCiphertext(mode)
        try {
            if (!MessageDigest.isEqual(current, slot.ciphertext)) unavailable()
            block()
        } finally {
            current.fill(0)
        }
    }

    private fun checkPhase(record: VaultPinRecord, mode: UiaRoomPinMode) {
        if (attributes(directory.resolve(NativeEnrollmentRetired)) != null) unavailable()
        val allowed = when (mode) {
            UiaRoomPinMode.ProvisionNew -> record.registration === VaultPinRegistration.Provisioning
            UiaRoomPinMode.ResumeEnrollment -> record.registration is VaultPinRegistration.Recovering
            UiaRoomPinMode.OpenExisting -> (record.registration as? VaultPinRegistration.Ready)?.delay != null
            UiaRoomPinMode.OpenExistingBiometric -> record.registration is VaultPinRegistration.Ready
        }
        if (!allowed) unavailable()
    }

    private fun requireNoPinJournal(mode: UiaRoomPinMode) {
        if (attributes(directory.resolve(PinJournalFile)) != null || attributes(directory.resolve(PinNextFile)) != null) {
            pinSlotUnavailable(mode)
        }
    }

    private fun readPinCiphertext(mode: UiaRoomPinMode): ByteArray {
        requireNoPinJournal(mode)
        val file = directory.resolve(PinFile)
        val info = attributes(file) ?: pinSlotUnavailable(mode)
        if (!info.isRegularFile || info.permissions() != FileMode || info.size() != VaultPinSlotBytes.toLong()) {
            pinSlotUnavailable(mode)
        }
        val bytes = readExact(file, VaultPinSlotBytes)
        if (bytes[0] != 1.toByte()) pinSlotUnavailable(mode)
        return bytes
    }

    private fun pinSlotUnavailable(mode: UiaRoomPinMode): Nothing = when (mode) {
        UiaRoomPinMode.ProvisionNew -> unavailable()
        UiaRoomPinMode.ResumeEnrollment, UiaRoomPinMode.OpenExisting,
        UiaRoomPinMode.OpenExistingBiometric -> throw UiaRoomPinSlotDamaged()
    }

    /**
     * An explicit first opt-in may claim only a complete mandatory PIN enrollment.
     * A leftover claim or established biometric ciphertext is never treated as a missing key
     * to regenerate. Only explicit pre-approval cancellation can release its own fresh claim.
     */
    fun <T> claimUiaBiometricOptIn(provision: (ByteArray) -> T): T = locked {
        requireBiometricReady()
        if (biometricAvailabilityLocked() != VaultBiometricAvailability.Eligible) {
            throw VaultBiometricException(VaultBiometricFailure.InterruptedOptIn)
        }
        val claimId = UUID.randomUUID()
        val claim = ByteBuffer.allocate(BioClaimBytes)
            .putLong(claimId.mostSignificantBits).putLong(claimId.leastSignificantBits).array()
        var retainedByPrompt = false
        try {
            createPrivateFile(directory.resolve(BioClaimFile), claim)
            durability.force(directory)
            // A clean cancellation receipt is consumed only after this new claim is durable.
            val cancelled = directory.resolve(BioCancelledFile)
            if (attributes(cancelled) != null) {
                verifyFile(cancelled, BioClaimBytes.toLong())
                Files.delete(cancelled)
                durability.force(directory)
            }
            // No process may revoke, confirm key deletion, and race a late key generation.
            provision(claim).also { retainedByPrompt = true }
        } finally {
            if (!retainedByPrompt) claim.fill(0)
        }
    }

    /**
     * Release only the exact just-created claim, and only after the worker has proved and
     * deleted its exact uncommitted optional key. No original enrollment bytes are changed.
     * A durable receipt precedes removing the claim: after a crash before that, the remaining
     * claim still blocks opt-in; after removal and correct key absence, fresh opt-in is safe.
     */
    fun cancelUncommittedBiometricOptIn(claim: ByteArray, deleteExactFreshAlias: () -> Boolean) = locked {
        requireBiometricReady()
        if (claim.size != BioClaimBytes ||
            listOf(BioFile, BioNextFile, BioRevokedFile, BioCleanupFile, BioCancelledFile).any {
                attributes(directory.resolve(it)) != null
            }
        ) unavailable()
        val marker = directory.resolve(BioClaimFile)
        verifyFile(marker, BioClaimBytes.toLong())
        val staged = readExact(marker, BioClaimBytes)
        try {
            if (!MessageDigest.isEqual(staged, claim)) unavailable()
        } finally { staged.fill(0) }
        if (!deleteExactFreshAlias()) return@locked
        createPrivateFile(directory.resolve(BioCancelledFile), claim)
        durability.force(directory)
        Files.delete(marker)
        durability.force(directory)
    }

    /** Status is not an unlock or an assertion from Android that its key is usable. */
    fun uiaBiometricAvailability(): VaultBiometricAvailability = locked {
        requireBiometricReady()
        biometricAvailabilityLocked()
    }

    private fun biometricAvailabilityLocked(): VaultBiometricAvailability {
        val revoked = directory.resolve(BioRevokedFile)
        val cleanup = directory.resolve(BioCleanupFile)
        if (attributes(revoked) != null) {
            verifyFile(revoked, 0)
            if (attributes(cleanup) == null) return VaultBiometricAvailability.RevocationPending
            verifyFile(cleanup, 0)
            return VaultBiometricAvailability.Revoked
        }
        if (attributes(cleanup) != null) unavailable()
        if (attributes(directory.resolve(BioClaimFile)) != null ||
            attributes(directory.resolve(BioNextFile)) != null
        ) return VaultBiometricAvailability.Interrupted
        if (attributes(directory.resolve(BioFile)) == null) {
            val cancelled = directory.resolve(BioCancelledFile)
            if (attributes(cancelled) != null) verifyFile(cancelled, BioClaimBytes.toLong())
            return VaultBiometricAvailability.Eligible
        }
        return try {
            readBiometricCiphertext().fill(0)
            VaultBiometricAvailability.Enrolled
        } catch (_: IOException) {
            VaultBiometricAvailability.Interrupted
        }
    }

    /**
     * Logical revocation is durable before deleting the exact alias. Retain ciphertext and all
     * staging evidence. Cleanup failure cannot restore authority; a later fresh PIN can retry it.
     */
    fun revokeUiaBiometricSlot(pinnedPin: UiaRoomPinSlot, deleteExactAlias: () -> Boolean): VaultBiometricAvailability = locked {
        verifyBiometricPinSnapshot(pinnedPin)
        when (biometricAvailabilityLocked()) {
            VaultBiometricAvailability.Enrolled, VaultBiometricAvailability.Interrupted -> {
                createPrivateFile(directory.resolve(BioRevokedFile))
            }
            VaultBiometricAvailability.RevocationPending -> Unit
            VaultBiometricAvailability.Revoked -> return@locked VaultBiometricAvailability.Revoked
            else -> unavailable()
        }
        // Ensure any marker from an earlier uncertain attempt is durable, too.
        durability.force(directory)
        try {
            if (!deleteExactAlias()) return@locked VaultBiometricAvailability.RevocationPending
            createPrivateFile(directory.resolve(BioCleanupFile))
            durability.force(directory)
            VaultBiometricAvailability.Revoked
        } catch (_: Exception) {
            VaultBiometricAvailability.RevocationPending
        }
    }

    /** Only after a fresh actual PIN-authenticated open and the exact approved Android Cipher. */
    fun publishUiaBiometricSlot(claim: ByteArray, pinnedPin: UiaRoomPinSlot, ciphertext: ByteArray) = locked {
        verifyBiometricPinSnapshot(pinnedPin)
        if (claim.size != BioClaimBytes || ciphertext.size != VaultBiometricSlotBytes || ciphertext[0] != 1.toByte()) {
            unavailable()
        }
        requireNotBiometricallyRevoked()
        val marker = directory.resolve(BioClaimFile)
        val next = directory.resolve(BioNextFile)
        val target = directory.resolve(BioFile)
        if (attributes(target) != null || attributes(next) != null ||
            attributes(directory.resolve(BioCancelledFile)) != null
        ) unavailable()
        verifyFile(marker, BioClaimBytes.toLong())
        if (!MessageDigest.isEqual(readExact(marker, BioClaimBytes), claim)) unavailable()
        createPrivateFile(next, ciphertext)
        Files.move(next, target, ATOMIC_MOVE)
        durability.force(directory)
        Files.delete(marker)
        durability.force(directory)
    }

    /** Does not alter the PIN strike/delay. Starting an optional prompt isn't a PIN attempt. */
    fun readUiaBiometricSlot(): ByteArray = locked {
        requireBiometricReady()
        readBiometricCiphertext()
    }

    fun requireSameUiaBiometricSlot(pinnedPin: UiaRoomPinSlot, expected: ByteArray) = locked {
        verifyBiometricPinSnapshot(pinnedPin, UiaRoomPinMode.OpenExistingBiometric)
        val current = readBiometricCiphertext()
        try {
            if (!MessageDigest.isEqual(current, expected)) unavailable()
        } finally {
            current.fill(0)
        }
    }

    private fun requireBiometricReady() {
        if (load().registration !is VaultPinRegistration.Ready) unavailable()
        readPinCiphertext(UiaRoomPinMode.OpenExisting).fill(0)
    }

    private fun verifyBiometricPinSnapshot(pinnedPin: UiaRoomPinSlot, mode: UiaRoomPinMode = UiaRoomPinMode.OpenExisting) {
        val record = load()
        if (record != pinnedPin.record) unavailable()
        checkPhase(record, mode)
        val current = readPinCiphertext(mode)
        try {
            if (!MessageDigest.isEqual(current, pinnedPin.ciphertext)) unavailable()
        } finally {
            current.fill(0)
        }
    }

    private fun requireNotBiometricallyRevoked() {
        if (attributes(directory.resolve(BioRevokedFile)) != null || attributes(directory.resolve(BioCleanupFile)) != null) {
            throw VaultBiometricException(VaultBiometricFailure.Revoked)
        }
    }

    private fun readBiometricCiphertext(): ByteArray {
        requireNotBiometricallyRevoked()
        if (attributes(directory.resolve(BioClaimFile)) != null || attributes(directory.resolve(BioNextFile)) != null ||
            attributes(directory.resolve(BioCancelledFile)) != null
        ) {
            throw VaultBiometricException(VaultBiometricFailure.InterruptedOptIn)
        }
        val file = directory.resolve(BioFile)
        verifyFile(file, VaultBiometricSlotBytes.toLong())
        val bytes = readExact(file, VaultBiometricSlotBytes)
        if (bytes[0] != 1.toByte()) unavailable()
        return bytes
    }

    private fun <T> locked(block: () -> T): T = synchronized(ProcessLock) {
        verifyDirectory(parent, requirePrivate = false)
        verifyDirectory(namespace)
        verifyDirectory(directory)
        val lockPath = directory.resolve(LockFile)
        verifyFile(lockPath, size = 0)
        FileChannel.open(lockPath, WRITE, NOFOLLOW_LINKS).use { channel ->
            // No indefinite wait on a stuck process. Conflict leaves the ledger unchanged.
            val lock = channel.tryLock() ?: unavailable()
            lock.use { block() }
        }
    }

    private fun load(): VaultPinRecord {
        requireNoJournal()
        return loadInstalled()
    }

    private fun loadInstalled(): VaultPinRecord {
        val file = directory.resolve(StateFile)
        verifyFile(file, size = EncodedBytes.toLong())
        return decode(readExact(file, EncodedBytes), scope)
    }

    private fun requireNoJournal() {
        if (attributes(directory.resolve(JournalFile)) != null || attributes(directory.resolve(NextFile)) != null) {
            unavailable()
        }
    }

    private fun install(record: VaultPinRecord) {
        requireNoJournal()
        val journal = directory.resolve(JournalFile)
        val next = directory.resolve(NextFile)
        createPrivateFile(journal)
        durability.force(directory)
        createPrivateFile(next, encode(record))
        // No fallback for providers that don't implement atomic rename.
        Files.move(next, directory.resolve(StateFile), ATOMIC_MOVE, REPLACE_EXISTING)
        durability.force(directory)
        // If a sync before this point failed, the durable pending marker is left untouched.
        Files.delete(journal)
        durability.force(directory)
    }

    companion object {
        /** Reopen only the registry-selected existing scope. A missing file never initializes anything. */
        fun open(parent: Path, scope: VaultPinScope, durability: VaultPinDirectorySync): VaultPinDiskStateStore =
            VaultPinDiskStateStore(parent, scope, durability).also { it.read() }

        /** Only the native-selected local Assistant startup may replay a known final PIN ledger. */
        fun reopenAssistantEnrollment(parent: Path, scope: VaultPinScope, durability: VaultPinDirectorySync): VaultPinDiskStateStore =
            VaultPinDiskStateStore(parent, scope, durability).also { it.recoverFinalEnrollmentJournal() }

        /**
         * Explicit new enrollment only. Allocates a *new* random UUID and an exclusive directory;
         * cannot be called with an existing identity, and never deletes partial setup.
         * A higher-level profile registry must not call this as recovery when open() fails.
         */
        fun createNew(parent: Path, durability: VaultPinDirectorySync): VaultPinDiskStateStore {
            verifyDirectory(parent, requirePrivate = false)
            val scope = VaultPinScope(UUID.randomUUID(), 1)
            val store = VaultPinDiskStateStore(parent, scope, durability)
            try {
                Files.createDirectory(store.namespace, PosixFilePermissions.asFileAttribute(DirectoryMode))
            } catch (_: FileAlreadyExistsException) {
                // A cooperating process may have created the common namespace.
            }
            verifyDirectory(store.namespace)
            durability.force(parent)
            Files.createDirectory(store.directory, PosixFilePermissions.asFileAttribute(DirectoryMode))
            durability.force(store.namespace)
            createPrivateFile(store.directory.resolve(LockFile))
            durability.force(store.directory)
            store.locked { store.install(VaultPinRecord(scope, 0, VaultPinRegistration.Fresh)) }
            return store
        }
    }
}

private fun encode(record: VaultPinRecord): ByteArray {
    val delay = when (val registration = record.registration) {
        is VaultPinRegistration.Ready -> registration.delay
        is VaultPinRegistration.Recovering -> registration.delay
        else -> null
    }
    val kind: Byte = when (record.registration) {
        VaultPinRegistration.Fresh -> 0
        VaultPinRegistration.Provisioning -> 1
        is VaultPinRegistration.Ready -> 2
        is VaultPinRegistration.Recovering -> 3
    }
    val payload = ByteBuffer.allocate(PayloadBytes)
        .putInt(Magic)
        .put(FormatVersion)
        .putLong(record.scope.vaultId.mostSignificantBits)
        .putLong(record.scope.vaultId.leastSignificantBits)
        .putLong(record.scope.keyGeneration)
        .putLong(record.revision)
        .put(kind)
        .put((delay?.strike ?: 0).toByte())
        .putLong(delay?.started?.boot ?: 0)
        .putLong(delay?.started?.elapsedMillis ?: 0)
        .array()
    return payload + sha256(payload)
}

private fun decode(encoded: ByteArray, scope: VaultPinScope): VaultPinRecord {
    val payload = encoded.copyOfRange(0, PayloadBytes)
    if (!MessageDigest.isEqual(sha256(payload), encoded.copyOfRange(PayloadBytes, EncodedBytes))) unavailable()
    val data = ByteBuffer.wrap(payload)
    if (data.int != Magic || data.get() != FormatVersion) unavailable()
    if (UUID(data.long, data.long) != scope.vaultId || data.long != scope.keyGeneration) unavailable()
    val revision = data.long
    val kind = data.get().toInt()
    val strike = data.get().toInt()
    val boot = data.long
    val elapsed = data.long
    val uncharged = strike == 0 && boot == 0L && elapsed == 0L
    val registration = when {
        kind == 0 && revision == 0L && scope.keyGeneration == 1L && uncharged -> VaultPinRegistration.Fresh
        kind == 1 && revision == 1L && uncharged -> VaultPinRegistration.Provisioning
        kind == 2 && revision >= 2L && uncharged -> VaultPinRegistration.Ready()
        kind == 2 && revision >= 3L && strike > 0 && boot >= 0 && elapsed >= 0 -> try {
            VaultPinRegistration.Ready(VaultPinDelay(strike, VaultPinTime(boot, elapsed)))
        } catch (_: IllegalArgumentException) {
            unavailable()
        }
        kind == 3 && revision >= 2L && scope.keyGeneration == 1L && strike > 0 && boot >= 0 && elapsed >= 0 -> try {
            VaultPinRegistration.Recovering(VaultPinDelay(strike, VaultPinTime(boot, elapsed)))
        } catch (_: IllegalArgumentException) {
            unavailable()
        }
        else -> unavailable()
    }
    return VaultPinRecord(scope, revision, registration)
}

private fun sha256(bytes: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(bytes)

private fun createPrivateFile(path: Path, contents: ByteArray = ByteArray(0)) {
    FileChannel.open(path, setOf(CREATE_NEW, WRITE, NOFOLLOW_LINKS), PosixFilePermissions.asFileAttribute(FileMode)).use { channel ->
        val bytes = ByteBuffer.wrap(contents)
        while (bytes.hasRemaining()) if (channel.write(bytes) <= 0) unavailable()
        channel.force(true)
    }
}

private fun readExact(file: Path, size: Int): ByteArray {
    val result = ByteArray(size)
    FileChannel.open(file, READ, NOFOLLOW_LINKS).use { channel ->
        val bytes = ByteBuffer.wrap(result)
        while (bytes.hasRemaining()) if (channel.read(bytes) <= 0) unavailable()
        bytes.clear().limit(1)
        if (channel.read(bytes) != -1) unavailable()
    }
    return result
}

private fun attributes(path: Path): PosixFileAttributes? = try {
    Files.readAttributes(path, PosixFileAttributes::class.java, NOFOLLOW_LINKS)
} catch (_: NoSuchFileException) {
    null
}

private fun verifyDirectory(path: Path, requirePrivate: Boolean = true) {
    val info = attributes(path) ?: unavailable()
    if (!path.isAbsolute || !info.isDirectory || (requirePrivate && info.permissions() != DirectoryMode)) unavailable()
}

private fun verifyFile(path: Path, size: Long) {
    val info = attributes(path) ?: unavailable()
    if (!info.isRegularFile || info.permissions() != FileMode || info.size() != size) unavailable()
}

private fun unavailable(): Nothing = throw IOException("Vault PIN metadata is unavailable")
