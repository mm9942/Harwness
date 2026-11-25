package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

class ChatTranscriptExportTest {
    @Test
    fun leadingSlashRoutesOnlyKnownCommandsLocally() {
        assertEquals(ChatSlashCommand.Export, parseChatSlashCommand(" /ExPoRt "))
        assertEquals(ChatSlashCommand.Export, parseChatSlashCommand("/transcript"))
        assertEquals(ChatSlashCommand.Help, parseChatSlashCommand("/"))
        assertEquals(ChatSlashCommand.Help, parseChatSlashCommand("/help"))
        assertEquals(ChatSlashCommand.Unknown, parseChatSlashCommand("/export to an external site"))
        assertEquals(ChatSlashCommand.Unknown, parseChatSlashCommand("/unknown"))
        assertNull(parseChatSlashCommand("Show an example of /export"))
    }

    @Test
    fun composerRouterNeverInvokesModelSendForAnyLeadingSlash() {
        var sent = 0
        val commands = mutableListOf<ChatSlashCommand>()
        for (text in listOf("/export", "/transcript", " /unknown ", "/help", "Explain /export")) {
            routeChatComposerSend(text, { sent++ }, { commands.add(it) })
        }
        assertEquals(1, sent)
        assertEquals(
            listOf(ChatSlashCommand.Export, ChatSlashCommand.Export, ChatSlashCommand.Unknown, ChatSlashCommand.Help),
            commands,
        )
    }

    @Test
    fun visibleExportExcludesReasoningToolErrorsRemainingEchoAndMediaRefs() {
        fun row(id: String, kind: TranscriptKind, text: String, images: List<String> = emptyList()) =
            TranscriptEntry(id = id, kind = kind, text = text, images = images, toolState = null, userPresentation = null)
        val rows = listOf(
            row("u", TranscriptKind.USER, "Question", listOf("opaque-private-media-ref")),
            row("a", TranscriptKind.ASSISTANT, "Response"),
            row("r", TranscriptKind.REASONING, "hidden"),
            row("t", TranscriptKind.TOOL, "hidden"),
            row("e", TranscriptKind.ERROR, "hidden"),
            row("mobile-new", TranscriptKind.USER, "uncommitted"),
        )
        val text = (renderChatTranscript(rows, "User", "Agent") { "[$it image omitted]" } as ChatTranscriptText.Ready).text
        assertEquals("User:\nQuestion\n[1 image omitted]\n\nAgent:\nResponse\n", text)
        assertFalse(text.contains("opaque-private-media-ref"))
    }
}
