package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.FinishedCategory
import uniffi.harw_mobile_ffi.MobileAgent
import uniffi.harw_mobile_ffi.MobileAgentState
import uniffi.harw_mobile_ffi.MobileJob
import uniffi.harw_mobile_ffi.ReasoningKind
import uniffi.harw_mobile_ffi.ReasoningLine

private const val REASONING_LINES = 8
private val CARD_SHAPE = RoundedCornerShape(12.dp)
private val SPACING = 8.dp
private val MIN_CARD_HEIGHT = 72.dp
private val STRIP_HEIGHT = 20.dp
private val PANEL_CHROME = 36.dp
private val CHAT_RESERVE = 190.dp

@Composable
internal fun MissionHeader(
    agents: List<MobileAgent>,
    jobs: List<MobileJob>,
    reasoning: List<ReasoningLine>,
    finished: List<FinishedCategory>,
    nowMs: Long,
    availableHeight: Dp,
    modifier: Modifier = Modifier,
) {
    val panelHeight = with(LocalDensity.current) {
        MaterialTheme.typography.bodySmall.lineHeight.toDp() * REASONING_LINES
    } + PANEL_CHROME
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val side = (maxWidth - SPACING) / 2
            val room = availableHeight - panelHeight - STRIP_HEIGHT - CHAT_RESERVE
            val cardHeight = minOf(side, maxOf(MIN_CARD_HEIGHT, room))
            Row(horizontalArrangement = Arrangement.spacedBy(SPACING)) {
                AgentsCard(agents, nowMs, Modifier.weight(1f).height(cardHeight))
                JobsCard(jobs, Modifier.weight(1f).height(cardHeight))
            }
        }
        ReasoningPanel(reasoning, activeAgents(agents))
        if (finished.isNotEmpty()) FinishedStrip(finished)
    }
}

@Composable
private fun CardTitle(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
    )
}

@Composable
private fun AgentsCard(agents: List<MobileAgent>, nowMs: Long, modifier: Modifier) {
    OutlinedCard(modifier, shape = CARD_SHAPE) {
        Column(Modifier.padding(SPACING), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            CardTitle("Agents · ${activeAgents(agents)} active")
            if (agents.isEmpty()) {
                Text("No agents", style = MaterialTheme.typography.bodySmall)
            } else {
                LazyColumn(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    items(agents) { AgentRow(it, nowMs) }
                }
            }
        }
    }
}

@Composable
private fun AgentRow(agent: MobileAgent, nowMs: Long) {
    val colors = MaterialTheme.colorScheme
    val dim = colors.onSurfaceVariant
    val tint = when (agent.state) {
        MobileAgentState.FAILED -> colors.error
        MobileAgentState.DONE -> dim
        else -> colors.onSurface
    }
    Column {
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                agent.role,
                modifier = Modifier.weight(1f, fill = false),
                style = MaterialTheme.typography.bodySmall,
                fontWeight = FontWeight.Bold,
                color = tint,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            agent.model?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.labelSmall,
                    color = dim,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Text(
            agentStatus(agent) + " · " + formatElapsed(agent.startedMs.toLong(), nowMs),
            style = MaterialTheme.typography.labelSmall,
            color = if (agent.state == MobileAgentState.FAILED) colors.error else dim,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun JobsCard(jobs: List<MobileJob>, modifier: Modifier) {
    OutlinedCard(modifier, shape = CARD_SHAPE) {
        Column(Modifier.padding(SPACING), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            CardTitle("Jobs · ${activeJobs(jobs)} active")
            if (jobs.isEmpty()) {
                Text("No jobs", style = MaterialTheme.typography.bodySmall)
            } else {
                LazyColumn(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    items(jobs) { JobRow(it) }
                }
            }
        }
    }
}

@Composable
private fun JobRow(job: MobileJob) {
    val dim = MaterialTheme.colorScheme.onSurfaceVariant
    Column {
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                job.kind,
                modifier = Modifier.weight(1f, fill = false),
                style = MaterialTheme.typography.bodySmall,
                fontWeight = FontWeight.Bold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                job.state,
                style = MaterialTheme.typography.labelSmall,
                color = dim,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        job.summary?.let {
            Text(
                it,
                style = MaterialTheme.typography.labelSmall,
                color = dim,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun ReasoningPanel(lines: List<ReasoningLine>, active: Int) {
    val state = rememberLazyListState()
    var follow by remember { mutableStateOf(true) }
    LaunchedEffect(state) {
        snapshotFlow {
            val info = state.layoutInfo
            val last = info.visibleItemsInfo.lastOrNull()?.index ?: -1
            state.isScrollInProgress to (last >= info.totalItemsCount - 2)
        }.collect { (scrolling, atBottom) -> if (scrolling) follow = atBottom }
    }
    LaunchedEffect(lines.size) {
        if (follow && lines.isNotEmpty()) state.scrollToItem(lines.size - 1)
    }
    val bodyHeight = with(LocalDensity.current) {
        MaterialTheme.typography.bodySmall.lineHeight.toDp() * REASONING_LINES
    }
    OutlinedCard(Modifier.fillMaxWidth(), shape = CARD_SHAPE) {
        Column(Modifier.padding(SPACING), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            CardTitle("Reasoning · $active active")
            if (lines.isEmpty()) {
                Text(
                    "No activity yet",
                    modifier = Modifier.height(bodyHeight),
                    style = MaterialTheme.typography.bodySmall,
                )
            } else {
                LazyColumn(Modifier.height(bodyHeight), state = state) {
                    items(lines) { ReasoningRow(it) }
                }
            }
        }
    }
}

@Composable
private fun ReasoningRow(line: ReasoningLine) {
    val colors = MaterialTheme.colorScheme
    val style = MaterialTheme.typography.bodySmall
    val failed = line.ok == false
    val color = if (failed && line.kind != ReasoningKind.THINKING) colors.error else colors.onSurface
    val body = when (line.kind) {
        ReasoningKind.THINKING -> "∴ " + line.text
        ReasoningKind.TOOL_START -> "● " + line.text
        ReasoningKind.TOOL_END -> (if (failed) "✗ " else "✓ ") + line.text
        ReasoningKind.FINISHED -> if (failed) "✗ failed" else "✓ done"
    }
    Row {
        Text(
            rolePrefix(line.role),
            style = style,
            fontFamily = FontFamily.Monospace,
            color = colors.onSurfaceVariant,
            maxLines = 1,
        )
        Text(
            body,
            modifier = Modifier.weight(1f),
            style = style,
            fontStyle = if (line.kind == ReasoningKind.THINKING) FontStyle.Italic else null,
            color = if (line.kind == ReasoningKind.THINKING) colors.onSurfaceVariant else color,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun FinishedStrip(finished: List<FinishedCategory>) {
    Text(
        "FINISHED / FAILED · " + finishedSummary(finished),
        modifier = Modifier.fillMaxWidth(),
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
    )
}
