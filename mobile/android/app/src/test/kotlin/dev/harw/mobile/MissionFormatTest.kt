package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.harw_mobile_ffi.FinishedCategory
import uniffi.harw_mobile_ffi.MobileAgent
import uniffi.harw_mobile_ffi.MobileAgentState
import uniffi.harw_mobile_ffi.MobileJob

class MissionFormatTest {
    private fun agent(state: MobileAgentState, currentTool: String? = null) =
        MobileAgent(
            id = "a",
            parent = null,
            role = "worker",
            model = null,
            state = state,
            currentTool = currentTool,
            startedMs = 0uL,
            toolCalls = 0u,
        )

    private fun job(state: String) =
        MobileJob(workId = "w", kind = "build", state = state, summary = null, updatedMs = 0uL)

    @Test
    fun formatsElapsedTimeWithoutLeadingZeroUnits() {
        assertEquals("14s", formatElapsed(0L, 14_000L))
        assertEquals("3m13s", formatElapsed(0L, 193_000L))
        assertEquals("1h02m", formatElapsed(0L, 3_720_000L))
    }

    @Test
    fun clampsNegativeElapsedTimeToZeroSeconds() {
        assertEquals("0s", formatElapsed(5_000L, 1_000L))
    }

    @Test
    fun missionClockMovesWithMonotonicTimeNotWallClock() {
        val anchor = MissionClockAnchor(wallMs = 15_000L, elapsedMs = 7_000L)
        assertEquals("3s", formatElapsed(13_000L, anchor.wallAt(8_000L)))
        assertEquals("6s", formatElapsed(13_000L, anchor.wallAt(11_000L)))
        assertEquals(15_000L, anchor.wallAt(6_000L))
    }

    @Test
    fun reasoningPreviewFollowsNewTextInTheSameRow() {
        val prefix = "Initial reasoning with a long first line"
        assertEquals(prefix, reasoningPreview(prefix))
        assertEquals("New sentence", reasoningPreview("$prefix\n\nNew sentence\n"))
        assertEquals("Suffix while streaming", reasoningPreview("$prefix\nSuffix while streaming"))
    }

    @Test
    fun padsRolePrefixToWidth() {
        assertEquals("coder       ", rolePrefix("coder"))
        assertEquals("abc   ", rolePrefix("abc", width = 6))
    }

    @Test
    fun keepsRolePrefixAtExactWidth() {
        assertEquals("abcdef", rolePrefix("abcdef", width = 6))
    }

    @Test
    fun truncatesLongRolePrefixWithEllipsis() {
        assertEquals("reviewer-ag…", rolePrefix("reviewer-agent", width = 12))
    }

    @Test
    fun summarisesNoFinishedCategoriesAsEmpty() {
        assertEquals("", finishedSummary(emptyList()))
    }

    @Test
    fun omitsZeroCountersInFinishedSummary() {
        val categories = listOf(FinishedCategory(label = "tests", total = 3u, ok = 3u, failed = 0u, cancelled = 0u))
        assertEquals("tests ×3 ✓3", finishedSummary(categories))
    }

    @Test
    fun joinsFinishedCategoriesWithAllCounters() {
        val categories = listOf(
            FinishedCategory(label = "tests", total = 4u, ok = 2u, failed = 1u, cancelled = 1u),
            FinishedCategory(label = "lint", total = 2u, ok = 2u, failed = 0u, cancelled = 0u),
        )
        assertEquals("tests ×4 ✓2 ✗1 ⊘1 · lint ×2 ✓2", finishedSummary(categories))
    }

    @Test
    fun prefersCurrentToolInAgentStatus() {
        assertEquals("grep", agentStatus(agent(MobileAgentState.RUNNING, currentTool = "grep")))
    }

    @Test
    fun describesAgentStateWhenNoToolIsRunning() {
        assertEquals("thinking", agentStatus(agent(MobileAgentState.RUNNING)))
        assertEquals("waiting", agentStatus(agent(MobileAgentState.WAITING)))
        assertEquals("done", agentStatus(agent(MobileAgentState.DONE)))
        assertEquals("failed", agentStatus(agent(MobileAgentState.FAILED)))
    }

    @Test
    fun countsOnlyRunningAndWaitingAgentsAsActive() {
        val agents = listOf(
            agent(MobileAgentState.RUNNING),
            agent(MobileAgentState.WAITING),
            agent(MobileAgentState.DONE),
            agent(MobileAgentState.FAILED),
        )
        assertEquals(2, activeAgents(agents))
    }

    @Test
    fun countsNonTerminalJobsCaseInsensitively() {
        val jobs = listOf(
            job("running"),
            job("Queued"),
            job("DONE"),
            job("Succeeded"),
            job("failed"),
            job("CANCELLED"),
            job("stopped"),
        )
        assertEquals(2, activeJobs(jobs))
    }

    @Test
    fun treatsCompletedErrorTimedOutAndCanceledJobsAsTerminal() {
        val jobs = listOf(
            job("completed"),
            job("error"),
            job("timed_out"),
            job("canceled"),
        )
        assertEquals(0, activeJobs(jobs))
    }

    @Test
    fun treatsUpperAndMixedCaseTerminalJobStatesAsTerminal() {
        val jobs = listOf(
            job("COMPLETED"),
            job("Error"),
            job("TIMED_OUT"),
            job("Canceled"),
            job("RUNNING"),
        )
        assertEquals(1, activeJobs(jobs))
    }

