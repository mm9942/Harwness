package dev.harw.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SessionActionGateTest {
    @Test
    fun repeatedTapsInTwoWindowsCannotSubmitTheSameSessionTwice() {
        val gate = SessionActionGate()
        val generation = gate.open()
        val send = SessionAction("session-a", SessionActionKind.Send)
        val ticket = checkNotNull(gate.begin(generation, send))
        assertNull(gate.begin(generation, send))
        assertNotNull(gate.begin(generation, SessionAction("session-b", SessionActionKind.Send)))
        assertTrue(gate.finish(ticket))
        assertNotNull(gate.begin(generation, send))
    }

    @Test
    fun reconnectRejectsActionsFromPriorSnapshotsAndIgnoresPriorCompletions() {
        val gate = SessionActionGate()
        val prior = gate.open()
        val decision = SessionAction("session-a", SessionActionKind.Decide)
        val obsolete = checkNotNull(gate.begin(prior, decision))
        gate.close()
        assertNull(gate.begin(prior, decision))
        val current = gate.open()
        assertNull(gate.begin(prior, decision))
        assertNotNull(gate.begin(current, decision))
        assertFalse(gate.finish(obsolete))
        assertNull(gate.begin(current, decision))
    }
}
