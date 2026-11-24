package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Test

class KnowledgeAvailabilityTest {
    private val supported = KnowledgeCloudSupport(manage = true, observe = true)

    @Test
    fun noCloudProfileTakesPrecedenceOverCachedSupport() {
        assertEquals(
            KnowledgeAvailability.NoCloudProfile,
            knowledgeAvailability(false, online = true, connecting = false, support = supported),
        )
    }

    @Test
    fun offlineDoesNotUseCachedSupport() {
        assertEquals(
            KnowledgeAvailability.Offline,
            knowledgeAvailability(true, online = false, connecting = false, support = supported),
        )
    }

    @Test
    fun loadingAProfileConnectingAndPendingSupportShowChecking() {
        assertEquals(
            KnowledgeAvailability.Checking,
            knowledgeAvailability(false, online = false, connecting = true, support = null),
        )
        assertEquals(
            KnowledgeAvailability.Checking,
            knowledgeAvailability(true, online = false, connecting = true, support = null),
        )
        assertEquals(
            KnowledgeAvailability.Checking,
            knowledgeAvailability(true, online = true, connecting = false, support = null),
        )
    }

    @Test
    fun serverWithoutManageIsNotARightsFailure() {
        assertEquals(
            KnowledgeAvailability.ManagementUnavailable,
            knowledgeAvailability(true, online = true, connecting = false, support = KnowledgeCloudSupport(false, false)),
        )
    }

    @Test
    fun manageWithoutObserveHasNoReadRights() {
        assertEquals(
            KnowledgeAvailability.ObserveDenied,
            knowledgeAvailability(true, online = true, connecting = false, support = KnowledgeCloudSupport(true, false)),
        )
    }

    @Test
    fun genericCloudReadPermissionDoesNotProveKnowledgeSupport() {
        assertEquals(
            KnowledgeAvailability.NotOffered,
            knowledgeAvailability(true, online = true, connecting = false, support = supported),
        )
    }
}
