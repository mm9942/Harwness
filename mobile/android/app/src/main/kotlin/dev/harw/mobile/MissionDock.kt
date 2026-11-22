package dev.harw.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import kotlinx.coroutines.delay
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
private val STRIP_HEIGHT = 20.dp
private val PANEL_CHROME = 36.dp
private const val TICK_MS = 1_000L

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun MissionHeader(
    agents: List<MobileAgent>,
    jobs: List<MobileJob>,
    reasoning: List<ReasoningLine>,
    finished: List<FinishedCategory>,
    availableHeight: Dp,
    modifier: Modifier = Modifier,
) {
    val panelHeight = with(LocalDensity.current) {
        MaterialTheme.typography.bodySmall.lineHeight.toDp() * REASONING_LINES
    } + PANEL_CHROME
    val imeVisible = WindowInsets.isImeVisible
    val now = rememberTicker(activeAgents(agents) > 0)
    val agentCount = activeAgents(agents)
    val jobCount = activeJobs(jobs)
    var expanded by rememberSaveable { mutableStateOf(agentCount > 0 || jobCount > 0) }
    val summary = missionSummary(agentCount, jobCount)
    if (!expanded) {
        Text(
            "▸ $summary",
            modifier = modifier.fillMaxWidth().clickable { expanded = true }.padding(vertical = 4.dp),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        return
    }
    Column(
        modifier
            .fillMaxWidth()
            .heightIn(max = missionHeaderMaxHeightDp(availableHeight.value).dp)
            .verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            "▾ $summary",
            modifier = Modifier.fillMaxWidth().clickable { expanded = false },
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val side = (maxWidth - SPACING) / 2
            val cardHeight = missionCardHeightDp(
                side.value,
                availableHeight.value,
                panelHeight.value,
                STRIP_HEIGHT.value,
                imeVisible,
            ).dp
            Row(horizontalArrangement = Arrangement.spacedBy(SPACING)) {
                AgentsCard(agents, now, Modifier.weight(1f).height(cardHeight))
                JobsCard(jobs, Modifier.weight(1f).height(cardHeight))
            }
        }
        ReasoningPanel(reasoning, activeAgents(agents))
        if (finished.isNotEmpty()) FinishedStrip(finished)
    }
}

/** Wall clock that ticks once a second only while [running]; read it only where the time is shown. */
@Composable
internal fun rememberTicker(running: Boolean): State<Long> {
    val now = remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(running) {
        while (running) {
            now.longValue = System.currentTimeMillis()
            delay(TICK_MS)
        }
    }
    return now
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
private fun AgentsCard(agents: List<MobileAgent>, now: State<Long>, modifier: Modifier) {
    OutlinedCard(modifier, shape = CARD_SHAPE) {
        Column(Modifier.padding(SPACING), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            CardTitle("Agents · ${activeAgents(agents)} active")
            if (agents.isEmpty()) {
                Text("No agents yet", style = MaterialTheme.typography.bodySmall)
            } else {
                LazyColumn(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    items(agents, key = { it.id }) { AgentRow(it, now) }
                }
            }
        }
    }
}

@Composable
private fun AgentRow(agent: MobileAgent, now: State<Long>) {
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
            agentStatus(agent) + " · " + formatElapsed(agent.startedMs.toLong(), now.value),
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
                Text("No jobs yet", style = MaterialTheme.typography.bodySmall)
            } else {
                LazyColumn(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    items(jobs, key = { it.workId }) { JobRow(it) }
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
    LaunchedEffect(lines.lastOrNull()?.seq) {
        if (follow && lines.isNotEmpty()) state.scrollToItem(lines.lastIndex)
    }
    val bodyHeight = with(LocalDensity.current) {
        MaterialTheme.typography.bodySmall.lineHeight.toDp() * REASONING_LINES
    }
    val context = androidx.compose.ui.platform.LocalContext.current
    // Echter Denktext, einklappbar; Startzustand folgt der Einstellung "Denkzusammenfassungen anzeigen".
    var open by rememberSaveable { mutableStateOf(readShowReasoningPref(context)) }
    OutlinedCard(Modifier.fillMaxWidth(), shape = CARD_SHAPE) {
        Column(Modifier.padding(SPACING), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                (if (open) "▾ " else "▸ ") + "Denkprozess · $active aktiv",
                modifier = Modifier.fillMaxWidth().clickable { open = !open },
                style = MaterialTheme.typography.labelLarge,
            )
            if (open && lines.isEmpty()) {
                Text(
                    "Noch keine Aktivität",
                    modifier = Modifier.height(bodyHeight),
                    style = MaterialTheme.typography.bodySmall,
                )
            } else if (open) {
                LazyColumn(Modifier.height(bodyHeight), state = state) {
                    items(lines, key = { it.seq.toLong() }) { ReasoningRow(it) }
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
