package dev.harw.mobile

import uniffi.harw_mobile_ffi.FinishedCategory
import uniffi.harw_mobile_ffi.MobileAgent
import uniffi.harw_mobile_ffi.MobileAgentState
import uniffi.harw_mobile_ffi.MobileJob
import kotlin.math.max
import kotlin.math.min

// Mirrors the terminal states of `job_outcome` in mobile/rust/src/mission.rs.
private val TERMINAL_JOB_STATES = setOf(
    "completed", "complete", "done", "succeeded", "success", "ok",
    "failed", "error", "timed_out", "timeout", "dead",
    "cancelled", "canceled", "aborted", "stopped",
)

internal fun missionCardHeightDp(sideDp: Float, availableHeightDp: Float, panelDp: Float, stripDp: Float, imeVisible: Boolean): Float =
    if (!imeVisible) sideDp else min(sideDp, max(72f, availableHeightDp - panelDp - stripDp - 190f))

internal fun missionHeaderMaxHeightDp(availableHeightDp: Float): Float = max(0f, availableHeightDp - 184f)

internal fun formatElapsed(startedMs: Long, nowMs: Long): String {
    val elapsedMs = nowMs - startedMs
    if (elapsedMs <= 0L) return "0s"
    val seconds = elapsedMs / 1000L
    return when {
        seconds < 60L -> "${seconds}s"
        seconds < 3600L -> "${seconds / 60L}m${(seconds % 60L).toString().padStart(2, '0')}s"
        else -> "${seconds / 3600L}h${((seconds % 3600L) / 60L).toString().padStart(2, '0')}m"
    }
}

internal fun rolePrefix(role: String, width: Int = 12): String =
    if (role.length > width) role.take(width - 1) + "…" else role.padEnd(width)

internal fun finishedSummary(categories: List<FinishedCategory>): String =
    categories.joinToString(" · ") { category ->
        buildString {
            append(category.label)
            append(" ×")
            append(category.total)
            if (category.ok > 0u) append(" ✓").append(category.ok)
            if (category.failed > 0u) append(" ✗").append(category.failed)
            if (category.cancelled > 0u) append(" ⊘").append(category.cancelled)
        }
    }

internal fun agentStatus(agent: MobileAgent): String =
    agent.currentTool ?: when (agent.state) {
        MobileAgentState.RUNNING -> "thinking"
        MobileAgentState.WAITING -> "waiting"
        MobileAgentState.DONE -> "done"
        MobileAgentState.FAILED -> "failed"
    }

internal fun activeAgents(agents: List<MobileAgent>): Int =
    agents.count { it.state == MobileAgentState.RUNNING || it.state == MobileAgentState.WAITING }

internal fun activeJobs(jobs: List<MobileJob>): Int =
    jobs.count { it.state.lowercase() !in TERMINAL_JOB_STATES }
