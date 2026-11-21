package dev.harw.mobile

import java.io.ByteArrayInputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class ProfileDocumentTest {
    @Test
    fun readsUtf8WithoutChangingDocumentBytes() {
        val json = "{\"label\":\"München\"}\n"
        assertEquals(json, readProfileDocument(json.byteInputStream()))
    }

    @Test
    fun rejectsInvalidUtf8InsteadOfChangingTheImportedDocument() {
        assertThrows(IllegalArgumentException::class.java) {
            readProfileDocument(ByteArrayInputStream(byteArrayOf(0xC3.toByte(), 0x28)))
        }
    }

    @Test
    fun rejectsAnOversizedDocumentBeforePassingItToNativeParsing() {
        assertThrows(IllegalArgumentException::class.java) {
            readProfileDocument(ByteArrayInputStream(ByteArray(128 * 1024 + 1)))
        }
    }
}
