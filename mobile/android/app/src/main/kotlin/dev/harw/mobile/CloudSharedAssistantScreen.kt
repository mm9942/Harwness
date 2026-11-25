package dev.harw.mobile

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import java.util.Locale
import java.util.UUID
import kotlinx.coroutines.CancellationException
import uniffi.harw_mobile_ffi.MobileCloudAssistantSharedDevice
import uniffi.harw_mobile_ffi.MobileCloudAssistantSharedSnapshot
import uniffi.harw_mobile_ffi.MobileCloudAssistantSharedStatus
import uniffi.harw_mobile_ffi.MobileException

internal sealed interface CloudSharedAssistantAction {
    data object Status : CloudSharedAssistantAction
    data object Provision : CloudSharedAssistantAction
    data object RequestJoin : CloudSharedAssistantAction
    data object Revoke : CloudSharedAssistantAction
    data class Approve(val device: MobileCloudAssistantSharedDevice, val fingerprint: String) : CloudSharedAssistantAction
    data class Remove(val device: MobileCloudAssistantSharedDevice) : CloudSharedAssistantAction
    data class Leave(val requestId: String, val expectedStatus: MobileCloudAssistantSharedStatus) : CloudSharedAssistantAction
    class Send(val id: String, val text: String) : CloudSharedAssistantAction
}

internal fun validCloudSharedRequestId(id: String): Boolean =
    id.length == 32 && id.all { it in '0'..'9' || it in 'a'..'f' } && id.any { it != '0' }

internal fun canonicalCloudSharedFingerprint(value: String): String? {
    val trimmed = value.trim().lowercase(Locale.ROOT)
    return trimmed.takeIf { it.length == 64 && it.all { char -> char in '0'..'9' || char in 'a'..'f' } &&
        it.any { char -> char != '0' } }
}

private enum class SharedPage { Status, Conversation }
private const val SharedPromptBytes = 8 * 1024

private fun canReadSharedHistory(status: MobileCloudAssistantSharedStatus?): Boolean =
    status == MobileCloudAssistantSharedStatus.OWNER || status == MobileCloudAssistantSharedStatus.PARTICIPANT

