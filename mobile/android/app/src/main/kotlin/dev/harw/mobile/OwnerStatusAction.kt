package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.MobileOwnerStatus
import uniffi.harw_mobile_ffi.ServerProfile

/** Full private trust snapshot; it is not part of saved chat/navigation state. */
internal class OwnerStatusTarget(
    val profile: ServerProfile,
    val generation: Long,
    val resetEpoch: Long,
    val deviceNodeId: String,
    val sessionId: String,
)

private sealed interface OwnerStatusRead {
    data object Ready : OwnerStatusRead
    data object Pending : OwnerStatusRead
    data object Unavailable : OwnerStatusRead
    data class Observed(val status: MobileOwnerStatus) : OwnerStatusRead
}

/** This UI launches nothing from composition. Closing the sheet drops the result. */
@Composable
internal fun OwnerStatusAction(
    target: OwnerStatusTarget,
    connection: HarwConnection,
    stillSelected: () -> Boolean,
) {
    val scope = rememberCoroutineScope()
    val currentSelection by rememberUpdatedState(stillSelected)
    var state by remember(target) { mutableStateOf<OwnerStatusRead>(OwnerStatusRead.Ready) }
    Column {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(
                enabled = state != OwnerStatusRead.Pending,
                onClick = {
                    if (state != OwnerStatusRead.Pending && currentSelection() &&
                        connection.ownerStatusTargetCurrent(target)
                    ) {
                        state = OwnerStatusRead.Pending
                        scope.launch {
                            val observed = connection.fencedOwnerStatus(target)
                            if (currentSelection() && connection.ownerStatusTargetCurrent(target)) {
                                state = observed?.let { OwnerStatusRead.Observed(it) } ?: OwnerStatusRead.Unavailable
                            } else {
                                state = OwnerStatusRead.Ready
                            }
                        }
                    }
                },
            ) { Text(stringResource(R.string.chat_owner_status_action)) }
            if (state == OwnerStatusRead.Pending) {
                CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
            }
        }
        val result = when (val value = state) {
            OwnerStatusRead.Ready, OwnerStatusRead.Pending -> null
            OwnerStatusRead.Unavailable -> R.string.chat_owner_status_unavailable
            is OwnerStatusRead.Observed -> when (value.status) {
                MobileOwnerStatus.IDLE -> R.string.chat_owner_status_idle
                MobileOwnerStatus.ACTIVE -> R.string.chat_owner_status_active
                MobileOwnerStatus.INTERRUPTED -> R.string.chat_owner_status_interrupted
                MobileOwnerStatus.FAILED -> R.string.chat_owner_status_failed
            }
        }
        result?.let { Text(stringResource(it), style = MaterialTheme.typography.bodySmall) }
    }
}
