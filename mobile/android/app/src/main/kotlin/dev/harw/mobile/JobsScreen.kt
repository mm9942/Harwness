package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.MobileJob

internal fun jobRows(jobs: List<MobileJob>): List<MobileJob> =
    jobs.sortedWith(compareByDescending<MobileJob> { it.updatedMs }.thenBy { it.workId })

@Composable
internal fun JobsScreen(state: WorkspaceState, sessionId: String, onBack: () -> Unit) {
    val snapshot = state.windows[sessionId]?.snapshot
    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Jobs", onBack = onBack) {}
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
                items(rows) { job ->
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
                        }
                    }
                }
            }
        }
    }
}
