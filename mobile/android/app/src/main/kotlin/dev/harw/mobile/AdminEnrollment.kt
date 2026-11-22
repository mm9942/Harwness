package dev.harw.mobile

import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/** Ergebnis der Anfrage an den Admin-Dienst: Anfrage-ID und Prüfcode des Schlüssels. */
internal data class AdminRequest(val id: String, val fingerprint: String, val base: String)

internal val AdminRequestPattern = Regex("^[0-9a-f]{32}$")

/** Anfragen laufen serverseitig nach 10 Minuten ab; die App wartet etwas länger auf den Rückweg. */
internal const val AdminRequestMaxAgeMs = 15 * 60 * 1000L

private const val MaxAdminResponseBytes = 16 * 1024
private const val AdminTimeoutMs = 15_000

/**
 * Prüft und normalisiert die Admin-Adresse: nur `https://host[:port][/pfad]`, druckbares ASCII,
 * keine Zugangsdaten, kein Query, kein Fragment. Die Adresse steht nie im Code, sondern kommt
 * aus dem Serverprofil oder der Eingabe des Nutzers.
 */
internal fun normalizeAdminUrl(input: String): String? {
    val text = input.trim().trimEnd('/')
    if (text.length > 256 || !text.startsWith("https://")) return null
    if (text.any { it.code < 33 || it.code > 126 || it in "@?#\\" }) return null
    val rest = text.removePrefix("https://")
    if (rest.isEmpty() || rest.startsWith("/") || rest.startsWith(":")) return null
    return text
}

private fun readLimited(stream: InputStream): String {
    val out = ByteArrayOutputStream()
    val buffer = ByteArray(2048)
    while (true) {
        val n = stream.read(buffer)
        if (n < 0) break
        check(out.size() + n <= MaxAdminResponseBytes) { "Die Antwort des Admin-Dienstes ist zu groß." }
        out.write(buffer, 0, n)
    }
    return out.toString(Charsets.UTF_8.name())
}

/** Blockierender JSON-Aufruf (GET, mit Body POST); nur auf Dispatchers.IO aufrufen. */
internal fun adminJsonCall(url: String, body: String?): JSONObject {
    val connection = URL(url).openConnection() as HttpURLConnection
    try {
        connection.connectTimeout = AdminTimeoutMs
        connection.readTimeout = AdminTimeoutMs
        connection.instanceFollowRedirects = false
        connection.setRequestProperty("Accept", "application/json")
        if (body != null) {
            connection.requestMethod = "POST"
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            connection.outputStream.use { it.write(body.toByteArray(Charsets.UTF_8)) }
        }
        val code = connection.responseCode
        check(code in 200..299) {
            if (code == 429) "Zu viele Anfragen; bitte später erneut versuchen." else "Der Admin-Dienst hat die Anfrage abgelehnt (HTTP $code)."
        }
        val text = connection.inputStream.use(::readLimited)
        return JSONObject(text)
    } finally {
        connection.disconnect()
    }
}
