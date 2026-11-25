package dev.harw.mobile

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.PersistableBundle
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.BaseLevel
import uniffi.harw_mobile_ffi.DiagnosticSnapshot
import uniffi.harw_mobile_ffi.Domain
import uniffi.harw_mobile_ffi.Level

internal sealed interface DiagnosticCommand {
    data object Refresh : DiagnosticCommand
    data object Clear : DiagnosticCommand
    data class Set(val domain: Domain, val level: BaseLevel) : DiagnosticCommand
    data class Trace(val domain: Domain) : DiagnosticCommand
}

internal data class DiagnosticView(val log: DiagnosticSnapshot, val levels: Map<Domain, Level>)

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun DiagnosticsPanel(connection: HarwConnection, local: Boolean, onClose: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var domain by remember(connection, local) { mutableStateOf(Domain.CHAT) }
    var view by remember(connection, local) { mutableStateOf<DiagnosticView?>(null) }
    var working by remember(connection, local) { mutableStateOf(true) }
    var notice by remember(connection, local) { mutableStateOf<Int?>(null) }
    val clipboardLabel = stringResource(R.string.harw_diagnostics_clipboard_label)

    suspend fun apply(command: DiagnosticCommand) {
        try {
            view = connection.diagnostics(command)
            notice = if (view == null) R.string.harw_diagnostics_unavailable else null
        } catch (exception: CancellationException) {
            throw exception
        } catch (_: Exception) {
            view = null
            notice = R.string.harw_diagnostics_failed
        } finally {
            working = false
        }
    }

    fun runCommand(command: DiagnosticCommand) {
        if (working) return
        working = true
        scope.launch { apply(command) }
    }

    LaunchedEffect(connection, local) { apply(DiagnosticCommand.Refresh) }
    AlertDialog(
        onDismissRequest = onClose,
        title = {
            Text(stringResource(if (local) R.string.harw_diagnostics_title_local else R.string.harw_diagnostics_title_cloud))
        },
        text = {
            Column(Modifier.heightIn(max = 480.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.harw_diagnostics_privacy))
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    for (entry in Domain.entries) {
                        FilterChip(
                            selected = domain == entry,
                            onClick = { domain = entry },
                            label = { Text(stringResource(entry.diagnosticLabel)) },
                        )
                    }
                }
                val level = view?.levels?.get(domain)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    for (option in BaseLevel.entries) {
                        FilterChip(
                            selected = level?.name == option.name,
                            enabled = !working && view != null,
                            onClick = { runCommand(DiagnosticCommand.Set(domain, option)) },
                            label = { Text(stringResource(option.diagnosticLabel)) },
                        )
                    }
                }
                TextButton(enabled = view != null && !working, onClick = { runCommand(DiagnosticCommand.Trace(domain)) }) {
                    Text(stringResource(
                        if (level == Level.TRACE) R.string.harw_diagnostics_trace_extend
                        else R.string.harw_diagnostics_trace_enable,
                    ))
                }
                notice?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error) }
                if (working) Text(stringResource(R.string.harw_diagnostics_refreshing))
                view?.log?.let { log ->
                    Text(stringResource(R.string.harw_diagnostics_counts, log.retained.toString(), log.dropped.toString()))
                    if (log.retained == 0u) Text(stringResource(R.string.harw_diagnostics_empty))
                    else SelectionContainer { Text(log.text, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall) }
                }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    TextButton(enabled = !working, onClick = { runCommand(DiagnosticCommand.Refresh) }) {
                        Text(stringResource(R.string.harw_diagnostics_refresh))
                    }
                    val canClear = view?.log?.let { it.retained > 0u || it.dropped > 0uL } == true
                    TextButton(enabled = !working && canClear, onClick = { runCommand(DiagnosticCommand.Clear) }) {
                        Text(stringResource(R.string.harw_diagnostics_clear))
                    }
                }
            }
        },
        confirmButton = {
            TextButton(enabled = !working && (view?.log?.retained ?: 0u) > 0u, onClick = {
                val text = view?.log?.text ?: return@TextButton
                try {
                    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                    if (clipboard == null) {
                        notice = R.string.harw_diagnostics_copy_failed
                        return@TextButton
                    }
                    val clip = ClipData.newPlainText(clipboardLabel, text)
                    clip.description.extras = PersistableBundle().apply { putBoolean("android.content.extra.IS_SENSITIVE", true) }
                    clipboard.setPrimaryClip(clip)
                    notice = null
                } catch (_: RuntimeException) {
                    notice = R.string.harw_diagnostics_copy_failed
                }
            }) { Text(stringResource(R.string.harw_diagnostics_copy)) }
        },
        dismissButton = { TextButton(onClick = onClose) { Text(stringResource(R.string.harw_diagnostics_close)) } },
    )
}


private val Domain.diagnosticLabel: Int
    get() = when (this) {
        Domain.CHAT -> R.string.harw_diagnostics_domain_chat
        Domain.STREAM -> R.string.harw_diagnostics_domain_stream
        Domain.ONION -> R.string.harw_diagnostics_domain_onion
    }

private val BaseLevel.diagnosticLabel: Int
    get() = when (this) {
        BaseLevel.OFF -> R.string.harw_diagnostics_level_off
        BaseLevel.ERROR -> R.string.harw_diagnostics_level_error
        BaseLevel.WARN -> R.string.harw_diagnostics_level_warn
        BaseLevel.INFO -> R.string.harw_diagnostics_level_info
        BaseLevel.DEBUG -> R.string.harw_diagnostics_level_debug
    }
