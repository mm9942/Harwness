package dev.harw.mobile

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CoreEventsTest {
    @Test
    fun statusSignalWakesWaiter() {
        val events = CoreEvents()
        events.statusChanged()
        assertTrue(runBlocking { events.awaitStatus(1_000) })
        events.statusChanged()
        assertTrue(runBlocking { events.awaitStatus(1_000) })
        assertFalse(runBlocking { events.awaitStatus(50) })
    }

    @Test
    fun awaitTimesOutWithoutSignal() {
        val events = CoreEvents()
        assertFalse(runBlocking { events.awaitStatus(50) })
        assertFalse(runBlocking { events.awaitSessions(50) })
    }

    @Test
    fun sessionChangeRecordsIdAndSignalsStatus() {
        val events = CoreEvents()
        events.sessionChanged("s1")
        assertTrue(runBlocking { events.awaitStatus(1_000) })
        assertEquals(setOf("s1"), events.takeChangedSessions())
        assertFalse(runBlocking { events.awaitSessions(50) })
    }

    @Test
    fun takeChangedSessionsDrains() {
        val events = CoreEvents()
        events.sessionChanged("a")
        events.sessionChanged("b")
        assertEquals(setOf("a", "b"), events.takeChangedSessions())
        assertTrue(events.takeChangedSessions().isEmpty())
    }

    @Test
    fun repeatedStatusSignalsCoalesceIntoOneWake() {
        val events = CoreEvents()
        events.statusChanged()
        events.statusChanged()
        assertTrue(runBlocking { events.awaitStatus(1_000) })
        assertFalse(runBlocking { events.awaitStatus(50) })
    }

    @Test
    fun sessionsSignalWakesSessionsWaiter() {
        val events = CoreEvents()
        events.sessionsChanged()
        assertTrue(runBlocking { events.awaitSessions(1_000) })
        assertFalse(runBlocking { events.awaitSessions(50) })
    }
}
