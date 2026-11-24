package dev.harw.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.Base64
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.ui.geometry.Offset
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import uniffi.harw_mobile_ffi.PromptImage
import uniffi.harw_mobile_ffi.TranscriptEntry

/** Wie viele Bilder die App an eine Nachricht haengt (Draht erlaubt 4; die Grenze haelt Platz im 1-MiB-Limit). */
internal const val MaxPromptImages = Int.MAX_VALUE

/** Hoechstgroesse eines Bildes nach dem Verkleinern; drei Bilder passen so unter die Draht-Grenze von 800 KiB Base64. */
private const val MaxImageBytes = 180_000
private const val MaxSourceBytes = 40L * 1024 * 1024
private val EdgeSteps = intArrayOf(1280, 1024, 800, 640, 512)
private val QualitySteps = intArrayOf(80, 65, 50)

/** Ein vorbereitetes Bild fuer die Anzeige im Entwurf. */
internal data class DraftImage(
    val label: String,
    val image: PromptImage,
    val bytes: Int,
    val freshlyPicked: Boolean = false,
)

/**
 * Liest ein Bild aus dem Foto-Picker, verkleinert es und kodiert es als JPEG-Base64.
 * Gibt `null` zurueck, wenn es nicht lesbar ist oder nicht klein genug wird.
 */
internal fun prepareImage(context: Context, uri: Uri, label: String): DraftImage? {
    val resolver = context.contentResolver
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    val size = try {
        resolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        resolver.openAssetFileDescriptor(uri, "r")?.use { it.length } ?: -1L
    } catch (_: Exception) {
        return null
    }
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0 || size > MaxSourceBytes) return null
    val longest = maxOf(bounds.outWidth, bounds.outHeight)
    var sample = 1
    while (longest / (sample * 2) >= EdgeSteps[0]) sample *= 2
    val decoded = try {
        resolver.openInputStream(uri)?.use {
            BitmapFactory.decodeStream(it, null, BitmapFactory.Options().apply { inSampleSize = sample })
        }
    } catch (_: Exception) {
        null
    } ?: return null
    return try {
        val bytes = encodePickerJpeg(decoded) ?: return null
        val b64 = Base64.encodeToString(bytes, Base64.NO_WRAP)
        DraftImage(label, PromptImage("image/jpeg", b64), bytes.size, freshlyPicked = true)
    } finally {
        decoded.recycle()
    }
}

private fun encodePickerJpeg(decoded: Bitmap): ByteArray? {
    for (edge in EdgeSteps) {
        val scaled = scaleTo(decoded, edge)
        try {
            for (quality in QualitySteps) {
                val out = ByteArrayOutputStream()
                scaled.compress(Bitmap.CompressFormat.JPEG, quality, out)
                val bytes = out.toByteArray()
                if (bytes.size <= MaxImageBytes) return bytes
            }
        } finally {
            if (scaled !== decoded) scaled.recycle()
        }
    }
    return null
}

private fun scaleTo(source: Bitmap, edge: Int): Bitmap {
    val longest = maxOf(source.width, source.height)
    if (longest <= edge) return source
    val factor = edge.toFloat() / longest
    return Bitmap.createScaledBitmap(
        source,
        maxOf(1, (source.width * factor).toInt()),
        maxOf(1, (source.height * factor).toInt()),
        true,
    )
}

private const val ImagePlaceholder = "[image]"

/** The wire view joins each attachment marker as its own line, never inline with authored text. */
internal fun splitImagePlaceholders(text: String): Pair<String, Int> {
    val lines = text.split('\n')
    val count = lines.count { it == ImagePlaceholder }
    if (count == 0) return text to 0
    return lines.filterNot { it == ImagePlaceholder }.joinToString("\n") to count
}

