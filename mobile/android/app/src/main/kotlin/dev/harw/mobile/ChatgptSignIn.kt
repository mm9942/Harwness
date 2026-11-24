package dev.harw.mobile

import com.google.gson.JsonObject
import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Proxy
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.net.URI
import java.time.Instant
import java.util.concurrent.TimeUnit
import javax.net.ssl.HttpsURLConnection
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext

internal suspend fun signInWithChatgpt(
    hostId: String,
    returningClient: String?,
    expectedSubject: String?,
    openBrowser: (String) -> Unit,
    onVerifying: () -> Unit,
    beforeTokenExchange: suspend (String) -> Unit = {},
): SiwcTokens = withContext(Dispatchers.IO) {
    val (attempt, callback) = ServerSocket().use { server ->
        server.bind(InetSocketAddress(InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1)), 0), 4)
        server.soTimeout = 350
        val attempt = SiwcAttempt(server.localPort, hostId, returningClient)
        withContext(Dispatchers.Main.immediate) { openBrowser(attempt.authorizeUrl) }
        attempt to waitForChatgptCallback(server, attempt)
    }
    if (callback !is SiwcCallback.Code) {
        throw SiwcFailure("ChatGPT sign-in or plan permission was declined.")
    }
    withContext(Dispatchers.Main.immediate) { onVerifying() }
    // The paired caller must durably retain a freshly issued client ID
    // before the code is consumed. Never persist the code, verifier or tokens.
    beforeTokenExchange(callback.clientId)
    currentCoroutineContext().ensureActive()
    val response = openAiJson(
        "/api/accounts/oauth/token",
        siwcForm(listOf(
            "grant_type" to "authorization_code",
            "client_id" to callback.clientId,
            "code" to callback.code,
            "code_verifier" to attempt.verifier,
            "redirect_uri" to attempt.redirectUri,
            "resource" to SiwcResource,
        )).encodeToByteArray(),
    )
    currentCoroutineContext().ensureActive()

    val scope = response.string("scope") ?: throw SiwcFailure("OpenAI did not confirm the granted permissions.")
    val scopes = scope.split(' ').filter { it.isNotEmpty() }.toSet()
    if (!scopes.containsAll(setOf("openid", "offline_access", "resource.invoke", SiwcPlanScope))) {
        throw SiwcFailure("ChatGPT plan permission was not granted.")
    }
    val expiresIn = response.integer("expires_in")?.takeIf { it in 1..86_400 }
        ?: throw SiwcFailure("OpenAI returned an invalid credential expiry.")
    if (!response.string("token_type").equals("Bearer", ignoreCase = true)) {
        throw SiwcFailure("OpenAI returned an unsupported credential type.")
    }
    val now = Instant.now()
    val idToken = response.token("id_token")
    val identity = validateSiwcIdentity(idToken, openAiJson("/.well-known/jwks.json"), callback.clientId, attempt.nonce, now)
    if (expectedSubject != null && !siwcEqual(expectedSubject, identity.subject)) {
        throw SiwcFailure("OpenAI returned a different ChatGPT account than the one registered here.")
    }
    val earliest = response.integer("earliest_refresh_at")
        ?: response.string("earliest_refresh_at")?.let {
            it.toLongOrNull() ?: runCatching { Instant.parse(it).epochSecond }.getOrNull()
        }
    val earliestRefreshAt = if (earliest == null || earliest <= 0L) null else earliest.toULong()
    SiwcTokens(
        callback.clientId, response.token("access_token"), response.token("refresh_token"),
        idToken, scope, (now.epochSecond + expiresIn).toULong(),
        earliestRefreshAt, identity,
    )
}

private suspend fun waitForChatgptCallback(server: ServerSocket, attempt: SiwcAttempt): SiwcCallback {
    val deadline = System.nanoTime() + TimeUnit.MINUTES.toNanos(5)
    while (System.nanoTime() < deadline) {
        currentCoroutineContext().ensureActive()
        val socket = try {
            server.accept()
        } catch (_: SocketTimeoutException) {
            continue
        }
        socket.use {
            val callback = try {
                readChatgptCallback(socket, attempt, server.localPort, deadline)
            } catch (_: Exception) {
                null
            }
            val message = when (callback) {
                is SiwcCallback.Code -> "Return to Harw to finish linking your ChatGPT account."
                SiwcCallback.Denied -> "Sign-in was canceled. You can return to Harw."
                null -> "This request could not complete Harw sign-in."
            }
            runCatching { writeCallbackReply(socket, callback != null, message) }
            if (callback != null) return callback
        }
    }
    throw SiwcFailure("ChatGPT sign-in timed out. Start again from Harw.")
}