/** Entering this separate route is opt-in. It does not copy any per-device or local data. */
@Composable
internal fun CloudSharedAssistantScreen(
    owner: CloudAssistantTarget,
    connection: HarwConnection,
    onBack: () -> Unit,
) {
    BackHandler(onBack = onBack)
    var operation by remember(owner) { mutableStateOf<CloudSharedAssistantAction?>(CloudSharedAssistantAction.Status) }
    var snapshot by remember(owner) { mutableStateOf<MobileCloudAssistantSharedSnapshot?>(null) }
    var page by remember(owner) { mutableStateOf(SharedPage.Status) }
    var draft by remember(owner) { mutableStateOf("") }
    var pending by remember(owner) { mutableStateOf<CloudSharedAssistantAction.Send?>(null) }
    var mayRetry by remember(owner) { mutableStateOf(false) }
    var notice by remember(owner) { mutableStateOf<Int?>(null) }
    var confirmation by remember(owner) { mutableStateOf<CloudSharedAssistantAction?>(null) }
    var verifyDevice by remember(owner) { mutableStateOf<MobileCloudAssistantSharedDevice?>(null) }

    fun enqueue(action: CloudSharedAssistantAction) {
        if (operation != null) return
        if (action is CloudSharedAssistantAction.Send) {
            pending = action
            mayRetry = false
        }
        operation = action
    }

    LaunchedEffect(owner, operation) {
        val request = operation ?: return@LaunchedEffect
        notice = null
        try {
            val result = connection.cloudSharedAssistantCall(owner, request)
            val visible = canReadSharedHistory(result.status)
            snapshot = result
            val sent = pending
            if (!visible) {
                pending = null
                draft = ""
                mayRetry = false
                page = SharedPage.Status
            } else if (sent != null) {
                val receipt = result.turns.firstOrNull { it.clientMessageId == sent.id }
                when {
                    receipt == null -> {
                        mayRetry = request == CloudSharedAssistantAction.Status
                        notice = if (mayRetry) R.string.cloud_shared_assistant_retry_checked
                            else R.string.cloud_shared_assistant_delivery_unknown
                    }
                    receipt.text == sent.text -> {
                        pending = null
                        draft = ""
                        mayRetry = false
                    }
                    else -> {
                        // Another admitted device can submit a caller-selected ID.
                        // The ID alone is not proof that this frozen draft arrived.
                        pending = null
                        draft = sent.text
                        mayRetry = false
                        notice = R.string.cloud_shared_assistant_receipt_conflict
                    }
                }
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            // Do not leave old member-only history on screen after denied or uncertain access.
            snapshot = null
            mayRetry = false
            notice = when (error) {
                is MobileException.Disconnected -> {
                    draft = ""
                    pending = null
                    page = SharedPage.Status
                    R.string.cloud_shared_assistant_link_ended
                }
                is MobileException.Revoked, is MobileException.Denied -> {
                    draft = ""
                    pending = null
                    page = SharedPage.Status
                    R.string.cloud_shared_assistant_authority_changed
                }
                else -> if (pending != null) R.string.cloud_shared_assistant_delivery_unknown
                    else R.string.cloud_shared_assistant_refresh_required
            }
        } finally {
            operation = null
        }
    }

    val idle = operation == null
    val current = snapshot
    val canRead = canReadSharedHistory(current?.status)
    val canChange = idle && pending == null
    Column(Modifier.fillMaxSize().imePadding()) {
        HarwHeader(stringResource(R.string.cloud_shared_assistant_title), onBack) {
            TextButton(enabled = idle, onClick = { enqueue(CloudSharedAssistantAction.Status) }) {
                Text(stringResource(R.string.cloud_shared_assistant_refresh))
            }
        }
        if (canRead) Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
            TextButton(
                enabled = page != SharedPage.Status && idle,
                onClick = { page = SharedPage.Status },
            ) { Text(stringResource(R.string.cloud_shared_assistant_status_tab)) }
            TextButton(
                enabled = page != SharedPage.Conversation && idle,
                onClick = { page = SharedPage.Conversation },
            ) { Text(stringResource(R.string.cloud_shared_assistant_conversation_tab)) }
        }
        if (!idle) LinearProgressIndicator(Modifier.fillMaxWidth())
        LazyColumn(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (page == SharedPage.Conversation && canRead && current != null) {
                if (current.turns.isEmpty()) item { Text(stringResource(R.string.cloud_shared_assistant_no_messages)) }
                items(current.turns) { turn ->
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(stringResource(R.string.cloud_shared_assistant_message), style = MaterialTheme.typography.labelLarge)
                        Text(turn.text)
                        Text(stringResource(R.string.cloud_shared_assistant_answer), style = MaterialTheme.typography.labelLarge)
                        Text(turn.answer)
                    }
                }
            } else {
                item {
                    Text(stringResource(R.string.cloud_shared_assistant_host, owner.profile.label), style = MaterialTheme.typography.labelLarge)
                    Text(stringResource(R.string.cloud_shared_assistant_disclosure))
                }
                when (val status = current?.status) {
                    MobileCloudAssistantSharedStatus.UNPROVISIONED -> item {
                        Text(stringResource(R.string.cloud_shared_assistant_unprovisioned))
                        Button(enabled = canChange, onClick = { confirmation = CloudSharedAssistantAction.Provision }) {
                            Text(stringResource(R.string.cloud_shared_assistant_provision))
                        }
                    }
                    MobileCloudAssistantSharedStatus.NOT_GRANTED -> item {
                        Text(stringResource(R.string.cloud_shared_assistant_not_granted))
                        val ownFingerprint = owner.sharedDeviceFingerprint?.let(::canonicalCloudSharedFingerprint)
                        if (ownFingerprint != null) SharedOwnFingerprint(ownFingerprint)
                        else Text(stringResource(R.string.cloud_shared_assistant_fingerprint_missing))
                        Button(
                            enabled = canChange && ownFingerprint != null,
                            onClick = { confirmation = CloudSharedAssistantAction.RequestJoin },
                        ) { Text(stringResource(R.string.cloud_shared_assistant_join)) }
                    }
                    MobileCloudAssistantSharedStatus.PENDING, MobileCloudAssistantSharedStatus.PARTICIPANT -> item {
                        val waiting = status == MobileCloudAssistantSharedStatus.PENDING
                        Text(stringResource(if (waiting) R.string.cloud_shared_assistant_pending
                            else R.string.cloud_shared_assistant_participant))
                        if (waiting) owner.sharedDeviceFingerprint?.let(::canonicalCloudSharedFingerprint)?.let { SharedOwnFingerprint(it) }
                        val id = current?.ownRequestId
                        if (id != null && validCloudSharedRequestId(id)) {
                            OutlinedButton(
                                enabled = canChange,
                                onClick = { confirmation = CloudSharedAssistantAction.Leave(id, status) },
                            ) { Text(stringResource(if (waiting) R.string.cloud_shared_assistant_withdraw else R.string.cloud_shared_assistant_leave)) }
                        }
                    }
                    MobileCloudAssistantSharedStatus.OWNER -> {
                        val devices = current?.devices.orEmpty()
                        item {
                            Text(stringResource(R.string.cloud_shared_assistant_owner))
                            Text(stringResource(R.string.cloud_shared_assistant_devices), style = MaterialTheme.typography.titleSmall)
                            if (devices.isEmpty()) Text(stringResource(R.string.cloud_shared_assistant_no_devices))
                        }
                        items(devices) { device ->
                            SharedDeviceRow(
                                device, canChange && validCloudSharedRequestId(device.requestId),
                                onApprove = { verifyDevice = device },
                                onRemove = { confirmation = CloudSharedAssistantAction.Remove(device) },
                            )
                        }
                        item {
                            OutlinedButton(enabled = canChange, onClick = { confirmation = CloudSharedAssistantAction.Revoke }) {
                                Text(stringResource(R.string.cloud_shared_assistant_revoke))
                            }
                        }
                    }
                    MobileCloudAssistantSharedStatus.REVOKED -> item { Text(stringResource(R.string.cloud_shared_assistant_revoked)) }
                    null -> if (idle) item { Text(stringResource(R.string.cloud_shared_assistant_no_status)) }
                }
            }
        }
        notice?.let { id ->
            Text(stringResource(id), Modifier.padding(horizontal = 16.dp, vertical = 6.dp), color = MaterialTheme.colorScheme.error)
        }
        if (page == SharedPage.Conversation && canRead) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
                Text(stringResource(R.string.cloud_shared_assistant_recipient_disclosure), style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(
                    value = draft,
                    onValueChange = { if (it.toByteArray(Charsets.UTF_8).size <= SharedPromptBytes) draft = it },
                    enabled = idle && pending == null,
                    label = { Text(stringResource(R.string.cloud_shared_assistant_prompt)) },
                    maxLines = 4,
                    modifier = Modifier.fillMaxWidth(),
                )
                val attempt = pending
                Button(
                    enabled = idle && if (attempt == null) draft.isNotBlank() else mayRetry,
                    onClick = {
                        if (attempt == null) {
                            if (draft.isNotBlank()) enqueue(CloudSharedAssistantAction.Send(UUID.randomUUID().toString(), draft))
                        } else if (mayRetry) enqueue(attempt)
                    },
                ) { Text(stringResource(if (attempt == null) R.string.cloud_shared_assistant_send else R.string.cloud_shared_assistant_retry)) }
            }
        }
    }

    verifyDevice?.let { device ->
        SharedVerifyDeviceDialog(device, onDismiss = { verifyDevice = null }) {
            verifyDevice = null
            enqueue(CloudSharedAssistantAction.Approve(device, it))
        }
    }
    confirmation?.let { action ->
        SharedConfirmation(action, onDismiss = { confirmation = null }) {
            confirmation = null
            enqueue(action)
        }
    }
}

