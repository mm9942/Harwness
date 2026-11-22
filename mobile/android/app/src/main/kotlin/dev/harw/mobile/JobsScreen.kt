package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.TextButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.MobileJob

internal fun jobRows(jobs: List<MobileJob>): List<MobileJob> =
    jobs.sortedWith(compareByDescending<MobileJob> { it.updatedMs }.thenBy { it.workId })

/** Nur Jobs, die noch nicht beendet sind (`state_code` des Hosts), lassen sich abbrechen. */
internal fun jobCancellable(job: MobileJob): Boolean =
    job.state == "queued" || job.state == "running" || job.state == "suspended"

@Composable
internal fun JobsScreen(
    state: WorkspaceState,
    sessionId: String,
    onBack: () -> Unit,
    connection: HarwConnection? = null,
) {
    val snapshot = state.windows[sessionId]?.snapshot
    val support = state.manage.support
    val canCancel = connection != null && state.online && snapshot?.online == true &&
        support?.manage == true && support.steer
    androidx.compose.runtime.LaunchedEffect(sessionId, state.online) { connection?.refreshManageSupport() }
    var confirming by remember(sessionId) { mutableStateOf<MobileJob?>(null) }
    var pending by remember(sessionId) { mutableStateOf<Set<String>>(emptySet()) }
    var failure by remember(sessionId) { mutableStateOf<String?>(null) }
    confirming?.let { job ->
        AlertDialog(
            onDismissRequest = { confirming = null },
            title = { Text("Job abbrechen?") },
            text = { Text("Der Job \"${job.kind}\" wird beendet.") },
            dismissButton = { TextButton(onClick = { confirming = null }) { Text("Zurück") } },
            confirmButton = {
                TextButton(onClick = {
                    confirming = null
                    pending = pending + job.workId
                    failure = null
                    connection?.cancelJob(sessionId, job.workId) { error ->
                        pending = pending - job.workId
                        failure = error
                    }
                }) { Text("Job abbrechen") }
            },
        )
    }
    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Jobs", onBack = onBack) {}
        failure?.let { StatusMessage("Job abbrechen: $it", Modifier.padding(horizontal = 12.dp, vertical = 4.dp), error = true) }
        if (snapshot == null) {
            StatusMessage("Not connected to this session.", Modifier.padding(12.dp))
        } else if (snapshot.jobs.isEmpty()) {
            StatusMessage("No jobs in this session.", Modifier.padding(12.dp))
        } else {
            val rows = remember(snapshot.jobs) { jobRows(snapshot.jobs) }
            val now = System.currentTimeMillis()
            LazyColumn(
                contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(rows, key = { it.workId }) { job ->
                    Surface(
                        Modifier.fillMaxWidth(),
                        shape = RoundedCornerShape(12.dp),
                        color = MaterialTheme.colorScheme.surfaceContainer,
                    ) {
                        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                            Text(job.kind, style = MaterialTheme.typography.titleSmall)
                            Text(job.state, style = MaterialTheme.typography.bodyMedium)
                            job.summary?.let {
                                Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
                            }
                            Text(
                                formatElapsed(job.updatedMs.toLong(), now) + " ago",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            if (canCancel && jobCancellable(job)) {
                                TextButton(onClick = { confirming = job }, enabled = job.workId !in pending) {
                                    Text(
                                        if (job.workId in pending) "Wird abgebrochen…" else "Job abbrechen",
                                        color = MaterialTheme.colorScheme.error,
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
