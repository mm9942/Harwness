package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.ManageCommand
import uniffi.harw_mobile_ffi.ManageEffect
import uniffi.harw_mobile_ffi.ManageOrigin
import uniffi.harw_mobile_ffi.MobileTier

class CommandsScreenTest {
    private fun cmd(
        name: String,
        domain: String = "session",
        aliases: List<String> = emptyList(),
        summary: String = "",
        usage: String = "",
        available: Boolean = true,
        reason: String? = null,
    ) = ManageCommand(
        name = name, aliases = aliases, summary = summary, usage = usage, domain = domain,
        permission = MobileTier.OBSERVER, busy = "immediate", origin = ManageOrigin.OPERATION,
        remote = true, available = available, reason = reason, reasonCode = null, subcommands = emptyList(),
    )

    @Test
    fun groupsInFixedOrderAndUnknownGoesToMisc() {
        val groups = groupCommands(listOf(cmd("zukunft", "future"), cmd("agent", "agents"), cmd("status", "session")))
        assertEquals(listOf("Unterhaltung", "Agenten", "Sonstiges"), groups.map { it.first })
        assertEquals(listOf("zukunft"), groups[2].second.map { it.name })
    }

    @Test
    fun filterMatchesNameAliasSummaryIgnoringSlash() {
        val all = listOf(
            cmd("status", summary = "Zeigt den Zustand"),
            cmd("stop", aliases = listOf("halt")),
            cmd("models", summary = "Modelle"),
        )
        assertEquals(listOf("status"), filterCommands(all, "/sta").map { it.name })
        assertEquals(listOf("stop"), filterCommands(all, "halt").map { it.name })
        assertEquals(listOf("status"), filterCommands(all, "ZUSTAND").map { it.name })
        assertEquals(3, filterCommands(all, "  ").size)
    }

    @Test
    fun unavailableStaysGreyWithReason() {
        val groups = groupCommands(
            listOf(
                cmd("alpha", available = false, reason = "Nur lokal"),
                cmd("beta"),
            ),
        )
        val list = groups.single().second
        assertEquals(listOf("beta", "alpha"), list.map { it.name })
        assertEquals("Nur lokal", list[1].reason)
        assertTrue(!list[1].available)
    }

    @Test
    fun buildsStatusAndModelsShow() {
        assertEquals("/status", buildCommandLine("status", null, ""))
        assertEquals("/status", buildCommandLine("/status", "", "  "))
        assertEquals("/models show", buildCommandLine("models", "show", ""))
        assertEquals("/models show a b", buildCommandLine("models", " show ", "a   b"))
        assertNull(buildCommandLine("sta\u0000tus", null, ""))
        assertNull(buildCommandLine("status", null, "x\ny"))
        assertNull(buildCommandLine("status", null, "a".repeat(MAX_LINE_CHARS + 1)))
    }

    @Test
    fun slashQueryOnlyForSingleLine() {
        assertEquals("st", slashQuery("/st"))
        assertEquals("", slashQuery("/"))
        assertNull(slashQuery("hi /x"))
        assertNull(slashQuery("/a\nb"))
    }

    @Test
    fun effectLabelsAreGerman() {
        val labels = ManageEffect.values().map { effectLabel(it) }
        assertEquals(ManageEffect.values().size, labels.toSet().size)
        assertEquals("Abgelehnt", effectLabel(ManageEffect.REFUSED))
        assertEquals("Ausgeführt", effectLabel(ManageEffect.APPLIED))
        assertTrue(labels.all { it.isNotBlank() })
    }

    @Test
    fun expertLineRules() {
        assertEquals("/status", expertLine("  /status  "))
        assertNull(expertLine("status"))
        assertNull(expertLine("/"))
        assertNull(expertLine("/a\nb"))
        assertNull(expertLine("/" + "a".repeat(MAX_LINE_CHARS)))
    }
}