@Composable
private fun SharedOwnFingerprint(fingerprint: String) {
    Text(stringResource(R.string.cloud_shared_assistant_own_fingerprint), style = MaterialTheme.typography.labelLarge)
    SelectionContainer { Text(fingerprint) }
}

@Composable
private fun SharedDeviceRow(
    device: MobileCloudAssistantSharedDevice,
    enabled: Boolean,
    onApprove: () -> Unit,
    onRemove: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(device.deviceId, style = MaterialTheme.typography.labelLarge)
        Text(device.nodeId, style = MaterialTheme.typography.bodySmall)
        Text(stringResource(if (device.pending) R.string.cloud_shared_assistant_device_pending else R.string.cloud_shared_assistant_device_approved))
        // Do not make the full pending fingerprint available for self-copy on
        // this device: approval requires the value from the requesting device.
        if (!device.pending) {
            SelectionContainer { Text(device.keyFingerprintHex, style = MaterialTheme.typography.bodySmall) }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            if (device.pending) TextButton(
                enabled = enabled && canonicalCloudSharedFingerprint(device.keyFingerprintHex) == device.keyFingerprintHex,
                onClick = onApprove,
            ) { Text(stringResource(R.string.cloud_shared_assistant_verify)) }
            TextButton(enabled = enabled, onClick = onRemove) { Text(stringResource(R.string.cloud_shared_assistant_remove)) }
        }
    }
}

