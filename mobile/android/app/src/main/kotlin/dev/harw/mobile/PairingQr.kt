package dev.harw.mobile

import com.google.gson.JsonObject

private const val ProfilePrefix = "HARW:PROFILE:1:"
private const val ProfilePrefixV2 = "HARW:PROFILE:2:"
private const val EnrollmentPrefix = "HARW:ENROLL:1:"
private const val PublicKeySize = 1952
private const val Alphabet = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:"

internal fun decodeProfileQr(text: String): String {
    val withOnion = when {
        text.startsWith(ProfilePrefix) -> false
        text.startsWith(ProfilePrefixV2) -> true
        else -> throw IllegalArgumentException("This is not a Harw server profile QR code.")
    }
    val decoded = decodeBase45(text.removePrefix(if (withOnion) ProfilePrefixV2 else ProfilePrefix))
    val header = if (withOnion) 4 else 3
    require(decoded.size >= PublicKeySize + header) { "The server profile code is incomplete." }
    val labelSize = decoded[0].unsigned()
    val nodeSize = decoded[1].unsigned()
    val endpointSize = decoded[2].unsigned()
    val onionSize = if (withOnion) decoded[3].unsigned() else 0
    require(labelSize in 1..128 && nodeSize in 1..128 && endpointSize in 1..63) { "The profile QR has invalid fields." }
    require(!withOnion || onionSize in 62..68) { "The profile QR has invalid fields." }
    require(decoded.size == header + labelSize + nodeSize + endpointSize + onionSize + PublicKeySize) { "The profile QR has an invalid length." }
    val nodeStart = header + labelSize
    val endpointStart = nodeStart + nodeSize
    val onionStart = endpointStart + endpointSize
    val keyStart = onionStart + onionSize
    val label = decoded.decodeToString(header, nodeStart, throwOnInvalidSequence = true)
    val nodeId = decoded.printableAscii(nodeStart, endpointStart)
    val endpoint = decoded.printableAscii(endpointStart, onionStart)
    val onion = if (withOnion) decoded.printableAscii(onionStart, keyStart) else null
    return SiwcJson.toJson(JsonObject().apply {
        addProperty("version", 1)
        addProperty("label", label)
        addProperty("server_node_id", nodeId)
        addProperty("endpoint", endpoint)
        if (onion != null) addProperty("onion", onion)
        addProperty("server_public_key_hex", decoded.copyOfRange(keyStart, decoded.size).hex())
    })
}

internal fun enrollmentQrText(label: String, nodeId: String, publicKeyHex: String): String {
    val labelBytes = label.encodeToByteArray()
    val nodeBytes = nodeId.encodeToByteArray()
    require(labelBytes.size in 1..128 && nodeBytes.size in 1..128)
    nodeBytes.printableAscii(0, nodeBytes.size)
    val key = publicKeyHex.hexToByteArray()
    require(key.size == PublicKeySize)
    return EnrollmentPrefix + encodeBase45(
        byteArrayOf(labelBytes.size.toByte(), nodeBytes.size.toByte()) + labelBytes + nodeBytes + key,
    )
}

internal fun encodeBase45(bytes: ByteArray): String = buildString((bytes.size * 3 + 1) / 2) {
    var index = 0
    while (index + 1 < bytes.size) {
        val value = bytes[index].unsigned() * 256 + bytes[index + 1].unsigned()
        append(Alphabet[value % 45])
        append(Alphabet[value / 45 % 45])
        append(Alphabet[value / 2025])
        index += 2
    }
    if (index < bytes.size) {
        val value = bytes[index].unsigned()
        append(Alphabet[value % 45])
        append(Alphabet[value / 45])
    }
}

internal fun decodeBase45(text: String): ByteArray {
    require(text.length <= 3600 && text.length % 3 != 1) { "Invalid Harw QR encoding." }
    val bytes = ByteArray(text.length / 3 * 2 + if (text.length % 3 == 2) 1 else 0)
    var input = 0
    var output = 0
    while (input + 2 < text.length) {
        val value = digit(text[input]) + 45 * digit(text[input + 1]) + 2025 * digit(text[input + 2])
        require(value <= 0xffff) { "Invalid Harw QR encoding." }
        bytes[output++] = (value / 256).toByte()
        bytes[output++] = value.toByte()
        input += 3
    }
    if (input < text.length) {
        val value = digit(text[input]) + 45 * digit(text[input + 1])
        require(value <= 0xff) { "Invalid Harw QR encoding." }
        bytes[output] = value.toByte()
    }
    return bytes
}

private fun digit(char: Char): Int = Alphabet.indexOf(char).also {
    require(it >= 0) { "Invalid Harw QR encoding." }
}

private fun Byte.unsigned(): Int = toInt() and 0xff

private fun ByteArray.printableAscii(start: Int, end: Int): String {
    require((start until end).all { this[it].unsigned() in 0x21..0x7e })
    return decodeToString(start, end, throwOnInvalidSequence = true)
}

internal fun ByteArray.hex(): String = toHexString()
