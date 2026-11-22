package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.MobileAgent
import uniffi.harw_mobile_ffi.MobileAgentState

internal data class AgentRow(val agent: MobileAgent, val depth: Int)

internal fun agentRows(agents: List<MobileAgent>): List<AgentRow> {
    val ids = agents.map { it.id }.toSet()
    val children = agents.filter { it.parent != null && it.parent in ids }.groupBy { it.parent }
    val visited = HashSet<String>()
    val rows = ArrayList<AgentRow>(agents.size)

    fun walk(agent: MobileAgent, depth: Int) {
        if (!visited.add(agent.id)) return
        rows.add(AgentRow(agent, depth))
        children[agent.id].orEmpty().forEach { walk(it, depth + 1) }
    }

    agents.filter { it.parent == null || it.parent !in ids }.forEach { walk(it, 0) }
    agents.forEach { walk(it, 0) }
    return rows
}

@Composable
internal fun AgentsScreen(state: WorkspaceState, sessionId: String, onBack: () -> Unit) {
    val snapshot = state.windows[sessionId]?.snapshot
    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Agents", onBack = onBack) {}
        when {
            snapshot == null -> StatusMessage("Not connected to this session.")
            snapshot.agents.isEmpty() -> StatusMessage("No agents in this session.")
            else -> {
                val rows = remember(snapshot.agents) { agentRows(snapshot.agents) }
                val nowMs = System.currentTimeMillis()
                LazyColumn(
                    contentPadding = PaddingValues(12.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    items(rows, key = { it.agent.id }) { row -> AgentCard(row, nowMs) }
                }
            }
        }
    }
}

@Composable
private fun AgentCard(row: AgentRow, nowMs: Long) {
    val agent = row.agent
    val colors = MaterialTheme.colorScheme
    val stateColor = when (agent.state) {
        MobileAgentState.FAILED -> colors.error
        MobileAgentState.RUNNING -> colors.primary
        else -> colors.onSurfaceVariant
    }
    Surface(
        Modifier
            .fillMaxWidth()
            .padding(start = (row.depth.coerceAtMost(6) * 16).dp),
        shape = MaterialTheme.shapes.medium,
        color = colors.surfaceVariant,
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(agent.role, style = MaterialTheme.typography.titleSmall, color = colors.onSurface)
            Text(agent.model ?: "default", style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant)
            Text(agentStatus(agent), style = MaterialTheme.typography.bodyMedium, color = stateColor)
            Text(
                "${formatElapsed(agent.startedMs.toLong(), nowMs)} - ${agent.toolCalls} tool calls",
                style = MaterialTheme.typography.bodySmall,
                color = colors.onSurfaceVariant,
            )
        }
    }
}
