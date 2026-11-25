package dev.harw.mobile

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private data class BackupRequest(val uri: Uri, val restore: Boolean)

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun LocalBackupSettings(localDisconnected: Boolean, disconnectLocal: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var request by remember { mutableStateOf<BackupRequest?>(null) }
    var password by remember { mutableStateOf("") }
    var confirmation by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }

    val export = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        if (uri != null) request = BackupRequest(uri, false)
    }
    val restore = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) request = BackupRequest(uri, true)
    }

    Column(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(stringResource(R.string.local_backup_title), style = MaterialTheme.typography.bodyLarge)
        val supportingColor = MaterialTheme.colorScheme.onSurfaceVariant
        val supportingStyle = MaterialTheme.typography.bodySmall
        Text(stringResource(R.string.local_backup_scope), color = supportingColor, style = supportingStyle)
        if (!localDisconnected) {
            Text(stringResource(R.string.local_backup_disconnect), color = supportingColor, style = supportingStyle)
            TextButton(enabled = !busy, onClick = disconnectLocal, contentPadding = PaddingValues(0.dp)) {
                Text(stringResource(R.string.local_backup_disconnect_action))
            }
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            TextButton(
                enabled = !busy && localDisconnected,
                onClick = { export.launch("harw-backup.hwb") },
                contentPadding = PaddingValues(0.dp),
            ) { Text(stringResource(R.string.local_backup_export)) }
            TextButton(
                enabled = !busy && localDisconnected,
                onClick = { restore.launch(arrayOf("*/*")) },
                contentPadding = PaddingValues(0.dp),
            ) { Text(stringResource(R.string.local_backup_restore)) }
        }
        message?.let { Text(it, color = supportingColor, style = supportingStyle) }
    }

    fun dismiss() {
        request = null
        password = ""
        confirmation = ""
    }

    val selected = request ?: return
    val operation = if (selected.restore) R.string.local_backup_restore else R.string.local_backup_export
    AlertDialog(
        onDismissRequest = ::dismiss,
        title = { Text(stringResource(operation)) },
        text = {
            Column {
                if (selected.restore) Text(stringResource(R.string.local_backup_restore_detail))
                OutlinedTextField(
                    value = password,
                    onValueChange = { if (it.length <= 256) password = it },
                    label = { Text(stringResource(R.string.local_backup_password)) },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                )
                if (!selected.restore) OutlinedTextField(
                    value = confirmation,
                    onValueChange = { if (it.length <= 256) confirmation = it },
                    label = { Text(stringResource(R.string.local_backup_confirm_password)) },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                )
            }
        },
        confirmButton = {
            TextButton(
                enabled = localDisconnected && password.length >= 12 && (selected.restore || confirmation == password),
                onClick = {
                    val key = password.toCharArray()
                    dismiss()
                    busy = true
                    message = null
                    scope.launch {
                        try {
                            val restored = withContext(Dispatchers.IO) {
                                if (selected.restore) LocalBackup.restore(context, selected.uri, key)
                                else {
                                    LocalBackup.export(context, selected.uri, key)
                                    0
                                }
                            }
                            message = if (selected.restore) context.getString(R.string.local_backup_restored, restored)
                            else context.getString(R.string.local_backup_exported)
                        } catch (error: CancellationException) {
                            throw error
                        } catch (_: Exception) {
                            message = context.getString(R.string.local_backup_failed)
                        } finally {
                            key.fill('\u0000')
                            busy = false
                        }
                    }
                },
            ) { Text(stringResource(operation)) }
        },
        dismissButton = { TextButton(onClick = ::dismiss) { Text(stringResource(android.R.string.cancel)) } },
    )
}
