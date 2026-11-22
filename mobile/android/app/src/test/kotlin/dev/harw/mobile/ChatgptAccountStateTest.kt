package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.ChatgptLinkAvailability
import uniffi.harw_mobile_ffi.ChatgptProviderStatus

class ChatgptAccountStateTest {
    @Test
    fun availableUnlinkedStatusCanStartSignIn() {
        val state = ChatgptProviderStatus(linked = false, availability = ChatgptLinkAvailability.AVAILABLE).toAccountState()
        assertEquals(false, state.linked)
        assertNull(state.unavailable)
        assertTrue(state.canStartSignIn())
    }

    @Test
    fun notConfiguredServerBlocksSignInWithMessage() {
        val state = ChatgptProviderStatus(linked = false, availability = ChatgptLinkAvailability.NOT_CONFIGURED).toAccountState()
        assertEquals(false, state.linked)
        assertEquals(ChatgptNotConfiguredMessage, state.unavailable)
        assertFalse(state.canStartSignIn())
    }

    @Test
    fun wrongLinkDeviceBlocksSignInWithMessage() {
        val state = ChatgptProviderStatus(linked = false, availability = ChatgptLinkAvailability.NOT_LINK_DEVICE).toAccountState()
        assertEquals(false, state.linked)
        assertEquals(ChatgptNotLinkDeviceMessage, state.unavailable)
        assertFalse(state.canStartSignIn())
    }

    @Test
    fun linkedAccountCannotStartSignIn() {
        val state = ChatgptProviderStatus(linked = true, availability = ChatgptLinkAvailability.AVAILABLE).toAccountState()
        assertEquals(true, state.linked)
        assertNull(state.unavailable)
        assertFalse(state.canStartSignIn())
    }

    @Test
    fun defaultStateCannotStartSignIn() {
        assertFalse(ChatgptAccountState().canStartSignIn())
    }

    @Test
    fun unavailableReasonBlocksSignInEvenWhenUnlinked() {
        assertFalse(ChatgptAccountState(linked = false, unavailable = "x").canStartSignIn())
    }
}
