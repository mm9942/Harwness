package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.harw_mobile_ffi.ConnectionPhase

class ConnectivityTest {
    private fun signal(
        phase: ConnectionPhase,
        stopping: Boolean = false,
        forgetting: Boolean = false,
        hasProfile: Boolean = true,
    ) = networkSignal(phase, stopping, forgetting, hasProfile)

    @Test
    fun noSignalWhenStoppingForgettingOrWithoutProfile() {
        for (phase in ConnectionPhase.values()) {
            assertEquals(NetworkSignal.None, signal(phase, stopping = true))
            assertEquals(NetworkSignal.None, signal(phase, forgetting = true))
            assertEquals(NetworkSignal.None, signal(phase, hasProfile = false))
        }
    }

    @Test
    fun nudgesWhileConnectingOrReconnecting() {
        assertEquals(NetworkSignal.Nudge, signal(ConnectionPhase.CONNECTING))
        assertEquals(NetworkSignal.Nudge, signal(ConnectionPhase.RECONNECTING))
    }

    @Test
    fun restartsAfterFailure() {
        assertEquals(NetworkSignal.Restart, signal(ConnectionPhase.FAILED))
    }

    @Test
    fun noSignalWhenConnectedDisconnectedOrRevoked() {
        val phases = listOf(
            ConnectionPhase.CONNECTED,
            ConnectionPhase.DISCONNECTED,
            ConnectionPhase.REVOKED,
        )
        for (phase in phases) {
            for (stopping in listOf(false, true)) {
                for (forgetting in listOf(false, true)) {
                    for (hasProfile in listOf(false, true)) {
                        assertEquals(
                            NetworkSignal.None,
                            networkSignal(phase, stopping, forgetting, hasProfile),
                        )
                    }
                }
            }
        }
    }

    @Test
    fun reconnectAttemptArgOmitsZero() {
        assertNull(reconnectAttemptArg(0u))
    }

    @Test
    fun reconnectAttemptArgPassesAttemptNumber() {
        assertEquals(1, reconnectAttemptArg(1u))
        assertEquals(8, reconnectAttemptArg(8u))
    }
}
