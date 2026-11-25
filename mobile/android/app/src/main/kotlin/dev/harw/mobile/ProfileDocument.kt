package dev.harw.mobile

import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException

private const val MaxProfileBytes = 128 * 1024

internal fun readProfileDocument(input: InputStream): String {
    val bytes = ByteArrayOutputStream()
    val buffer = ByteArray(8192)
    while (true) {
        val count = input.read(buffer, 0, minOf(buffer.size, MaxProfileBytes + 1 - bytes.size()))
        if (count < 0) break
        require(bytes.size() + count <= MaxProfileBytes) { "The profile exceeds 128 KiB." }
        bytes.write(buffer, 0, count)
    }
    return try {
        Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(bytes.toByteArray())).toString()
    } catch (_: CharacterCodingException) {
        throw IllegalArgumentException("The profile must be valid UTF-8.")
    }
}