private fun readChatgptCallback(socket: Socket, attempt: SiwcAttempt, port: Int, deadline: Long): SiwcCallback {
    socket.soTimeout = 1_500
    val requestDeadline = minOf(deadline, System.nanoTime() + TimeUnit.SECONDS.toNanos(3))
    val input = socket.getInputStream().buffered()
    val request = asciiLine(input, requestDeadline).split(' ')
    require(request.size == 3 && request[0] == "GET" && request[2] == "HTTP/1.1")
    var host: String? = null
    var headerBytes = 0
    while (true) {
        val header = asciiLine(input, requestDeadline)
        if (header.isEmpty()) break
        headerBytes += header.length
        require(headerBytes <= 16_384)
        if (header.substringBefore(':').equals("Host", ignoreCase = true)) {
            require(host == null)
            host = header.substringAfter(':').trim()
        }
    }
    require(host == "127.0.0.1:$port")
    return attempt.callback(request[1])
}

private fun asciiLine(input: InputStream, deadline: Long): String {
    val bytes = ByteArrayOutputStream()
    while (bytes.size() < 8192 && System.nanoTime() < deadline) {
        val next = input.read()
        if (next == '\r'.code) {
            require(input.read() == '\n'.code)
            return bytes.toString(Charsets.US_ASCII.name())
        }
        require(next in 32..126 || next == '\t'.code)
        bytes.write(next)
    }
    throw IllegalArgumentException("Local HTTP line too long.")
}

private fun writeCallbackReply(socket: Socket, accepted: Boolean, message: String) {
    val body = "<!doctype html><meta charset=utf-8><title>Harw</title><p>$message</p>".encodeToByteArray()
    val status = if (accepted) "200 OK" else "400 Bad Request"
    val headers = "HTTP/1.1 $status\r\nContent-Type: text/html; charset=utf-8\r\n" +
        "Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\n" +
        "Connection: close\r\nContent-Length: ${body.size}\r\n\r\n"
    socket.getOutputStream().apply {
        write(headers.encodeToByteArray())
        write(body)
        flush()
    }
}

private fun openAiJson(path: String, form: ByteArray? = null): JsonObject {
    val connection = URI.create("$SiwcIssuer$path").toURL().openConnection(Proxy.NO_PROXY) as HttpsURLConnection
    try {
        connection.instanceFollowRedirects = false
        connection.connectTimeout = 15_000
        connection.readTimeout = 15_000
        connection.setRequestProperty("Accept", "application/json")
        if (form != null) {
            connection.requestMethod = "POST"
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/x-www-form-urlencoded")
            connection.setFixedLengthStreamingMode(form.size)
            connection.outputStream.use { it.write(form) }
        }
        if (connection.responseCode != 200) {
            throw SiwcFailure("OpenAI could not complete the sign-in. Start again from Harw.")
        }
        val bytes = connection.inputStream.use { input ->
            val out = ByteArrayOutputStream()
            val buffer = ByteArray(8192)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                if (out.size() + count > 256 * 1024) throw SiwcFailure("OpenAI returned an oversized response.")
                out.write(buffer, 0, count)
            }
            out.toByteArray()
        }
        return try {
            SiwcJson.fromJson(bytes.decodeToString(throwOnInvalidSequence = true), JsonObject::class.java)
        } finally {
            bytes.fill(0)
        }
    } finally {
        form?.fill(0)
        connection.disconnect()
    }
}

private fun JsonObject.token(name: String): String =
    string(name)?.takeIf { it.length in 1..32_768 && it.all { char -> char in '!'..'~' } }
        ?: throw SiwcFailure("OpenAI returned incomplete or invalid credentials.")
