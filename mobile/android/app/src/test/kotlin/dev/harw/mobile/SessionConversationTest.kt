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
import uniffi.harw_mobile_ffi.TranscriptEntry
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
    fun asyncReplyStatusNeverInventsThinkingOrStreaming() {
        assertNull(chatReplyStatus(attached = true, running = false, hasReplyText = false, reloading = false, approval = false))
        assertEquals("Antwort wird erstellt … Noch kein Antworttext empfangen.", chatReplyStatus(true, true, false, false, false))
        assertEquals("Antwort wird gestreamt …", chatReplyStatus(true, true, true, false, false))
        assertEquals("Wartet auf Freigabe.", chatReplyStatus(true, true, false, false, true))
        assertEquals("Antwortstatus wird abgeglichen …", chatReplyStatus(false, true, true, false, false))
        assertEquals("Antwort wird nachgeladen …", chatReplyStatus(false, false, false, true, false))
    }

    @Test
    fun incompleteMarkerRecognizesOnlySpecificFailure() {
        assertTrue(isIncompleteResponseNotice("Antwort unvollständig: Das Ausgabe- oder Kontextlimit wurde erreicht."))
        assertTrue(isIncompleteResponseNotice("model output was truncated"))
        assertFalse(isIncompleteResponseNotice("Die Verbindung wurde beendet."))
    }

    @Test
    fun originalHistoricalIdsRemainSelectableThroughTheLaterRows() {
        fun user(id: String, text: String = id) = TranscriptEntry(id, TranscriptKind.USER, text, null, emptyList(), null)
        val current = listOf(user("prefix"), user("replacement"), user("current-later"))
        val archived = listOf(user("old-id", "Old prompt"), user("old-later"))
        val ui = VersionsUi(
            versions = mapOf("prefix" to listOf(ChatVersion(archived))),
            viewing = mapOf("prefix" to 0),
            onView = { _, _ -> },
            onEdit = { _, _, _ -> },
        )
        val (shown, firstArchived) = applyViewing(current, ui)
        assertEquals("old-id", shown[1].id)
        assertEquals("Old prompt", shown[1].text)
        assertEquals("prefix", selectedForkRow(shown, 0, firstArchived)?.selected?.itemId)
        assertEquals("old-id", selectedForkRow(shown, 1, firstArchived)?.selected?.itemId)
        assertEquals("old-later", selectedForkRow(shown, 2, firstArchived)?.selected?.itemId)
        assertEquals("current-later", selectedForkRow(current, 2, Int.MAX_VALUE)?.selected?.itemId)
    }

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
        assertFalse(rendersMarkdown(TranscriptKind.REASONING))
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
