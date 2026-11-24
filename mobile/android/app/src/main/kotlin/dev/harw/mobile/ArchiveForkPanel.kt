package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/** Dedicated verified archive surface; never accepts device-only versions or live draft rows. */
@Composable
internal fun ArchiveForkPanel(
    state: ArchiveForkState,
    attachment: ArchiveAttachment?,
    onOlder: () -> Unit,
    onNewest: () -> Unit,
    onFork: (ArchiveRowLocator) -> Unit,
    onResolveUnknown: () -> Unit,
    modifier: Modifier = Modifier,
    fetchMedia: (suspend (ArchiveRowLocator, UInt, uniffi.harw_mobile_ffi.MobileMediaPreviewMode) ->
        uniffi.harw_mobile_ffi.MobileMediaPreview)? = null,
) {
    if (!state.matches(attachment)) return
    val verified = state.rowsAreVerified(attachment)
    val canFork = verified && !state.busy && !state.outcomeUnknown
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Server-Archiv")
        if (state.busy) CircularProgressIndicator()
        state.notice?.let { Text(it) }
        if (state.outcomeUnknown) {
            TextButton(enabled = !state.busy, onClick = onResolveUnknown) {
                Text("Denselben Abzweig prüfen")
            }
        }
        if (!verified) {
            if (state.checkpoint != null && !state.outcomeUnknown) {
                TextButton(enabled = !state.busy, onClick = onNewest) { Text("Archiv neu laden") }
            }
            return@Column
        }
        Row {
            if (state.currentBefore != null) {
                TextButton(enabled = canFork, onClick = onNewest) { Text("Neueste Seite") }
            }
            if (state.nextBefore != null) {
                TextButton(enabled = canFork, onClick = onOlder) { Text("Ältere Nachrichten") }
            }
        }
        LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(state.rows, key = { it.locator.sequence.toString() }) { row ->
                Column {
                    val author = if (row.role == ArchiveRowRole.USER) "Du" else "Harw"
                    Text("$author · ${row.recordedAt}")
                    for (part in row.textParts) Text(part)
                    if (row.imageCount > 0u) {
                        val checkpoint = state.checkpoint
                        if (checkpoint != null && fetchMedia != null) {
                            SessionMediaPreviewStrip(
                                scope = Triple(attachment, checkpoint.checkpointId, row.locator),
                                retainedCount = row.imageCount.coerceAtMost(Int.MAX_VALUE.toUInt()).toInt(),
                                legacyCount = 0,
                                fetch = { ordinal, mode -> fetchMedia(row.locator, ordinal, mode) },
                            )
                        } else {
                            Text("Bilder: ${row.imageCount}")
                        }
                    }
                    TextButton(enabled = canFork, onClick = { onFork(row.locator) }) {
                        Text("Bis hier abzweigen")
                    }
                }
            }
        }
    }
}
