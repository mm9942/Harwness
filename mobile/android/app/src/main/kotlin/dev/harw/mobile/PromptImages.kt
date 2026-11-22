package dev.harw.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.Base64
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import uniffi.harw_mobile_ffi.PromptImage

/** Wie viele Bilder die App an eine Nachricht haengt (Draht erlaubt 4; die Grenze haelt Platz im 1-MiB-Limit). */
internal const val MaxPromptImages = 3

/** Hoechstgroesse eines Bildes nach dem Verkleinern; drei Bilder passen so unter die Draht-Grenze von 800 KiB Base64. */
private const val MaxImageBytes = 180_000
private const val MaxSourceBytes = 40L * 1024 * 1024
private val EdgeSteps = intArrayOf(1280, 1024, 800, 640, 512)
private val QualitySteps = intArrayOf(80, 65, 50)

/** Ein vorbereitetes Bild fuer die Anzeige im Entwurf. */
internal data class DraftImage(val label: String, val image: PromptImage, val bytes: Int)

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
    try {
        for (edge in EdgeSteps) {
            val scaled = scaleTo(decoded, edge)
            for (quality in QualitySteps) {
                val out = ByteArrayOutputStream()
                scaled.compress(Bitmap.CompressFormat.JPEG, quality, out)
                val bytes = out.toByteArray()
                if (bytes.size <= MaxImageBytes) {
                    if (scaled !== decoded) scaled.recycle()
                    val b64 = Base64.encodeToString(bytes, Base64.NO_WRAP)
                    return DraftImage(label, PromptImage("image/jpeg", b64), bytes.size)
                }
            }
            if (scaled !== decoded) scaled.recycle()
        }
        return null
    } finally {
        decoded.recycle()
    }
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

/** Trennt die `[image]`-Platzhalter des Servers vom Text: (Text ohne Platzhalter, Anzahl Bilder). */
internal fun splitImagePlaceholders(text: String): Pair<String, Int> {
    val count = text.split(ImagePlaceholder).size - 1
    if (count == 0) return text to 0
    return text.replace(ImagePlaceholder, "").trim() to count
}

/** Vergleichsschluessel fuer gesendeten Text; ein reiner Bild-Prompt wird als "(Bild)" gesendet. */
internal fun sentTextKey(text: String): String = text.trim().let { if (it == "(Bild)") "" else it }

/** Kleine Vorschauen gesendeter Bilder; ohne lokale Kopie (Verlauf vom Server) je ein "Bild"-Chip. */
@Composable
internal fun ImageStrip(count: Int, thumbs: List<PromptImage>?) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (thumbs != null && thumbs.size == count) {
            thumbs.forEach { Thumbnail(it) }
        } else {
            repeat(count) {
                Surface(
                    shape = RoundedCornerShape(50),
                    color = MaterialTheme.colorScheme.surfaceContainerHighest,
                    contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                ) {
                    Row(
                        Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(5.dp),
                    ) {
                        Icon(Icons.Outlined.Image, contentDescription = null, Modifier.size(16.dp))
                        Text("Bild", style = MaterialTheme.typography.labelMedium)
                    }
                }
            }
        }
    }
}

@Composable
private fun Thumbnail(image: PromptImage) {
    val bitmap by produceState<ImageBitmap?>(null, image.dataB64) {
        value = withContext(Dispatchers.Default) {
            try {
                val bytes = Base64.decode(image.dataB64, Base64.DEFAULT)
                BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = 4 })?.asImageBitmap()
            } catch (_: Exception) {
                null
            }
        }
    }
    val shape = RoundedCornerShape(10.dp)
    val shown = bitmap
    if (shown != null) {
        Image(shown, contentDescription = "Gesendetes Bild", contentScale = ContentScale.Crop, modifier = Modifier.size(72.dp).clip(shape))
    } else {
        Box(Modifier.size(72.dp).clip(shape).background(MaterialTheme.colorScheme.surfaceContainerHighest))
    }
}