@Composable
private fun SharedVerifyDeviceDialog(
    device: MobileCloudAssistantSharedDevice,
    onDismiss: () -> Unit,
    onApprove: (String) -> Unit,
) {
    var entered by remember(device) { mutableStateOf("") }
    val verified = canonicalCloudSharedFingerprint(entered)?.takeIf { it == device.keyFingerprintHex }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.cloud_shared_assistant_verify)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.cloud_shared_assistant_verify_explain, device.deviceId))
                OutlinedTextField(
                    value = entered,
                    onValueChange = { if (it.length <= 80) entered = it },
                    label = { Text(stringResource(R.string.cloud_shared_assistant_fingerprint_entry)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(enabled = verified != null, onClick = { verified?.let(onApprove) }) {
                Text(stringResource(R.string.cloud_shared_assistant_approve))
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cloud_shared_assistant_cancel)) } },
    )
}

@Composable
private fun SharedConfirmation(action: CloudSharedAssistantAction, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    val (title, detail) = when (action) {
        CloudSharedAssistantAction.Provision -> R.string.cloud_shared_assistant_provision to R.string.cloud_shared_assistant_confirm_provision
        CloudSharedAssistantAction.RequestJoin -> R.string.cloud_shared_assistant_join to R.string.cloud_shared_assistant_confirm_join
        CloudSharedAssistantAction.Revoke -> R.string.cloud_shared_assistant_revoke to R.string.cloud_shared_assistant_confirm_revoke
        is CloudSharedAssistantAction.Remove -> R.string.cloud_shared_assistant_remove to R.string.cloud_shared_assistant_confirm_remove
        is CloudSharedAssistantAction.Leave -> {
            if (action.expectedStatus == MobileCloudAssistantSharedStatus.PENDING) {
                R.string.cloud_shared_assistant_withdraw to R.string.cloud_shared_assistant_confirm_withdraw
            } else {
                R.string.cloud_shared_assistant_leave to R.string.cloud_shared_assistant_confirm_leave
            }
        }
        else -> return
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(title)) },
        text = {
            Text(if (action is CloudSharedAssistantAction.Remove) stringResource(detail, action.device.deviceId)
                else stringResource(detail))
        },
        confirmButton = { TextButton(onClick = onConfirm) { Text(stringResource(title)) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cloud_shared_assistant_cancel)) } },
    )
}
