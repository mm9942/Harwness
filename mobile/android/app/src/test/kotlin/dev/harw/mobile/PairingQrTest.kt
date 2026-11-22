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
import org.junit.Assert.assertTrue
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
        assertThrows(IllegalArgumentException::class.java) { decodeProfileQr("HARW:PROFILE:3:ABC") }
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

    private fun profileQrV2(onion: String, publicKey: ByteArray): String {
        val fields = listOf("Harw", "gateway-mobile", "127.0.0.1:7443", onion).map { it.encodeToByteArray() }
        val binary = fields.map { it.size.toByte() }.toByteArray() + fields.reduce(ByteArray::plus) + publicKey
        return "HARW:PROFILE:2:" + encodeBase45(binary)
    }

    @Test
    fun profileQrV2CarriesTheOnionRoute() {
        val onion = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443"
        val publicKey = ByteArray(1952) { it.toByte() }
        val qr = profileQrV2(onion, publicKey)
        assertTrue("v2 profile text too long: ${qr.length}", qr.length < 3600)
        val profile = SiwcJson.fromJson(decodeProfileQr(qr), com.google.gson.JsonObject::class.java)
        assertEquals(1L, profile.integer("version"))
        assertEquals("Harw", profile.string("label"))
        assertEquals("gateway-mobile", profile.string("server_node_id"))
        assertEquals("127.0.0.1:7443", profile.string("endpoint"))
        assertEquals(onion, profile.string("onion"))
        assertEquals(publicKey.hex(), profile.string("server_public_key_hex"))
        val matrix = QRCodeWriter().encode(
            qr, BarcodeFormat.QR_CODE, 720, 720,
            mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M),
        )
        val pixels = IntArray(matrix.width * matrix.height) { index ->
            if (matrix[index % matrix.width, index / matrix.width]) 0xff000000.toInt() else 0xffffffff.toInt()
        }
        val image = BinaryBitmap(HybridBinarizer(RGBLuminanceSource(matrix.width, matrix.height, pixels)))
        assertEquals(qr, QRCodeReader().decode(image).text)
    }

    @Test
    fun profileQrV2RejectsOnionLocatorsJustOutsideTheAcceptedLengths() {
        for (length in listOf(61, 69)) {
            val qr = profileQrV2("a".repeat(length), ByteArray(1952) { it.toByte() })
            assertThrows("onion length $length", IllegalArgumentException::class.java) { decodeProfileQr(qr) }
        }
    }

    @Test
    fun profileQrV2AcceptsOnionLocatorsAtTheAcceptedLengthBoundaries() {
        for (length in listOf(62, 68)) {
            val onion = "a".repeat(length)
            val qr = profileQrV2(onion, ByteArray(1952) { it.toByte() })
            val profile = SiwcJson.fromJson(decodeProfileQr(qr), com.google.gson.JsonObject::class.java)
            assertEquals(onion, profile.string("onion"))
        }
    }

    @Test
    fun profileQrV1RoundTripsAtTheMaximumFieldLengths() {
        val label = "L".repeat(128)
        val nodeId = "n".repeat(128)
        val endpoint = "e".repeat(63)
        val publicKey = ByteArray(1952) { it.toByte() }
        val fields = listOf(label, nodeId, endpoint).map { it.encodeToByteArray() }
        val binary = fields.map { it.size.toByte() }.toByteArray() + fields.reduce(ByteArray::plus) + publicKey
        val qr = "HARW:PROFILE:1:" + encodeBase45(binary)
        val profile = SiwcJson.fromJson(decodeProfileQr(qr), com.google.gson.JsonObject::class.java)
        assertEquals(1L, profile.integer("version"))
        assertEquals(label, profile.string("label"))
        assertEquals(nodeId, profile.string("server_node_id"))
        assertEquals(endpoint, profile.string("endpoint"))
        assertEquals(publicKey.hex(), profile.string("server_public_key_hex"))
        assertEquals(3411, qr.length - "HARW:PROFILE:1:".length)
    }

    @Test
    fun profileQrV2RoundTripsAtTheMaximumFieldLengthsBelowTheTextLimit() {
        val label = "L".repeat(128)
        val nodeId = "n".repeat(128)
        val endpoint = "e".repeat(63)
        val onion = "o".repeat(68)
        val publicKey = ByteArray(1952) { it.toByte() }
        val fields = listOf(label, nodeId, endpoint, onion).map { it.encodeToByteArray() }
        val binary = fields.map { it.size.toByte() }.toByteArray() + fields.reduce(ByteArray::plus) + publicKey
        val qr = "HARW:PROFILE:2:" + encodeBase45(binary)
        assertEquals(3515, qr.length - "HARW:PROFILE:2:".length)
        val profile = SiwcJson.fromJson(decodeProfileQr(qr), com.google.gson.JsonObject::class.java)
        assertEquals(label, profile.string("label"))
        assertEquals(nodeId, profile.string("server_node_id"))
        assertEquals(endpoint, profile.string("endpoint"))
        assertEquals(onion, profile.string("onion"))
        assertEquals(publicKey.hex(), profile.string("server_public_key_hex"))
    }
}
