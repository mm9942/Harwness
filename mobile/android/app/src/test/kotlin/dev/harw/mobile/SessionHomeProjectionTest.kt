package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SessionHomeProjectionTest {
    private val local = SessionHomeScope(SessionHomeBackend.Local, "primary")
    private val cloud = SessionHomeScope(SessionHomeBackend.Cloud, "primary")
    private val otherLocal = SessionHomeScope(SessionHomeBackend.Local, "other")

    private fun row(
        id: String,
        activityAtMs: Long?,
        kind: SessionHomeKind = SessionHomeKind.User,
        scope: SessionHomeScope = local,
        title: String? = null,
    ) = SessionHomeRow(SessionHomeId(scope, id), kind, activityAtMs, title)

    private fun List<SessionHomeRow>.ids() = map { it.id.sessionId }

    @Test
    fun scopeIncludesBothBackendAndProfileEvenWhenSessionIdsMatch() {
        val localRow = row("shared", 10)
        val cloudRow = row("shared", 30, scope = cloud)
        val otherProfileRow = row("shared", 20, scope = otherLocal)
        val rows = listOf(cloudRow, otherProfileRow, localRow)

        val projected = projectSessionHome(local, rows, listOf(cloudRow.id, otherProfileRow.id))
        assertTrue(projected.pinned.isEmpty())
        assertEquals(listOf(localRow), projected.recent)
        assertEquals(listOf(cloudRow), projectSessionHome(cloud, rows, emptyList()).recent)
    }

    @Test
    fun pinsHonorSavedOrderAndNeverRepeatInRecentsOrOrdinary() {
        val first = row("old", 1)
        val second = row("new", 99)
        val remaining = row("remaining", 42)
        val projected = projectSessionHome(
            local,
            listOf(second, first, remaining),
            listOf(first.id, first.id, row("missing", 1).id, second.id),
        )
        assertEquals(listOf("old", "new"), projected.pinned.ids())
        assertEquals(listOf("remaining"), projected.recent.ids())
        assertTrue(projected.ordinary.isEmpty())
    }

    @Test
    fun recentLimitDefaultsToTenAndIsClampedToFiveThroughTwenty() {
        val rows = (1..25).map { row("s${it}", it.toLong()) }
        assertEquals((25 downTo 16).map { "s${it}" }, projectSessionHome(local, rows, emptyList()).recent.ids())
        val minimum = projectSessionHome(local, rows, emptyList(), recentLimit = 0)
        assertEquals(5, minimum.recent.size)
        assertEquals(20, minimum.ordinary.size)
        assertEquals(20, projectSessionHome(local, rows, emptyList(), recentLimit = Int.MAX_VALUE).recent.size)
    }

    @Test
    fun unifiedHomeKeepsThreeRecentsAfterFavoritesForBothBackends() {
        for (scope in listOf(local, cloud)) {
            val rows = (1..7).map {
                row("s${it}", it.toLong(), SessionHomeKind.UnknownLegacy, scope)
            }.reversed()
            val favorite = rows.first()
            val internal = row("internal", 99, SessionHomeKind.Internal, scope)
            val other = row("other", 100, scope = otherLocal)
            val projection = projectUnifiedSessionHome(scope, rows + internal + other, listOf(favorite.id))

            assertEquals(listOf("s7"), projection.pinned.ids())
            assertEquals(listOf("s6", "s5", "s4"), projection.recent.ids())
            assertEquals(listOf("s3", "s2", "s1"), projection.ordinary.ids())
            assertEquals(listOf(internal), projection.internal)
        }
    }

    @Test
    fun recentUsesTrueActivityAndPutsUnobservedActivityInOrdinaryOnly() {
        val rows = listOf(row("z-tied", 2), row("unobserved", null), row("old", 1), row("a-tied", 2))
        val projected = projectSessionHome(local, rows, emptyList())
        assertEquals(listOf("a-tied", "z-tied", "old"), projected.recent.ids())
        assertEquals(listOf("unobserved"), projected.ordinary.ids())
    }

    @Test
    fun internalAndUnknownRemainSeparateAndCannotAppearAsPinsOrRecents() {
        val dream = row("dream", 30, SessionHomeKind.Internal, title = "User chat")
        val old = row("legacy", 20, SessionHomeKind.UnknownLegacy, title = "Looks real")
        val user = row("user", 10, title = "Dreamer")
        val projected = projectSessionHome(local, listOf(dream, old, user), listOf(dream.id, old.id))
        assertTrue(projected.pinned.isEmpty())
        assertTrue(projected.ordinary.isEmpty())
        assertEquals(listOf(user), projected.recent)
        assertEquals(listOf(dream), projected.internal)
        assertEquals(listOf(old), projected.unknownLegacy)
    }

    @Test
    fun trustedKindReplacesLegacyButConflictingTrustFailsClosed() {
        val oldUser = row("duplicate", 100, SessionHomeKind.UnknownLegacy)
        val user = row("duplicate", 10, title = "Verified user")
        val bad = row("conflict", 12, SessionHomeKind.Internal)
        val inconsistent = row("conflict", 99)
        val projected = projectSessionHome(
            local,
            listOf(oldUser, user, inconsistent, bad),
            listOf(user.id, bad.id),
        )
        assertEquals(listOf(user), projected.pinned)
        assertEquals(listOf(bad), projected.internal)
        assertTrue(projected.unknownLegacy.isEmpty())
        assertTrue(projected.recent.isEmpty())
    }

    @Test
    fun newestDuplicateSnapshotWinsAndEqualActivityOrderIsStable() {
        val rows = listOf(row("beta", 2), row("alpha", 1, title = "old"), row("alpha", 2, title = "new"))
        val projected = projectSessionHome(local, rows, emptyList())
        assertEquals(listOf("alpha", "beta"), projected.recent.ids())
        assertEquals("new", projected.recent.first().title)
        assertEquals(projected, projectSessionHome(local, rows.reversed(), emptyList()))
    }
}
