package dev.harw.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import android.net.Uri
import android.util.Log
import androidx.core.content.FileProvider
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.EncodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.WriterException
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import java.io.File
import java.util.concurrent.TimeUnit

internal data class PairingPhoto(val name: String, val uri: Uri)
internal class PairingCameraFileProvider : FileProvider()

internal fun newPairingPhoto(context: Context): PairingPhoto {
    val directory = File(context.cacheDir, "pairing-camera")
    check(directory.isDirectory || directory.mkdirs() || directory.isDirectory) { "The camera cache is unavailable." }
    val cutoff = System.currentTimeMillis() - TimeUnit.DAYS.toMillis(1)
    directory.listFiles()?.filter { it.lastModified() < cutoff }?.forEach(::discardPairingPhoto)
    val file = File.createTempFile("profile-", ".jpg", directory)
    return try {
        PairingPhoto(file.name, FileProvider.getUriForFile(context, "${context.packageName}.pairing-camera", file))
    } catch (error: Exception) {
        discardPairingPhoto(file)
        throw error
    }
}

internal fun discardPairingPhoto(file: File) {
    val removed = try {
        file.delete() || !file.exists()
    } catch (_: SecurityException) {
        false
    }
    if (!removed) Log.w("HarwPairing", "Temporary camera image cleanup will be retried.")
}

internal fun pairingPhotoFile(context: Context, name: String): File {
    require(name.matches(Regex("profile-[0-9A-Za-z-]+\\.jpg")))
    return File(File(context.cacheDir, "pairing-camera"), name)
}

internal fun readPairingPhoto(file: File): String {
    require(file.length() in 1..(48L * 1024 * 1024)) { "The camera did not save a readable picture." }
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.path, bounds)
    check(bounds.outWidth > 0 && bounds.outHeight > 0) { "The camera picture could not be opened." }
    var sample = 1
    while (maxOf(bounds.outWidth, bounds.outHeight) / sample > 3072) sample *= 2
    val bitmap = checkNotNull(
        BitmapFactory.decodeFile(file.path, BitmapFactory.Options().apply {
            inSampleSize = sample
            inPreferredConfig = Bitmap.Config.ARGB_8888
        }),
    ) { "The camera picture could not be opened." }
    try {
        val pixels = IntArray(bitmap.width * bitmap.height)
        bitmap.getPixels(pixels, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height)
        val image = BinaryBitmap(HybridBinarizer(RGBLuminanceSource(bitmap.width, bitmap.height, pixels)))
        return try {
            MultiFormatReader().decode(image, mapOf(
                DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
                DecodeHintType.TRY_HARDER to true,
                DecodeHintType.ALSO_INVERTED to true,
            )).text
        } catch (_: com.google.zxing.ReaderException) {
            throw IllegalArgumentException("No QR code found. Fill the camera frame with the code, or import the profile file.")
        }
    } finally {
        bitmap.recycle()
    }
}

internal fun pairingQrImage(text: String): Bitmap {
    val writer = QRCodeWriter()
    val matrix = try {
        writer.encode(text, BarcodeFormat.QR_CODE, 0, 0, mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M))
    } catch (_: WriterException) {
        writer.encode(text, BarcodeFormat.QR_CODE, 0, 0, mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.L))
    }
    val pixels = IntArray(matrix.width * matrix.height) { index ->
        if (matrix[index % matrix.width, index / matrix.width]) Color.BLACK else Color.WHITE
    }
    return Bitmap.createBitmap(pixels, matrix.width, matrix.height, Bitmap.Config.ARGB_8888)
}
