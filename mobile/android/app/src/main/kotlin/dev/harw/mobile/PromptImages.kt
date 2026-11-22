package dev.harw.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.Base64
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
