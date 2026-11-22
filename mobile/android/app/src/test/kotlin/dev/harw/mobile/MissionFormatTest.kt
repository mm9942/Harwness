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
}
