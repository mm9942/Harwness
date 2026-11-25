package dev.harw.mobile

import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.EncodeHintType
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import org.junit.Assert.assertEquals
import org.junit.Test

class PairingQrTest {
    @Test
    fun base45EncodesTheRfcVector() {
        assertEquals("BB8", encodeBase45("AB".encodeToByteArray()))
        assertEquals(" 0", encodeBase45(byteArrayOf(36)))
    }

    @Test
    fun anEntireEnrollmentFitsAndDecodesInOneQrAtMediumCorrection() {
        val text = enrollmentQrText("Harw Android", "phone-mobile", ByteArray(1952) { it.toByte() }.hex())
        val matrix = QRCodeWriter().encode(
            text, BarcodeFormat.QR_CODE, 720, 720,
            mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M),
        )
        val pixels = IntArray(matrix.width * matrix.height) { index ->
            if (matrix[index % matrix.width, index / matrix.width]) 0xff000000.toInt() else 0xffffffff.toInt()
        }
        val image = BinaryBitmap(HybridBinarizer(RGBLuminanceSource(matrix.width, matrix.height, pixels)))
        assertEquals(text, QRCodeReader().decode(image).text)
    }
}
