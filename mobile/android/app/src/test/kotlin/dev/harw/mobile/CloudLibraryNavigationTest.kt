package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test

class CloudLibraryNavigationTest {
    private val gateway = CloudLibraryTarget("cloud-a", 7L, "authority-a")
    private val otherProfile = CloudLibraryTarget("cloud-b", 7L, "authority-a")
    private val otherLink = CloudLibraryTarget("cloud-a", 8L, "authority-a")
    private val otherAuthority = CloudLibraryTarget("cloud-a", 7L, "authority-b")

    private fun launched() = CloudLibraryNavigation<String, String, String>()
        .resume().open(gateway).beginPick(41uL)

    @Test fun pickerPauseHidesContentAndSameVisitGetsOnlyTheOnceConsumedSelection() {
        val open = launched()
        val visit = requireNotNull(open.visible)
        val waiting = open.pause()
        assertNull(waiting.visible)
        assertSame(visit, waiting.visit)
        val returned = waiting.pickResult("content://photo", gateway)
        assertNull(returned.visible)
        val foreground = returned.resume()
        assertSame(visit, foreground.visible)
        val photo = requireNotNull(foreground.photo)
        assertEquals(41uL, photo.revision)
        assertEquals("content://photo", photo.selected)
        assertNull(foreground.consume(visit, photo).photo)
    }

    @Test fun expiredResultCannotAttachToNewVisitForTheSameServer() {
        val old = launched()
        val oldVisit = requireNotNull(old.visible)
        val reopened = old.close().open(gateway)
        assertNotSame(oldVisit, reopened.visible)
        // Android supplies no ticket in its result. A fresh launch must wait
        // for the old activity result before it can reserve another slot.
        assertSame(reopened, reopened.beginPick(42uL))
        val returned = reopened.pickResult("content://old", gateway)
        assertNull(returned.photo)
        assertEquals(42uL, returned.beginPick(42uL).request?.revision)
    }

    @Test fun backendProfileLinkAndAuthorityChangesDropThePendingResult() {
        for (replacement in listOf(null, otherProfile, otherLink, otherAuthority)) {
            val old = launched().pause()
            val returned = old.retain(replacement).pickResult("content://old", replacement).resume()
            assertNull(returned.visible)
            assertNull(returned.photo)
            assertNull(returned.request)
        }
    }

    @Test fun unrelatedPauseClosesRouteAndDoesNotReopenAfterResume() {
        val open = CloudLibraryNavigation<String, String, String>().resume().open(gateway)
        assertNull(open.pause().resume().visible)
        val returned = launched().pause().pickResult("content://photo", gateway).resume()
        assertNull(returned.pause().resume().visible)
        assertNull(returned.pause().resume().photo)
    }

    @Test fun pickerCancellationReturnsToSameVisitWithoutPayload() {
        val open = launched()
        val returned = open.pause().pickResult(null, gateway).resume()
        assertSame(open.visible, returned.visible)
        assertNull(returned.photo)
        assertNull(returned.request)
    }
}
