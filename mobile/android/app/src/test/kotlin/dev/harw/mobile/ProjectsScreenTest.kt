package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.harw_mobile_ffi.MobileProject

class ProjectsScreenTest {
    private fun project(key: String, label: String, last: String, count: UInt = 1u, hint: String? = null) =
        MobileProject(
            projectKey = key, label = label, pathHint = hint,
            sessionCount = count, activeCount = 0u, lastActivity = last,
        )

    private val alpha = project("alpha-aaaaaaaaaaaa", "alpha", "2026-11-20T10:00:00Z", hint = "~/x/alpha")
    private val beta = project("beta-bbbbbbbbbbbb", "Beta", "2026-11-21T10:00:00Z")
    private val none = project("", "", "2026-11-22T10:00:00Z", count = 5u)

    @Test
    fun noProjectLabelAndOrderingPutsItLast() {
        assertEquals("Ohne Projekt", none.displayLabel())
        assertEquals(listOf(beta, alpha, none), orderProjects(listOf(none, alpha, beta)))
    }

    @Test
    fun filterMatchesLabelHintAndNoProject() {
        val all = listOf(alpha, beta, none)
        assertEquals(all, filterProjects(all, "  "))
        assertEquals(listOf(beta), filterProjects(all, "BET"))
        assertEquals(listOf(alpha), filterProjects(all, "x/alpha"))
        assertEquals(listOf(none), filterProjects(all, "ohne"))
        assertEquals(emptyList<MobileProject>(), filterProjects(all, "zzz"))
    }

    @Test
    fun monthKeyAndHeading() {
        assertEquals("2026-11", monthKey("2026-11-30T23:30:00Z"))
        assertEquals("2026-12", monthKey("2026-11-30T23:30:00-02:00"))
        assertNull(monthKey("kaputt"))
        assertEquals("November 2026", monthHeading("2026-11"))
        assertEquals("Unbekannt", monthHeading(null))
        assertEquals("x", monthHeading("x"))
    }

    @Test
    fun groupByMonthNewestFirstUnparseableLast() {
        val groups = groupByMonth(listOf("2026-09-01T00:00:00Z", "bad", "2026-11-02T00:00:00Z", "2026-11-01T00:00:00Z"), activity = { it })
        assertEquals(listOf<String?>("2026-11", "2026-09", null), groups.map { it.first })
        assertEquals(2, groups[0].second.size)
    }

    @Test
    fun countAndActivityText() {
        assertEquals("1 Session", sessionCountText(1u, 0u))
        assertEquals("5 Sessions · 2 aktiv", sessionCountText(5u, 2u))
        val now = parseActivityMillis("2026-11-22T12:00:00Z")!!
        assertEquals("gerade eben", lastActivityText("2026-11-22T12:00:00Z", now))
        assertEquals("vor 5 Min.", lastActivityText("2026-11-22T11:55:00Z", now))
        assertEquals("vor 3 Std.", lastActivityText("2026-11-22T09:00:00Z", now))
        assertEquals("vor 2 Tg.", lastActivityText("2026-11-20T12:00:00Z", now))
        assertEquals("am 01.11.2026", lastActivityText("2026-11-01T12:00:00Z", now))
        assertEquals("Aktivität unbekannt", lastActivityText("x", now))
    }
}