/** New rows keep actual attachments separate from text the user typed literally. */
internal fun TranscriptEntry.userTextAndImageCount(): Pair<String, Int> {
    val typed = userPresentation
    if (typed != null) return typed.text to maxOf(typed.imageCount.coerceAtMost(Int.MAX_VALUE.toUInt()).toInt(), images.size)
    val (authored, legacyCount) = splitImagePlaceholders(text)
    return authored to maxOf(legacyCount, images.size)
}

internal fun decodeThumb(image: PromptImage, targetPx: Int): Bitmap? = try {
    val bytes = Base64.decode(image.dataB64, Base64.DEFAULT)
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    val longest = maxOf(bounds.outWidth, bounds.outHeight)
    if (bounds.outWidth !in 1..32_768 || bounds.outHeight !in 1..32_768 || targetPx <= 0) {
        null
    } else {
        var sample = 1
        while (longest / sample > targetPx) sample *= 2
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = sample })
    }
} catch (_: Exception) {
    null
}

/** Entwurfsbild als kleine Vorschau mit Entfernen-Knopf (x). */
@Composable
internal fun DraftThumbnail(image: DraftImage, enabled: Boolean, onRemove: () -> Unit) {
    val bitmap by produceState<ImageBitmap?>(null, image.image.dataB64) {
        value = withContext(Dispatchers.Default) { decodeThumb(image.image, 192)?.asImageBitmap() }
    }
    val shape = RoundedCornerShape(10.dp)
    var open by remember(image.image.dataB64) { mutableStateOf(false) }
    if (open) ImageViewerDialog(
        load = {
            decodeThumb(image.image, 1800)?.let { ImageViewerLoad.Ready(it) }
                ?: ImageViewerLoad.Failed(ImageViewerFailure.Unreadable)
        },
        onClose = { open = false },
    )
    Box(Modifier.size(72.dp)) {
        val shown = bitmap
        if (shown != null) {
            Image(shown, contentDescription = image.label, contentScale = ContentScale.Crop, modifier = Modifier.matchParentSize().clip(shape).clickable { open = true })
        } else {
            Box(Modifier.matchParentSize().clip(shape).background(MaterialTheme.colorScheme.surfaceContainerHighest))
        }
        Surface(
            onClick = onRemove,
            enabled = enabled,
            shape = RoundedCornerShape(50),
            color = MaterialTheme.colorScheme.surface.copy(alpha = 0.85f),
            modifier = Modifier.align(Alignment.TopEnd).padding(3.dp).size(22.dp),
        ) {
            Box(contentAlignment = Alignment.Center) {
                Icon(Icons.Outlined.Close, contentDescription = "${image.label} entfernen", Modifier.size(14.dp))
            }
        }
    }
}

internal enum class ImageViewerFailure(val message: Int, val retryable: Boolean = false) {
    Missing(R.string.chat_media_missing),
    Denied(R.string.chat_media_denied),
    Unsupported(R.string.chat_media_unsupported),
    Unreadable(R.string.chat_media_unreadable),
    Offline(R.string.chat_media_offline, true),
    Busy(R.string.chat_media_busy, true),
    Unexpected(R.string.chat_media_unexpected, true),
}

internal sealed interface ImageViewerLoad {
    data object Loading : ImageViewerLoad
    data class Ready(val bitmap: Bitmap) : ImageViewerLoad
    data class Failed(val reason: ImageViewerFailure) : ImageViewerLoad
}

private fun boundViewerOffset(wanted: Offset, viewport: IntSize, image: ImageBitmap, scale: Float): Offset {
    if (viewport.width <= 0 || viewport.height <= 0 || scale <= 1f) return Offset.Zero
    val fit = minOf(viewport.width.toFloat() / image.width, viewport.height.toFloat() / image.height)
    val x = ((image.width * fit * scale - viewport.width) / 2f).coerceAtLeast(0f)
    val y = ((image.height * fit * scale - viewport.height) / 2f).coerceAtLeast(0f)
    return Offset(wanted.x.coerceIn(-x, x), wanted.y.coerceIn(-y, y))
}

