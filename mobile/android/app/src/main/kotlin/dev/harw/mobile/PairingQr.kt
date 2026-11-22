package dev.harw.mobile

private const val EnrollmentPrefix = "HARW:ENROLL:1:"
private const val PublicKeySize = 1952
private const val Alphabet = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:"

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

private fun Byte.unsigned(): Int = toInt() and 0xff

private fun ByteArray.printableAscii(start: Int, end: Int): String {
    require((start until end).all { this[it].unsigned() in 0x21..0x7e })
    return decodeToString(start, end, throwOnInvalidSequence = true)
}

internal fun ByteArray.hex(): String = toHexString()
