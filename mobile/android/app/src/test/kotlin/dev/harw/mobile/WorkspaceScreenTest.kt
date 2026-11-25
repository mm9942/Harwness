package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.MobileHolder
import uniffi.harw_mobile_ffi.MobileHolderKind
import uniffi.harw_mobile_ffi.MobileLastOrigin
import uniffi.harw_mobile_ffi.MobileRowOrigin
import uniffi.harw_mobile_ffi.MobileSessionPurpose
import uniffi.harw_mobile_ffi.MobileSessionRow
import uniffi.harw_mobile_ffi.MobileSessionState
import uniffi.harw_mobile_ffi.MobileShareInfo
import uniffi.harw_mobile_ffi.MobileShareMode
import uniffi.harw_mobile_ffi.MobileTier

class WorkspaceScreenTest {
    private fun row(
        id: String,
        title: String? = null,
        firstMessage: String? = null,
        holder: MobileHolder? = null,
        lastOrigin: MobileLastOrigin? = null,
        updatedAt: String = "2026-11-22T10:00:00Z",
    ) = MobileSessionRow(
        sessionId = id, title = title, projectKey = null, updatedAt = updatedAt,
        turns = null, state = MobileSessionState.IDLE, attached = 0u, model = null,
        origin = MobileRowOrigin.HOSTED, continuable = true, reason = null, holder = holder,
        lastOrigin = lastOrigin, firstMessage = firstMessage,
        purpose = MobileSessionPurpose.UNKNOWN,
    )

    private fun holder(kind: MobileHolderKind) = MobileHolder(kind, "2026-11-22T10:00:00Z")

    @Test
    fun ownerMark_holder_beats_last_origin() {
        assertEquals(
            OwnerMark.RUNNING_ON_GATEWAY,
            row("a", holder = holder(MobileHolderKind.GATEWAY), lastOrigin = MobileLastOrigin.TUI).ownerMark(),
        )
        assertEquals(OwnerMark.LAST_PHONE, row("a", lastOrigin = MobileLastOrigin.GATEWAY).ownerMark())
        assertEquals(OwnerMark.LAST_TERMINAL, row("a", lastOrigin = MobileLastOrigin.TUI).ownerMark())
        assertNull(row("a", lastOrigin = MobileLastOrigin.UNKNOWN).ownerMark())
        assertNull(row("a", lastOrigin = null).ownerMark())
        assertEquals(OwnerMark.HELD_BY_TERMINAL, row("a", holder = holder(MobileHolderKind.TUI)).ownerMark())
        assertEquals(OwnerMark.HELD_BY_JOB, row("a", holder = holder(MobileHolderKind.JOB)).ownerMark())
        assertEquals(OwnerMark.HELD, row("a", holder = holder(MobileHolderKind.UNKNOWN)).ownerMark())
    }

    @Test
    fun projectEmptyText_cases() {
        fun view(
            rows: List<MobileSessionRow> = emptyList(),
            query: String = "",
            share: MobileShareInfo? = null,
            legacy: Boolean = false,
        ) = ProjectSessionsState(project = null, rows = rows, query = query, share = share, legacy = legacy)

        assertNull(projectEmptyText(view(rows = listOf(row("a"))), canCreate = true))
        assertTrue(projectEmptyText(view(query = "abc"), canCreate = true)!!.contains("abc"))
        val off = MobileShareInfo(MobileShareMode.OFF, MobileTier.OPERATOR)
        assertEquals(
            "Dieses Gateway teilt keine TUI-Sessions.",
            projectEmptyText(view(share = off, legacy = false), canCreate = true),
        )
        assertTrue(projectEmptyText(view(legacy = true), canCreate = true)!!.startsWith("Dieser Server kennt keine Projekte"))
        val withCreate = projectEmptyText(view(), canCreate = true)
        val withoutCreate = projectEmptyText(view(), canCreate = false)
        assertNotNull(withCreate)
        assertNotNull(withoutCreate)
        assertNotEquals(withCreate, withoutCreate)
    }

    @Test
    fun tier_labels() {
        val labels = MobileTier.values().map { it.label() }
        assertEquals(4, labels.size)
        assertTrue(labels.all { it.isNotBlank() })
        assertEquals(labels.size, labels.toSet().size)
    }

    @Test
    fun displayTitle_falls_back_to_id_without_showing_first_message() {
        assertEquals("T", row("sess-0001", title = "T").displayTitle())
        assertEquals("Session sess-000", row("sess-0001", title = " ", firstMessage = "Hallo").displayTitle())
        assertEquals("Session sess-000", row("sess-0001").displayTitle())
    }

    @Test
    fun sections_group_without_project_by_month() {
        val rows = listOf(
            row("a", updatedAt = "2026-10-05T10:00:00Z"),
            row("b", updatedAt = "2026-11-02T10:00:00Z"),
        )
        val none = ProjectSessionsState(project = null, rows = rows, projectKey = NoProjectKey)
        assertEquals(listOf("November 2026", "Oktober 2026"), projectSessionSections(none).map { it.heading })

        val proj = ProjectSessionsState(project = null, rows = rows, projectKey = "proj-a")
        val sections = projectSessionSections(proj)
        assertEquals(1, sections.size)
        assertNull(sections[0].heading)
        assertEquals(rows, sections[0].rows)

        assertTrue(projectSessionSections(ProjectSessionsState(project = null, rows = emptyList(), projectKey = NoProjectKey)).isEmpty())
    }
}
