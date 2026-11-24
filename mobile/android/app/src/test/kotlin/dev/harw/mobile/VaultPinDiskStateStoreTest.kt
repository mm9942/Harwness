package dev.harw.mobile

import java.io.File
import java.io.IOException
import java.nio.channels.FileChannel
import java.security.MessageDigest
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption
import java.nio.file.attribute.PosixFilePermissions
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class VaultPinDiskStateStoreTest {
    @Test
    fun explicitCreationAndAtomicCasSurviveReopenWithoutAllowingStaleOrIllegalReset() = directory { root ->
        val new = VaultPinDiskStateStore.createNew(root, hostSync)
        val initial = new.read()
        assertEquals(VaultPinRegistration.Fresh, initial.registration)
        val scoped = VaultPinDiskStateStore.open(root, initial.scope, hostSync)
        val claimed = initial.copy(revision = 1, registration = VaultPinRegistration.Provisioning)
        assertTrue(new.replace(initial, claimed))
        assertFalse(scoped.replace(initial, claimed))
        assertEquals(claimed, scoped.read())
        assertThrows(IOException::class.java) { scoped.replace(claimed, initial.copy(revision = 2)) }
        val ready = claimed.copy(revision = 2, registration = VaultPinRegistration.Ready())
        assertTrue(scoped.replace(claimed, ready))
        val charged = ready.copy(revision = 3, registration = VaultPinRegistration.Ready(VaultPinDelay(1, VaultPinTime(35, 20_000))))
        assertTrue(new.replace(ready, charged))
        assertEquals(charged, VaultPinDiskStateStore.open(root, initial.scope, hostSync).read())
        assertEquals("rwx------", PosixFilePermissions.toString(Files.getPosixFilePermissions(vaultDir(root, initial))))
        assertEquals("rw-------", PosixFilePermissions.toString(Files.getPosixFilePermissions(vaultDir(root, initial).resolve("state.bin"))))
    }

    @Test
    fun missingOrWrongScopeIsNotFreshAndCreatesNothing() = directory { root ->
        val foreign = VaultPinScope(UUID.randomUUID(), 1)
        assertThrows(IOException::class.java) { VaultPinDiskStateStore.open(root, foreign, hostSync) }
        assertEquals(0L, Files.list(root).use { it.count() })
        val new = VaultPinDiskStateStore.createNew(root, hostSync)
        val initial = new.read()
        assertThrows(IOException::class.java) {
            VaultPinDiskStateStore.open(root, initial.scope.copy(keyGeneration = 2), hostSync)
        }
        val state = vaultDir(root, initial).resolve("state.bin")
        Files.delete(state)
        assertThrows(IOException::class.java) { new.read() }
        assertFalse(Files.exists(state))
    }

    @Test
    fun checksumSizeAndOrphanJournalFailClosedWithoutChangingFiles() = directory { root ->
        val store = VaultPinDiskStateStore.createNew(root, hostSync)
        val record = store.read()
        val dir = vaultDir(root, record)
        val state = dir.resolve("state.bin")
        val initial = Files.readAllBytes(state)
        val corrupt = initial.copyOf().apply { this[32] = (this[32].toInt() xor 1).toByte() }
        for (bad in listOf(corrupt, initial.copyOf(initial.size + 1), initial.copyOf(4))) {
            Files.write(state, bad)
            assertThrows(IOException::class.java) { store.read() }
            assertTrue(Files.readAllBytes(state).contentEquals(bad))
        }
        Files.write(state, initial)
        Files.createFile(dir.resolve("state.pending"))
        assertThrows(IOException::class.java) { store.read() }
        assertTrue(Files.exists(dir.resolve("state.pending")))
    }

    @Test
    fun validChecksumDoesNotPermitUnknownVersionOrNoncanonicalMetadata() = directory { root ->
        val store = VaultPinDiskStateStore.createNew(root, hostSync)
        val initial = store.read()
        val state = vaultDir(root, initial).resolve("state.bin")
        val payload = Files.readAllBytes(state).copyOfRange(0, 55)
        for ((offset, value) in listOf(4 to 99, 36 to 1, 37 to 2, 38 to 1)) {
            val incorrect = payload.copyOf().apply { this[offset] = value.toByte() }
            val digest = MessageDigest.getInstance("SHA-256").digest(incorrect)
            Files.write(state, incorrect + digest)
            assertThrows(IOException::class.java) { store.read() }
        }
    }

    @Test
    fun writeFailureAfterRenameRetainsQuarantineAndNeverAcknowledgesCas() = directory { root ->
        var failAfterRename = false
        val sync = VaultPinDirectorySync { dir ->
            if (failAfterRename && Files.exists(dir.resolve("state.pending")) && !Files.exists(dir.resolve("state.next"))) {
                // The first match is the prior durable journal sync, the second is after rename.
                if (Files.readAllBytes(dir.resolve("state.bin"))[37] == 1.toByte()) throw IOException("synthetic fsync failure")
            }
            hostSync.force(dir)
        }
        val store = VaultPinDiskStateStore.createNew(root, sync)
        val record = store.read()
        failAfterRename = true
        val next = record.copy(revision = 1, registration = VaultPinRegistration.Provisioning)
        assertThrows(IOException::class.java) { store.replace(record, next) }
        assertTrue(Files.exists(vaultDir(root, record).resolve("state.pending")))
        assertThrows(IOException::class.java) { VaultPinDiskStateStore.open(root, record.scope, hostSync) }
    }

    @Test
    fun aSeparateJvmHoldingTheStableLockDeniesAccessUntilRelease() = directory { root ->
        val store = VaultPinDiskStateStore.createNew(root, hostSync)
        val record = store.read()
        val lock = vaultDir(root, record).resolve("state.lock")
        val javaExe = Path.of(System.getProperty("java.home"), "bin", "java").toString()
        val peerClass = VaultPinLockPeer::class.java
        val peerClasspath = listOf(peerClass, Unit::class.java).joinToString(File.pathSeparator) {
            val location = requireNotNull(it.protectionDomain?.codeSource?.location)
            Path.of(location.toURI()).toString()
        }
        val peer = ProcessBuilder(javaExe, "-cp", peerClasspath, peerClass.name, lock.toString())
            .redirectErrorStream(true).start()
        try {
            val readiness = CompletableFuture.supplyAsync { peer.inputStream.bufferedReader().readLine() }
            assertEquals("ready", readiness.get(5, TimeUnit.SECONDS))
            assertThrows(IOException::class.java) { store.read() }
            assertThrows(IOException::class.java) {
                store.replace(record, record.copy(revision = 1, registration = VaultPinRegistration.Provisioning))
            }
        } finally {
            peer.outputStream.close()
            if (!peer.waitFor(3, TimeUnit.SECONDS)) peer.destroyForcibly()
        }
        assertEquals(record, store.read())
    }

    @Test
    fun symlinkAndUnsafeDirectoryModesFailWithoutFollowing() = directory { root ->
        val store = VaultPinDiskStateStore.createNew(root, hostSync)
        val record = store.read()
        val dir = vaultDir(root, record)
        val state = dir.resolve("state.bin")
        val other = root.resolve("not-vault")
        Files.move(state, other)
        Files.createSymbolicLink(state, other)
        assertThrows(IOException::class.java) { store.read() }
        assertTrue(Files.exists(other))
        Files.delete(state)
        Files.move(other, state)
        Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwxrwx---"))
        assertThrows(IOException::class.java) { store.read() }
    }

    @Test
    fun monotonicClockRequiresStableBootIdAndPropagatesUnavailable() {
        assertEquals(VaultPinTime(42, 10), stableVaultPinTime({ 42 }, { 10L }))
        val boots = ArrayDeque(listOf(42, 43))
        assertThrows(IOException::class.java) { stableVaultPinTime({ boots.removeFirst() }, { 10L }) }
        assertThrows(IOException::class.java) { stableVaultPinTime({ -1 }, { 10L }) }
        assertThrows(IOException::class.java) { stableVaultPinTime({ 42 }, { -1L }) }
        assertThrows(IOException::class.java) { stableVaultPinTime({ throw IOException("absent") }, { 10L }) }
    }

    private fun vaultDir(root: Path, record: VaultPinRecord): Path =
        root.resolve("harw-vault-pin-v1").resolve(record.scope.vaultId.toString())

    private fun directory(block: (Path) -> Unit) {
        val dir = Files.createTempDirectory("vault-pin-metadata-test")
        try { block(dir) } finally { dir.toFile().deleteRecursively() }
    }

    private val hostSync = VaultPinDirectorySync { dir ->
        FileChannel.open(dir, StandardOpenOption.READ).use { it.force(true) }
    }
}

object VaultPinLockPeer {
    @JvmStatic fun main(args: Array<String>) {
        FileChannel.open(Path.of(args[0]), StandardOpenOption.WRITE).use { channel ->
            channel.lock().use {
                println("ready")
                System.out.flush()
                System.`in`.read()
            }
        }
    }
}
