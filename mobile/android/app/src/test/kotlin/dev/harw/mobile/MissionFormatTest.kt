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
    fun cardIsExactSquareWhenKeyboardIsClosed() {
        assertEquals(180f, missionCardHeightDp(180f, 800f, 200f, 40f, imeVisible = false), 0.001f)
        assertEquals(180f, missionCardHeightDp(180f, 0f, 200f, 40f, imeVisible = false), 0.001f)
    }

    @Test
    fun cardNeverExceedsSideWhenKeyboardIsOpen() {
        assertEquals(180f, missionCardHeightDp(180f, 2000f, 200f, 40f, imeVisible = true), 0.001f)
    }

    @Test
    fun cardShrinksWhenKeyboardIsOpenAndSpaceIsTight() {
        assertEquals(170f, missionCardHeightDp(180f, 600f, 200f, 40f, imeVisible = true), 0.001f)
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
}
