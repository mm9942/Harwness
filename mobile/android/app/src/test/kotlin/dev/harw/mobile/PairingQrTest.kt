package dev.harw.mobile

import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.EncodeHintType
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import java.security.MessageDigest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class PairingQrTest {
    @Test
    fun profileQrMatchesTheHostInteroperabilityVector() {
        val publicKey = ByteArray(1952) { it.toByte() }
        val fields = listOf("Harw", "gateway-mobile", "127.0.0.1:7443").map { it.encodeToByteArray() }
        val binary = fields.map { it.size.toByte() }.toByteArray() + fields.reduce(ByteArray::plus) + publicKey
        val qr = "HARW:PROFILE:1:" + encodeBase45(binary)
        assertEquals(2996, qr.length)
        assertEquals(
            "0abd4b9fb444aa9a9c45e588668bb2a6981ab4a3b3bf95bf36dcf50bc3b50181",
            MessageDigest.getInstance("SHA-256").digest(qr.encodeToByteArray()).hex(),
        )
        val profile = SiwcJson.fromJson(decodeProfileQr(qr), com.google.gson.JsonObject::class.java)
        assertEquals(1L, profile.integer("version"))
        assertEquals("Harw", profile.string("label"))
        assertEquals("gateway-mobile", profile.string("server_node_id"))
        assertEquals("127.0.0.1:7443", profile.string("endpoint"))
        assertEquals(publicKey.hex(), profile.string("server_public_key_hex"))
    }

    @Test
    fun rejectsNonProfileVersionAndMalformedBase45() {
        assertThrows(IllegalArgumentException::class.java) { decodeProfileQr("HARW:ENROLL:1:ABC") }
        assertThrows(IllegalArgumentException::class.java) { decodeProfileQr("HARW:PROFILE:2:ABC") }
        assertThrows(IllegalArgumentException::class.java) { decodeProfileQr("HARW:PROFILE:1:::!") }
        assertThrows(IllegalArgumentException::class.java) { decodeProfileQr("HARW:PROFILE:1::::") }
    }

    @Test
    fun base45UsesTheRfcVectorAndKeepsWhitespaceSignificant() {
        assertEquals("BB8", encodeBase45("AB".encodeToByteArray()))
        assertEquals("AB", decodeBase45("BB8").decodeToString())
        val bytes = byteArrayOf(36)
        assertEquals(" 0", encodeBase45(bytes))
        assertEquals(36.toByte(), decodeBase45(" 0").single())
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
