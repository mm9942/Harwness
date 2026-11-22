package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.MobileHolder
import uniffi.harw_mobile_ffi.ManageSupport
import uniffi.harw_mobile_ffi.MobileHolderKind
import uniffi.harw_mobile_ffi.TranscriptKind

class SessionConversationTest {
    private fun holder(kind: MobileHolderKind) = MobileHolder(kind = kind, since = "2026-11-22T10:00:00Z")

    private fun access(
        active: Boolean = true,
        canSubmit: Boolean = true,
        holder: MobileHolder? = null,
        turnRunning: Boolean = false,
    ) = conversationAccess(active, canSubmit, holder, turnRunning)

    @Test
    fun freeSessionIsWritable() {
        assertEquals(ConversationAccess.Writable, access())
        assertNull(holderBannerText(ConversationAccess.Writable, null))
        assertNull(holderBadge(null))
    }

    @Test
    fun offlineBeatsEverything() {
        assertEquals(ConversationAccess.Offline, access(active = false, holder = holder(MobileHolderKind.TUI)))
    }

    @Test
    fun terminalJobAndUnknownHoldersBlockSending() {
        for (kind in listOf(MobileHolderKind.TUI, MobileHolderKind.JOB, MobileHolderKind.UNKNOWN)) {
            val result = access(holder = holder(kind))
            assertEquals(ConversationAccess.HeldElsewhere, result)
            assertNotNull(holderBannerText(result, holder(kind)))
            assertNotNull(holderBadge(holder(kind)))
        }
        assertTrue(holderBannerText(ConversationAccess.HeldElsewhere, holder(MobileHolderKind.TUI))!!.contains("Terminal"))
    }

    @Test
    fun gatewayHolderOnlyBlocksWhenNoOwnTurnRuns() {
        val gateway = holder(MobileHolderKind.GATEWAY)
        assertEquals(ConversationAccess.RunningOnGateway, access(holder = gateway, turnRunning = false))
        assertEquals(ConversationAccess.Writable, access(holder = gateway, turnRunning = true))
        assertEquals("Läuft auf Gateway", holderBadge(gateway))
    }

    @Test
    fun serverReadOnlyIsReadOnlyState() {
        val result = access(canSubmit = false)
        assertEquals(ConversationAccess.ReadOnly, result)
        assertTrue(composerHint(result).startsWith("Nur lesen"))
        assertEquals("Nachricht an Harw", composerHint(ConversationAccess.Writable))
    }

    @Test
    fun onlyAssistantTextIsMarkdown() {
        assertTrue(rendersMarkdown(TranscriptKind.ASSISTANT))
        assertFalse(rendersMarkdown(TranscriptKind.USER))
        assertFalse(rendersMarkdown(TranscriptKind.TOOL))
        assertFalse(rendersMarkdown(TranscriptKind.ERROR))
    }

    private fun support(
        manage: Boolean = true,
        stepup: Boolean = true,
        control: Boolean = true,
    ) = ManageSupport(
        manage = manage,
        stepup = stepup,
        observe = true,
        steer = true,
        approve = true,
        control = control,
    )

    @Test
    fun modelChipLabelPrefersLive() {
        assertEquals("l", modelChipLabel("s", "l"))
        assertEquals("s", modelChipLabel("s", null))
        assertEquals("Modell", modelChipLabel(null, null))
    }

    @Test
    fun modelChipBlockReasons() {
        assertEquals("Session nicht live", modelChipBlock(support(), active = false))
        assertEquals(NO_MANAGE_TEXT, modelChipBlock(null, active = true))
        assertEquals(NO_MANAGE_TEXT, modelChipBlock(support(manage = false), active = true))
        assertEquals("Modellwechsel braucht Profil Voll", modelChipBlock(support(control = false), active = true))
        assertNull(modelChipBlock(support(control = true), active = true))
    }

    @Test
    fun slashOpensCommandsOnlyForBareSlash() {
        assertTrue(slashOpensCommands("/", true))
        assertFalse(slashOpensCommands("/m", true))
        assertFalse(slashOpensCommands("", true))
        assertFalse(slashOpensCommands("/", false))
    }

    @Test
    fun switchableModelsDropRetired() {
        val result = switchableModels(listOf("a" to "active", "b" to "retired")) { it.second }
        assertEquals(listOf("a" to "active"), result)
    }
}
