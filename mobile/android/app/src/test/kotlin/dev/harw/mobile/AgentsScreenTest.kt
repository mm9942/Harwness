package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.MobileAgent
import uniffi.harw_mobile_ffi.MobileAgentState
import uniffi.harw_mobile_ffi.MobileJob

class AgentsScreenTest {
    private fun agent(id: String, parent: String? = null) =
        MobileAgent(
            id = id,
            parent = parent,
            role = "worker",
            model = null,
            state = MobileAgentState.RUNNING,
            currentTool = null,
            startedMs = 0uL,
            toolCalls = 0u,
        )

    private fun job(workId: String, updatedMs: ULong) =
        MobileJob(workId = workId, kind = "build", state = "running", summary = null, updatedMs = updatedMs)

    private fun pairs(rows: List<AgentRow>) = rows.map { it.agent.id to it.depth }

    @Test
    fun nestsChildrenDepthFirstUnderTheirParent() {
        val rows = agentRows(listOf(agent("a"), agent("b", "a"), agent("c", "a"), agent("d", "b")))
        assertEquals(listOf("a" to 0, "b" to 1, "d" to 2, "c" to 1), pairs(rows))
    }

    @Test
    fun listsRootsInInputOrderEachFollowedByItsSubtree() {
        val rows = agentRows(listOf(agent("a"), agent("x"), agent("b", "a"), agent("y", "x")))
        assertEquals(listOf("a" to 0, "b" to 1, "x" to 0, "y" to 1), pairs(rows))
    }

    @Test
    fun treatsAnOrphanAsARoot() {
        val rows = agentRows(listOf(agent("a", "missing"), agent("b", "a")))
        assertEquals(listOf("a" to 0, "b" to 1), pairs(rows))
    }

    @Test
    fun terminatesOnParentCyclesAndEmitsEachAgentOnce() {
        val rows = agentRows(listOf(agent("x", "y"), agent("y", "x")))
        assertEquals(listOf("x" to 0, "y" to 1), pairs(rows))
    }

    @Test
    fun returnsNoRowsForNoAgents() {
        assertTrue(agentRows(emptyList()).isEmpty())
    }

    @Test
    fun sortsJobsByUpdateTimeDescending() {
        val rows = jobRows(listOf(job("a", 1uL), job("b", 3uL), job("c", 2uL)))
        assertEquals(listOf("b", "c", "a"), rows.map { it.workId })
    }

    @Test
    fun breaksJobTiesByWorkIdAscending() {
        val rows = jobRows(listOf(job("z", 5uL), job("a", 5uL), job("m", 5uL)))
        assertEquals(listOf("a", "m", "z"), rows.map { it.workId })
    }

    @Test
    fun doesNotMutateTheJobInput() {
        val input = listOf(job("a", 1uL), job("b", 2uL))
        jobRows(input)
        assertEquals(listOf("a", "b"), input.map { it.workId })
    }
}
