package dev.harw.mobile

import java.io.IOException
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest
import java.util.UUID

internal class LocalUiaProfile private constructor(val key: String) {
    override fun equals(other: Any?): Boolean = other is LocalUiaProfile && key == other.key
    override fun hashCode(): Int = key.hashCode()

    companion object {
        fun parse(value: String?): LocalUiaProfile? = value?.takeIf {
            it.length == 64 && it.all { ch -> ch in '0'..'9' || ch in 'a'..'f' }
        }?.let(::LocalUiaProfile)
    }
}

internal fun showLocalUiaDev(buildOptedIn: Boolean, localOnline: Boolean, profile: LocalUiaProfile?): Boolean =
    buildOptedIn && localOnline && profile != null

/** One app-private, non-backed-up durable map. Class does not provision on lookup or recover/rotate. */
internal class LocalUiaDevProfiles(private val noBackup: Path, private val forceDirectory: (Path) -> Unit) {
    // Retain the original namespace and tag domain so the production opt-in
    // reopens an enrolled local room instead of allocating another identity.
    private val namespace = noBackup.resolve("harw-local-uia-development-v1")
    private val dirs = PosixFilePermissions.fromString("rwx------")
    private val files = PosixFilePermissions.fromString("rw-------")

    fun lookup(profile: LocalUiaProfile): VaultPinScope? {
        requirePrivate(noBackup, strict = false)
        val entry = namespace.resolve(profile.key)
        if (absent(namespace)) return null
        requirePrivate(namespace)
        if (absent(entry)) return null
        requirePrivate(entry)
        // Once the profile entry exists, an interrupted or missing committed
        // map is recovery-only, not permission to allocate a new identity.
        if (!absent(entry.resolve("pending")) || !absent(entry.resolve("next"))) fail()
        val file = entry.resolve("scope")
        if (Files.getPosixFilePermissions(file, NOFOLLOW_LINKS) != files ||
            Files.getOwner(file, NOFOLLOW_LINKS) != Files.getOwner(noBackup, NOFOLLOW_LINKS) ||
            !Files.isRegularFile(file, NOFOLLOW_LINKS)
        ) fail()
        val wire = ByteArray(56)
        Files.newInputStream(file, READ, NOFOLLOW_LINKS).use { input ->
            var offset = 0
            while (offset < wire.size) {
                val count = input.read(wire, offset, wire.size - offset)
                if (count <= 0) fail()
                offset += count
            }
            if (input.read() != -1) fail()
        }
        val payload = wire.copyOfRange(0, 24)
        if (!MessageDigest.isEqual(tag(profile, payload), wire.copyOfRange(24, 56))) fail()
        val fields = ByteBuffer.wrap(payload)
        val uuid = UUID(fields.long, fields.long)
        val epoch = fields.long
        if (uuid.version() != 4 || uuid.variant() != 2 || epoch <= 0L) fail()
        // A previous replacement may have been renamed just before a power loss
        // or an uncertain directory sync. Never use it for enrollment until the
        // directory is durably synced on the reader side too.
        forceDirectory(entry)
        return VaultPinScope(uuid, epoch)
    }

    /** Explicit new-only flow; exclusively reserve the profile before allocating the platform ledger. */
    fun allocate(profile: LocalUiaProfile, newScope: () -> VaultPinScope): VaultPinScope {
        requirePrivate(noBackup, strict = false)
        if (absent(namespace)) {
            try { Files.createDirectory(namespace, PosixFilePermissions.asFileAttribute(dirs)) }
            catch (_: java.nio.file.FileAlreadyExistsException) { /* verify existing */ }
            forceDirectory(noBackup)
        }
        requirePrivate(namespace)
        val entry = namespace.resolve(profile.key)
        Files.createDirectory(entry, PosixFilePermissions.asFileAttribute(dirs))
        forceDirectory(namespace)
        val marker = entry.resolve("pending")
        write(marker, byteArrayOf())
        forceDirectory(entry)
        val fresh = newScope()
        if (fresh.vaultId.version() != 4 || fresh.vaultId.variant() != 2 || fresh.keyGeneration != 1L) fail()
        val payload = ByteBuffer.allocate(24)
            .putLong(fresh.vaultId.mostSignificantBits).putLong(fresh.vaultId.leastSignificantBits)
            .putLong(fresh.keyGeneration).array()
        val staged = entry.resolve("next")
        write(staged, payload + tag(profile, payload))
        Files.move(staged, entry.resolve("scope"), ATOMIC_MOVE)
        forceDirectory(entry)
        Files.delete(marker)
        forceDirectory(entry)
        return fresh
    }

