package dev.harw.mobile

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.MobileMediaPreview
import uniffi.harw_mobile_ffi.MobileMediaPreviewMode
import uniffi.harw_mobile_ffi.PromptImage

private val visiblePreviewAdmission = Semaphore(1)
private const val MaxDisplayedAttachments = 50

/** Scope must include the active connection generation and session or exact checkpoint/row. */
@Composable
internal fun SessionMediaPreviewStrip(
    scope: Any,
    retainedCount: Int,
    legacyCount: Int,
    fetch: suspend (UInt, MobileMediaPreviewMode) -> MobileMediaPreview,
) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        repeat(retainedCount.coerceIn(0, MaxDisplayedAttachments)) { index ->
            key(scope, index) {
                SessionMediaPreviewCell { mode -> fetch(index.toUInt(), mode) }
            }
        }
        repeat(legacyCount.coerceIn(0, MaxDisplayedAttachments - retainedCount.coerceIn(0, MaxDisplayedAttachments))) {
            Text("Bild ohne gespeicherten Verweis", style = MaterialTheme.typography.labelSmall)
        }
        if (retainedCount.toLong() + legacyCount > MaxDisplayedAttachments) {
            Text("Weitere Bilder nicht angezeigt", style = MaterialTheme.typography.labelSmall)
        }
    }
}

/** Never resolve old images through a current row with the same opaque ID. */
@Composable
internal fun HistoricalSessionMediaStrip(
    scope: Any,
    resolve: suspend () -> VerifiedHistoricalMedia?,
    fetch: suspend (VerifiedHistoricalMedia, UInt, MobileMediaPreviewMode) -> MobileMediaPreview,
) {
    key(scope) {
        var attempt by remember { mutableIntStateOf(0) }
        var loading by remember { mutableStateOf(false) }
        val verified by produceState<VerifiedHistoricalMedia?>(null, attempt) {
            if (attempt == 0) return@produceState
            loading = true
            value = try {
                resolve()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                null
            } finally {
                loading = false
            }
        }
        val source = verified
        if (source == null) {
            TextButton(enabled = !loading, onClick = { attempt++ }) {
                Text(when {
                    loading -> "Archivbilder werden geprüft …"
                    attempt > 0 -> "Archivbilder nicht bestätigt · erneut versuchen"
                    else -> "Bilder dieser Fassung laden"
                })
            }
        } else {
            SessionMediaPreviewStrip(
                scope = source,
                retainedCount = source.count,
                legacyCount = 0,
                fetch = { index, mode -> fetch(source, index, mode) },
            )
        }
    }
}

private sealed interface PreviewCellState {
    data object Loading : PreviewCellState
    data object OpenRequired : PreviewCellState
    data class Ready(val bitmap: Bitmap) : PreviewCellState
    data class Failed(val reason: ImageViewerFailure) : PreviewCellState
}

private suspend fun decodeSessionPreview(image: MobileMediaPreview.Found, maxPx: Int): Bitmap? =
    withContext(Dispatchers.Default) { decodeThumb(PromptImage(image.mime, image.dataB64), maxPx) }

@Composable
private fun SessionMediaPreviewCell(fetch: suspend (MobileMediaPreviewMode) -> MobileMediaPreview) {
    var attempt by remember { mutableIntStateOf(0) }
    var open by remember { mutableStateOf(false) }
    val status by produceState<PreviewCellState>(PreviewCellState.Loading, attempt) {
        value = PreviewCellState.Loading
        value = try {
            visiblePreviewAdmission.withPermit {
                val thumbnail = fetch(MobileMediaPreviewMode.THUMBNAIL)
                val result = if (thumbnail == MobileMediaPreview.OpenRequired) {
                    fetch(MobileMediaPreviewMode.VISIBLE_PREVIEW)
                } else {
                    thumbnail
                }
                when (result) {
                    is MobileMediaPreview.Found -> decodeSessionPreview(result, 288)?.let { PreviewCellState.Ready(it) }
                        ?: PreviewCellState.Failed(ImageViewerFailure.Unreadable)
                    MobileMediaPreview.OpenRequired -> PreviewCellState.OpenRequired
                    MobileMediaPreview.Missing -> PreviewCellState.Failed(ImageViewerFailure.Missing)
                    MobileMediaPreview.Unsupported -> PreviewCellState.Failed(ImageViewerFailure.Unsupported)
                    MobileMediaPreview.Denied -> PreviewCellState.Failed(ImageViewerFailure.Denied)
                    MobileMediaPreview.Unreadable -> PreviewCellState.Failed(ImageViewerFailure.Unreadable)
                    MobileMediaPreview.Offline -> PreviewCellState.Failed(ImageViewerFailure.Offline)
                    MobileMediaPreview.Busy -> PreviewCellState.Failed(ImageViewerFailure.Busy)
                }
            }
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            PreviewCellState.Failed(ImageViewerFailure.Unexpected)
        }
    }
    val result = status
    if (open) {
        ImageViewerDialog(
            load = {
                visiblePreviewAdmission.withPermit {
                    when (val full = fetch(MobileMediaPreviewMode.VIEWER)) {
                        is MobileMediaPreview.Found -> decodeSessionPreview(full, 1800)?.let { ImageViewerLoad.Ready(it) }
                            ?: ImageViewerLoad.Failed(ImageViewerFailure.Unreadable)
                        MobileMediaPreview.Missing -> ImageViewerLoad.Failed(ImageViewerFailure.Missing)
                        MobileMediaPreview.Denied -> ImageViewerLoad.Failed(ImageViewerFailure.Denied)
                        MobileMediaPreview.Unreadable -> ImageViewerLoad.Failed(ImageViewerFailure.Unreadable)
                        MobileMediaPreview.Offline -> ImageViewerLoad.Failed(ImageViewerFailure.Offline)
                        MobileMediaPreview.Busy -> ImageViewerLoad.Failed(ImageViewerFailure.Busy)
                        MobileMediaPreview.OpenRequired, MobileMediaPreview.Unsupported ->
                            ImageViewerLoad.Failed(ImageViewerFailure.Unsupported)
                    }
                }
            },
            onClose = { open = false },
        )
    }
    val tile = Modifier.size(72.dp).clip(RoundedCornerShape(10.dp))
    when (result) {
        is PreviewCellState.Ready -> Image(
            result.bitmap.asImageBitmap(),
            contentDescription = stringResource(R.string.chat_media_image),
            contentScale = ContentScale.Crop,
            modifier = tile.clickable { open = true },
        )
        else -> {
            val action: (() -> Unit)? = when (result) {
                PreviewCellState.OpenRequired -> ({ open = true })
                is PreviewCellState.Failed -> if (result.reason.retryable) ({ attempt++ }) else null
                else -> null
            }
            Box(
                modifier = tile.background(MaterialTheme.colorScheme.surfaceContainerHighest)
                    .then(if (action == null) Modifier else Modifier.clickable(
                        onClickLabel = stringResource(if (result is PreviewCellState.Failed) R.string.chat_media_retry_short else R.string.chat_media_open),
                        onClick = action,
                    )),
                contentAlignment = Alignment.Center,
            ) {
                if (result == PreviewCellState.Loading) {
                    CircularProgressIndicator(Modifier.size(20.dp))
                } else {
                    Text(
                        stringResource(when {
                            result is PreviewCellState.Failed && result.reason.retryable -> R.string.chat_media_retry_short
                            result is PreviewCellState.Failed -> result.reason.message
                            else -> R.string.chat_media_open
                        }),
                        Modifier.padding(4.dp),
                        textAlign = TextAlign.Center,
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
            }
        }
    }
}