/** View the authorized image occurrence ONLY. User gestures never change the media scope. */
@Composable
internal fun ImageViewerDialog(load: suspend () -> ImageViewerLoad, onClose: () -> Unit) {
    var attempt by remember { androidx.compose.runtime.mutableIntStateOf(0) }
    val result by produceState<ImageViewerLoad>(ImageViewerLoad.Loading, attempt) {
        value = ImageViewerLoad.Loading
        value = try {
            withContext(Dispatchers.IO) { load() }
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            ImageViewerLoad.Failed(ImageViewerFailure.Unexpected)
        }
    }
    val shown = remember(result) { (result as? ImageViewerLoad.Ready)?.bitmap?.asImageBitmap() }
    var scale by remember(result) { mutableFloatStateOf(1f) }
    var offset by remember(result) { mutableStateOf(Offset.Zero) }
    var viewport by remember { mutableStateOf(IntSize.Zero) }

    fun transformAt(image: ImageBitmap, bounds: IntSize, point: Offset, zoom: Float, pan: Offset = Offset.Zero) {
        val target = zoom.coerceIn(1f, 6f)
        val center = Offset(bounds.width / 2f, bounds.height / 2f)
        val pivot = point - center
        val wanted = pivot - (pivot - offset) * (target / scale) + pan
        offset = boundViewerOffset(wanted, bounds, image, target)
        scale = target
    }

    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(Modifier.fillMaxSize().clipToBounds().background(Color.Black.copy(alpha = 0.94f))) {
            when (val snapshot = result) {
                ImageViewerLoad.Loading -> CircularProgressIndicator(Modifier.align(Alignment.Center))
                is ImageViewerLoad.Failed -> {
                    val reason = snapshot.reason
                    val label = Modifier.align(Alignment.Center).padding(24.dp)
                    if (reason.retryable) {
                        TextButton(onClick = { attempt++ }, modifier = label) {
                            Text(stringResource(R.string.chat_media_retry, stringResource(reason.message)), color = Color.White)
                        }
                    } else {
                        Text(stringResource(reason.message), modifier = label, color = Color.White)
                    }
                }
                is ImageViewerLoad.Ready -> if (shown != null) {
                    Image(
                        shown, contentDescription = stringResource(R.string.chat_media_image), contentScale = ContentScale.Fit,
                        modifier = Modifier.fillMaxSize()
                            .onSizeChanged { size ->
                                viewport = size
                                offset = boundViewerOffset(offset, size, shown, scale)
                            }
                            .pointerInput(shown) {
                                detectTapGestures(onDoubleTap = { point ->
                                    transformAt(shown, size, point, if (scale > 1f) 1f else 2.5f)
                                })
                            }
                            .pointerInput(shown) {
                                detectTransformGestures { point, pan, zoom, _ ->
                                    transformAt(shown, size, point, scale * zoom, pan)
                                }
                            }
                            .graphicsLayer(scaleX = scale, scaleY = scale, translationX = offset.x, translationY = offset.y),
                    )
                    TextButton(
                        onClick = {
                            val center = Offset(viewport.width / 2f, viewport.height / 2f)
                            transformAt(shown, viewport, center, if (scale > 1f) 1f else 2.5f)
                        },
                        modifier = Modifier.align(Alignment.BottomEnd).padding(14.dp),
                    ) {
                        Text(stringResource(if (scale > 1f) R.string.chat_media_zoom_reset else R.string.chat_media_zoom_in), color = Color.White)
                    }
                }
            }
            Surface(
                onClick = onClose,
                shape = RoundedCornerShape(50),
                color = Color.Black.copy(alpha = 0.6f),
                contentColor = Color.White,
                modifier = Modifier.align(Alignment.TopEnd).padding(16.dp).size(44.dp),
            ) {
                Box(contentAlignment = Alignment.Center) {
                    Icon(Icons.Outlined.Close, contentDescription = stringResource(R.string.chat_media_close), Modifier.size(26.dp))
                }
            }
        }
    }
}
