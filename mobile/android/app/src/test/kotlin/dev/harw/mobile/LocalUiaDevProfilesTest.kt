package dev.harw.mobile

import java.nio.file.Files
import java.util.UUID
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class LocalUiaDevProfilesTest {
    @get:Rule val tmp = TemporaryFolder()
    private val local = LocalUiaProfile.parse("a".repeat(64))!!
    private val another = LocalUiaProfile.parse("b".repeat(64))!!

    @Test fun visibleOnlyWithDebugOptInAndVerifiedLocalRuntime() {
        assertFalse(showLocalUiaDev(false, true, local))
        assertFalse(showLocalUiaDev(true, false, local))
        assertFalse(showLocalUiaDev(true, true, null))
        assertTrue(showLocalUiaDev(true, true, local))
        assertNull(LocalUiaProfile.parse("../bad"))
    }

    @Test fun restartReopensOnlyTheRegisteredProfileAndNeverAllocates() {
        val base = tmp.root.toPath()
        val registry = LocalUiaDevProfiles(base) {}
        assertNull(registry.lookup(local))
        assertEquals(0, tmp.root.listFiles()!!.size)
        val expected = VaultPinScope(UUID.randomUUID(), 1)
        registry.allocate(local) { expected }
        val restarted = LocalUiaDevProfiles(base) {}
        assertEquals(expected, restarted.lookup(local))
        assertNull(restarted.lookup(another))
        assertThrows(Exception::class.java) { restarted.allocate(local) { error("no re-enrollment") } }
    }

    @Test fun interruptedRegistrationIsRecoveryOnly() {
        val set = LocalUiaDevProfiles(tmp.root.toPath()) {}
        assertThrows(Exception::class.java) { set.allocate(local) { throw java.io.IOException("interruption") } }
        assertThrows(Exception::class.java) { set.lookup(local) }
        assertThrows(Exception::class.java) { set.allocate(local) { error("cannot overwrite") } }
    }

    @Test fun backOrBackgroundRevokesLateSessionAndNeverRestoresIt() {
        for (exit in listOf("back", "pause")) {
            val lease = LocalUiaRouteFence()
            var revokes = 0
            lease.close()
            assertFalse(exit, lease.admit { revokes++ })
            assertEquals(1, revokes)
            assertFalse(lease.active())
        }
    }

    @Test fun cancellingPinErasesVolatileDraftAndDoesNotRestoreIt() {
        val digits = PinDigits()
        repeat(8) { digits.add(it) }
        assertEquals(8, digits.count)
        digits.clear()
        assertEquals(0, digits.count)
        assertTrue(digits.take().isEmpty())
    }
}
