package dev.harw.mobile

import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

internal enum class ChatSlashCommand { Export, Help, Unknown }

/** A leading slash is reserved; no unrecognized command becomes an AI prompt. */
internal fun parseChatSlashCommand(text: String): ChatSlashCommand? {
    val input = text.trim()
    if (!input.startsWith('/')) return null
    return when (input.lowercase()) {
        "/export", "/transcript" -> ChatSlashCommand.Export
        "/", "/help" -> ChatSlashCommand.Help
        else -> ChatSlashCommand.Unknown
    }
}

/** The composer has exactly one destination per tap: local command or model prompt. */
internal fun routeChatComposerSend(
    text: String,
    sendPrompt: () -> Unit,
    handleCommand: (ChatSlashCommand) -> Unit,
) {
    val command = parseChatSlashCommand(text)
    if (command == null) sendPrompt() else handleCommand(command)
}

private const val MaxTranscriptBytes = 4 * 1024 * 1024

internal sealed interface ChatTranscriptText {
    data class Ready(val text: String) : ChatTranscriptText
    data object Empty : ChatTranscriptText
    data object TooLarge : ChatTranscriptText
}

/** Export precisely the bound, visible user and agent entries, never internal frames or media references. */
internal fun renderChatTranscript(
    entries: List<TranscriptEntry>,
    userLabel: String,
    agentLabel: String,
    imageNote: (Int) -> String,
): ChatTranscriptText {
    val output = StringBuilder()
    for (entry in entries) {
        val label = when (entry.kind) {
            TranscriptKind.USER -> userLabel
            TranscriptKind.ASSISTANT -> agentLabel
            else -> continue
        }
        if (entry.kind == TranscriptKind.USER && !isDurableRewriteItemId(entry.id)) continue
        val (text, images) = if (entry.kind == TranscriptKind.USER) {
            entry.userTextAndImageCount()
        } else entry.text to 0
        if (output.length.toLong() + label.length + text.length > MaxTranscriptBytes) {
            return ChatTranscriptText.TooLarge
        }
        if (output.isNotEmpty()) output.append("\n\n")
        output.append(label).append(":\n").append(text)
        if (images > 0) output.append('\n').append(imageNote(images))
    }
    if (output.isEmpty()) return ChatTranscriptText.Empty
    val text = output.append('\n').toString()
    return if (text.toByteArray(Charsets.UTF_8).size <= MaxTranscriptBytes) {
        ChatTranscriptText.Ready(text)
    } else ChatTranscriptText.TooLarge
}

private class PendingTranscriptExport(val text: String, val stillAuthorized: () -> Boolean)

/** Nothing is shared automatically: the OS picker is the user's destination choice. */
@Composable
internal fun rememberChatTranscriptExporter(): (String, () -> Boolean) -> Boolean {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var pending by remember { mutableStateOf<PendingTranscriptExport?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("text/plain")) { uri ->
        val request = pending
        pending = null
        if (uri == null || request == null) return@rememberLauncherForActivityResult
        if (uri.scheme != "content" || !request.stillAuthorized()) {
            Toast.makeText(context, R.string.chat_slash_export_changed, Toast.LENGTH_LONG).show()
            return@rememberLauncherForActivityResult
        }
        scope.launch {
            val saved = try {
                withContext(Dispatchers.IO) {
                    if (!request.stillAuthorized()) return@withContext false
                    val output = context.contentResolver.openOutputStream(uri, "wt")
                        ?: return@withContext false
                    output.bufferedWriter(Charsets.UTF_8).use { it.write(request.text) }
                    true
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                false
            }
            Toast.makeText(
                context,
                if (saved) R.string.chat_slash_export_saved else R.string.chat_slash_export_failed,
                Toast.LENGTH_LONG,
            ).show()
        }
    }
    return { text, guard ->
        if (pending != null || !guard()) false
        else {
            pending = PendingTranscriptExport(text, guard)
            try {
                picker.launch("harw-transcript.txt")
                true
            } catch (_: Exception) {
                pending = null
                false
            }
        }
    }
}