    /**
     * Retire and reselect only under the old vault's platform ledger lock.
     * An old retained reader cannot publish a room after native retirement.
     * Nothing in the old vault, its PIN slot, or its Keystore is deleted.
     */
    fun replaceProvenUncommitted(
        profile: LocalUiaProfile,
        old: VaultPinDiskStateStore,
        stillCurrent: () -> Boolean,
        retireNative: () -> Boolean,
        fresh: () -> VaultPinScope,
    ): VaultPinScope? = old.withUncommittedEnrollment {
        if (!stillCurrent() || lookup(profile) != old.scope || !retireNative()) return@withUncommittedEnrollment null
        if (!stillCurrent() || lookup(profile) != old.scope) return@withUncommittedEnrollment null
        val created = fresh()
        if (created == old.scope || created.vaultId.version() != 4 ||
            created.vaultId.variant() != 2 || created.keyGeneration != 1L
        ) fail()
        if (!stillCurrent() || lookup(profile) != old.scope) return@withUncommittedEnrollment null
        val payload = ByteBuffer.allocate(24)
            .putLong(created.vaultId.mostSignificantBits).putLong(created.vaultId.leastSignificantBits)
            .putLong(created.keyGeneration).array()
        val entry = namespace.resolve(profile.key)
        val staged = entry.resolve("restart-${UUID.randomUUID()}")
        // An orphan staging file is non-authoritative. A crash before rename
        // leaves the non-reopenable old scope eligible for explicit retry;
        // after rename, lookup must successfully sync before using the new one.
        write(staged, payload + tag(profile, payload))
        Files.move(staged, entry.resolve("scope"), ATOMIC_MOVE, REPLACE_EXISTING)
        forceDirectory(entry)
        created
    }

    private fun tag(profile: LocalUiaProfile, payload: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").run {
        update("harw-local-uia-development-scope-v1".encodeToByteArray())
        update(profile.key.encodeToByteArray())
        digest(payload)
    }

    private fun write(path: Path, data: ByteArray) {
        FileChannel.open(path, setOf(CREATE_NEW, WRITE, NOFOLLOW_LINKS), PosixFilePermissions.asFileAttribute(files)).use {
            val source = ByteBuffer.wrap(data)
            while (source.hasRemaining()) if (it.write(source) <= 0) fail()
            it.force(true)
        }
    }

    private fun requirePrivate(path: Path, strict: Boolean = true) {
        if (!Files.isDirectory(path, NOFOLLOW_LINKS) ||
            Files.getOwner(path, NOFOLLOW_LINKS) != Files.getOwner(noBackup, NOFOLLOW_LINKS) ||
            (strict && Files.getPosixFilePermissions(path, NOFOLLOW_LINKS) != dirs)
        ) fail()
    }

    private fun absent(path: Path): Boolean = try {
        Files.readAttributes(path, java.nio.file.attribute.BasicFileAttributes::class.java, NOFOLLOW_LINKS)
        false
    } catch (_: java.nio.file.NoSuchFileException) {
        true
    }

    private fun fail(): Nothing = throw IOException("Local UIA registration requires recovery")
}

/** Small non-serializable route lifetime; any late result is revoked, never shown. */
internal class LocalUiaRouteFence {
    private var active = true
    @Synchronized fun active(): Boolean = active
    @Synchronized fun close() { active = false }
    @Synchronized fun admit(onLate: () -> Unit): Boolean {
        if (!active) { onLate(); return false }
        return true
    }
}