    @Test
    fun formatsElapsedTimeAtMinuteBoundary() {
        assertEquals("59s", formatElapsed(0L, 59_000L))
        assertEquals("1m00s", formatElapsed(0L, 60_000L))
    }

    @Test
    fun formatsElapsedTimeAtHourBoundary() {
        assertEquals("59m59s", formatElapsed(0L, 3_599_000L))
        assertEquals("1h00m", formatElapsed(0L, 3_600_000L))
    }

    @Test
    fun cardUsesOwnerSelectedTwoThirdsHeightWhenKeyboardIsClosed() {
        assertEquals(120f, missionCardHeightDp(180f, 800f, 200f, 40f, imeVisible = false), 0.001f)
        assertEquals(120f, missionCardHeightDp(180f, 0f, 200f, 40f, imeVisible = false), 0.001f)
    }

    @Test
    fun cardNeverExceedsOwnerSelectedTwoThirdsHeightWhenKeyboardIsOpen() {
        assertEquals(120f, missionCardHeightDp(180f, 2000f, 200f, 40f, imeVisible = true), 0.001f)
    }

    @Test
    fun cardShrinksWhenKeyboardIsOpenAndSpaceIsTight() {
        assertEquals(105f, missionCardHeightDp(180f, 535f, 200f, 40f, imeVisible = true), 0.001f)
        assertEquals(72f, missionCardHeightDp(180f, 500f, 200f, 40f, imeVisible = true), 0.001f)
    }

    @Test
    fun cardNeverShrinksBelowMinimumWhenKeyboardIsOpen() {
        assertEquals(72f, missionCardHeightDp(180f, 0f, 200f, 40f, imeVisible = true), 0.001f)
    }

    @Test
    fun headerReserveIsSubtractedFromAvailableHeight() {
        assertEquals(216f, missionHeaderMaxHeightDp(400f), 0.001f)
    }

    @Test
    fun headerMaxHeightIsNeverNegative() {
        assertEquals(0f, missionHeaderMaxHeightDp(100f), 0.001f)
        assertEquals(0f, missionHeaderMaxHeightDp(184f), 0.001f)
    }

    @Test
    fun sessionIsOfflineWhenNotOnline() {
        assertEquals(SessionFreshness.Offline, sessionFreshness(false, 3L, 3L))
        assertEquals(SessionFreshness.Offline, sessionFreshness(false, null, null))
    }

    @Test
    fun sessionIsLiveWhenGenerationsMatch() {
        assertEquals(SessionFreshness.Live, sessionFreshness(true, 3L, 3L))
        assertEquals(SessionFreshness.Live, sessionFreshness(true, 0L, 0L))
    }

    @Test
    fun sessionIsStaleWhenGenerationsDifferOrAreMissing() {
        assertEquals(SessionFreshness.Stale, sessionFreshness(true, 4L, 3L))
        assertEquals(SessionFreshness.Stale, sessionFreshness(true, 3L, null))
        assertEquals(SessionFreshness.Stale, sessionFreshness(true, null, 3L))
        assertEquals(SessionFreshness.Stale, sessionFreshness(true, null, null))
    }

    @Test
    fun computesApprovalRemainingFromUtcTimestamp() {
        // 2026-11-22T02:15:00Z = 1795313700000 ms
        assertEquals(60_000L, approvalRemainingMs("2026-11-22T02:15:00Z", 1_795_313_640_000L))
    }

    @Test
    fun computesApprovalRemainingFromOffsetTimestamp() {
        assertEquals(60_000L, approvalRemainingMs("2026-11-22T03:15:00+01:00", 1_795_313_640_000L))
    }

    @Test
    fun approvalRemainingMayBeZeroOrNegative() {
        assertEquals(0L, approvalRemainingMs("2026-11-22T02:15:00Z", 1_795_313_700_000L))
        assertEquals(-5_000L, approvalRemainingMs("2026-11-22T02:15:00Z", 1_795_313_705_000L))
    }

    @Test
    fun approvalRemainingIsNullForEmptyOrInvalidTimestamp() {
        assertEquals(null, approvalRemainingMs("", 0L))
        assertEquals(null, approvalRemainingMs("   ", 0L))
        assertEquals(null, approvalRemainingMs("not a date", 0L))
        assertEquals(null, approvalRemainingMs("2026-11-22 02:15", 0L))
    }

    @Test
    fun formatsRemainingTimeLikeElapsed() {
        assertEquals("45s", formatRemaining(45_000L))
        assertEquals("4m05s", formatRemaining(245_000L))
        assertEquals("1h02m", formatRemaining(3_720_000L))
    }

    @Test
    fun formatsNonPositiveRemainingAsExpired() {
        assertEquals("expired", formatRemaining(0L))
        assertEquals("expired", formatRemaining(-1L))
        assertEquals("0s", formatRemaining(1L))
    }

    @Test
    fun summarisesIdleMissionWhenNothingIsActive() {
        assertEquals("idle", missionSummary(0, 0))
    }

    @Test
    fun summarisesMissionWithSingularAndPlural() {
        assertEquals("1 agent", missionSummary(1, 0))
        assertEquals("3 jobs", missionSummary(0, 3))
        assertEquals("1 job", missionSummary(0, 1))
        assertEquals("2 agents · 1 job", missionSummary(2, 1))
        assertEquals("1 agent · 2 jobs", missionSummary(1, 2))
    }
}
