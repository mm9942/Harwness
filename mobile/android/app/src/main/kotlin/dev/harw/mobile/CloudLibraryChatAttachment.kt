package dev.harw.mobile

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.LocalSaveableStateRegistry
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import kotlinx.coroutines.CancellationException
import uniffi.harw_mobile_ffi.MobileCloudLibraryAsset
import uniffi.harw_mobile_ffi.MobileCloudLibraryCommand
import uniffi.harw_mobile_ffi.MobileCloudLibraryReply
import uniffi.harw_mobile_ffi.MobileCloudLibraryScope
import uniffi.harw_mobile_ffi.ServerProfile

internal data class CloudLibraryChatTarget<P, S>(
    val profile: P,
    val scope: S,
    val generation: Long,
    val sessionId: String,
) {
    fun permits(
        actualProfile: P?, actualScope: S?, actualGeneration: Long?, selectedSession: String?,
        local: Boolean, ready: Boolean, foreground: Boolean,
    ): Boolean = !local && ready && foreground && actualProfile == profile && actualScope == scope &&
        actualGeneration == generation && selectedSession == sessionId
}

internal fun <D, H> sameCloudLibrarySend(
    picked: Any, stillSelected: Any?, originalDraft: D, currentDraft: D, originalHead: H, currentHead: H?,
): Boolean = picked === stillSelected && originalDraft == currentDraft && originalHead == currentHead

internal typealias ScopedCloudChatLibraryTarget = CloudLibraryChatTarget<ServerProfile, MobileCloudLibraryScope>

/** In-memory selection only, never serialised or resolved as a generic media reference. */
internal class CloudChatLibraryItem(
    val target: ScopedCloudChatLibraryTarget,
    val revision: ULong,
    val assetId: String,
    val label: String,
)

@Composable
internal fun CloudLibraryChatPicker(
    connection: HarwConnection,
    target: ScopedCloudChatLibraryTarget,
    onPick: (CloudChatLibraryItem) -> Unit,
    onClose: () -> Unit,
) {
    var snapshot by remember(target) { mutableStateOf<MobileCloudLibraryReply.Snapshot?>(null) }
    var failed by remember(target) { mutableStateOf(false) }
    var query by remember(target) { mutableStateOf("") }
    LaunchedEffect(connection, target) {
        try {
            snapshot = connection.cloudLibrary(target.scope, MobileCloudLibraryCommand.List)
                as? MobileCloudLibraryReply.Snapshot
            failed = snapshot == null
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            failed = true
        }
    }
    val protection = readScreenshotProtection(LocalContext.current)
    CompositionLocalProvider(LocalSaveableStateRegistry provides null) {
        AlertDialog(
            onDismissRequest = onClose,
            properties = DialogProperties(securePolicy = if (protection) SecureFlagPolicy.SecureOn else SecureFlagPolicy.SecureOff),
            title = { Text(stringResource(R.string.chat_attachment_library)) },
            text = {
                Column {
                    Text(stringResource(R.string.chat_attachment_library_disclosure))
                    val current = snapshot
                    when {
                        failed -> Text(stringResource(R.string.chat_attachment_library_unavailable))
                        current == null -> CircularProgressIndicator()
                        current.assets.isEmpty() -> Text(stringResource(R.string.chat_attachment_library_empty))
                        else -> {
                            val search = query.trim()
                            val matches = remember(current, search) {
                                if (search.isEmpty()) current.assets else current.assets.filter {
                                    it.name.contains(search, ignoreCase = true) ||
                                        it.description?.contains(search, ignoreCase = true) == true
                                }
                            }
                            OutlinedTextField(
                                value = query, onValueChange = { if (it.length <= 160) query = it },
                                label = { Text(stringResource(R.string.chat_attachment_library_search)) },
                                singleLine = true,
                                modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
                            )
                            if (matches.isEmpty()) Text(stringResource(R.string.chat_attachment_library_no_match))
                            else LazyColumn(Modifier.heightIn(max = 245.dp)) {
                                items(matches, key = { it.assetId }) { item: MobileCloudLibraryAsset ->
                                    TextButton(
                                        onClick = { onPick(CloudChatLibraryItem(target, current.revision, item.assetId, item.name)) },
                                        modifier = Modifier.fillMaxWidth(),
                                    ) {
                                        Column(Modifier.fillMaxWidth()) {
                                            Text(item.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                            item.description?.takeUnless { it.isBlank() }?.let {
                                                Text(it, maxLines = 2, overflow = TextOverflow.Ellipsis,
                                                    style = MaterialTheme.typography.bodySmall)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            confirmButton = { TextButton(onClick = onClose) { Text(stringResource(R.string.chat_media_close_picker)) } },
        )
    }
}
