package dev.peppy.mobile

import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import okhttp3.Request
import uniffi.peppy_mobile_bindings.NativeAttachmentRetentionProtocolHeader
import uniffi.peppy_mobile_bindings.NativeAttachmentRetentionRequest
import uniffi.peppy_mobile_bindings.attachmentRetentionProtocolHeader

/** One HTTP response. For non-2xx, [body] is the bounded error body (if any). */
class HttpResult(val code: Int, val body: String?) {
    val ok get() = code in 200..299

    /** The server's `{"code":"..."}` error code, if present and well-formed. */
    val errorCode: String?
        get() = if (ok) null else try {
            body?.let { org.json.JSONObject(it).optString("code").takeIf { code -> code.matches(Regex("[a-z_]{1,64}")) } }
        } catch (_: org.json.JSONException) {
            null
        }
}

/** Thrown for transport failures and over-budget responses; the worker retries with backoff. */
class GatewayTransportException(message: String) : IOException(message)

internal enum class GatewayHttpMethod(val code: Long) { GET(1), POST(2), PUT(3), DELETE(4), OTHER(5) }
internal enum class GatewayHttpRoute(val code: Long) { CAPABILITIES(1), COMPACTION_CAPABILITY(2), EVENTS(3), SNAPSHOT(4), SNAPSHOT_RECORDS(5), VAULT(6), ATTACHMENTS(7), OTHER(8) }

/**
 * Authenticated HTTP to the credential's canonical origin. Redirects are disabled so the bearer
 * token can never be forwarded elsewhere; response bodies are bounded like the Rust simulator's.
 */
class GatewayHttp(
    private val origin: String,
    private val bearerToken: String,
    private val observeResponse: ((HttpResult) -> Unit)? = null,
) {
    @Volatile private var retentionHeader: NativeAttachmentRetentionProtocolHeader? = null
    @Volatile private var attachmentProducerReady = true
    fun get(path: String): HttpResult = request("GET", path, null)

    fun postJson(path: String, body: String): HttpResult = request("POST", path, body)

    fun deleteJson(path: String, body: String): HttpResult = request("DELETE", path, body)

    fun retentionEnabled(): Boolean = retentionHeader != null
    fun attachmentProducerReady(): Boolean = attachmentProducerReady
    fun setAttachmentProducerReady(ready: Boolean) { attachmentProducerReady = ready }

    /** Initializes mutation headers from the core negotiated state, not observed request traffic. */
    fun initializeRetentionProtocol(enabled: Boolean) {
        retentionHeader = if (enabled) attachmentRetentionProtocolHeader() else null
    }

    /** Executes a canonical retention request without widening its generated response budget. */
    fun retentionRequest(request: NativeAttachmentRetentionRequest, readLimitBytes: ULong): HttpResult {
        require(request.method in setOf("GET", "POST", "PUT", "DELETE"))
        requireV1JsonPath(request.relativePath)
        require(readLimitBytes > 0uL && readLimitBytes <= MAX_RESPONSE_BYTES.toULong())
        val connection = URL(origin + request.relativePath).openConnection() as HttpURLConnection
        try {
            configureConnection(connection, request.method)
            request.protocolHeader?.let { header -> connection.setRequestProperty(header.name, header.value) }
            request.jsonBody?.let { body ->
                val bytes = body.toByteArray(Charsets.UTF_8)
                connection.doOutput = true
                connection.setFixedLengthStreamingMode(bytes.size)
                connection.setRequestProperty("Content-Type", "application/json")
                connection.outputStream.use { it.write(bytes) }
            }
            val code = connection.responseCode
            val stream = if (code in 200..299) connection.inputStream else connection.errorStream
            val body = stream?.use { readStrictBounded(it, readLimitBytes.toInt()) }
            if (code == 204 && !body.isNullOrEmpty()) throw GatewayTransportException("retention 204 body")
            return HttpResult(code, body)
        } finally {
            connection.disconnect()
        }
    }

    /** The websocket uses the same credential origin and never accepts a caller-supplied URL. */
    fun webSocketRequest(): Request {
        val uri = java.net.URI(origin)
        val scheme = when (uri.scheme) {
            "https" -> "wss"
            "http" -> "ws"
            else -> throw IllegalArgumentException("unsupported canonical origin")
        }
        require(uri.rawPath.isNullOrEmpty() || uri.rawPath == "/")
        require(uri.rawQuery == null && uri.rawFragment == null && uri.userInfo == null)
        return Request.Builder()
            .url("$scheme://${uri.authority}/v1/ws")
            .header("Authorization", "Bearer $bearerToken")
            .header("Accept", "application/json")
            .build()
    }

    fun putFile(path: String, file: File, expectedBytes: Long): HttpResult {
        requireV1ResourcePath(path)
        if (!file.isFile || file.length() != expectedBytes) throw GatewayTransportException("cipher file size changed")
        return binaryConnection("PUT", path, expectedBytes) { connection ->
            connection.doOutput = true
            connection.setFixedLengthStreamingMode(expectedBytes)
            FileInputStream(file).use { input -> connection.outputStream.use { output -> input.copyTo(output, 16 * 1024) } }
            if (file.length() != expectedBytes) throw GatewayTransportException("cipher file size changed")
            0L
        }
    }

    fun getToFile(path: String, destination: File, byteLimit: Long): HttpResult {
        requireV1ResourcePath(path)
        require(byteLimit in 1..MAX_MEDIA_BYTES)
        destination.parentFile?.mkdirs()
        destination.delete()
        return try {
            val result = binaryConnection("GET", path) { connection ->
                if (connection.contentLengthLong > byteLimit) throw GatewayTransportException("media exceeds budget")
                var total = 0L
                connection.inputStream.use { input -> FileOutputStream(destination).use { output ->
                    val buffer = ByteArray(16 * 1024)
                    while (true) {
                        val read = input.read(buffer)
                        if (read < 0) break
                        total += read
                        if (total > byteLimit) throw GatewayTransportException("media exceeds budget")
                        output.write(buffer, 0, read)
                    }
                } }
                total
            }
            if (!result.ok) destination.delete()
            result
        } catch (error: Exception) {
            destination.delete()
            throw error
        }
    }

    private fun request(method: String, path: String, body: String?): HttpResult {
        requireV1JsonPath(path)
        val diagnosticsEnabled = SyncDiagnostics.enabled
        val started = if (diagnosticsEnabled) SyncDiagnostics.nowMs() else 0L
        val requestBytes = if (diagnosticsEnabled) body?.toByteArray(Charsets.UTF_8)?.size?.toLong() ?: 0L else 0L
        val connection = URL(origin + path).openConnection() as HttpURLConnection
        try {
            configureConnection(connection, method)
            applyRetentionHeader(connection, method)
            if (body != null) {
                val bytes = body.toByteArray(Charsets.UTF_8)
                connection.doOutput = true
                connection.setFixedLengthStreamingMode(bytes.size)
                connection.setRequestProperty("Content-Type", "application/json")
                connection.outputStream.use { it.write(bytes) }
            }
            val code = connection.responseCode
            if (code !in 200..299) {
                // Small bounded error body so permanent rejections can be told apart by code.
                val error = try {
                    connection.errorStream?.use { readBoundedWithCount(it, MAX_ERROR_BYTES) }
                } catch (_: IOException) {
                    null
                }
                return observed(HttpResult(code, error?.body), method, path, requestBytes, started, error?.bytes ?: 0L)
            }
            val response = connection.inputStream.use { readBoundedWithCount(it) }
            return observed(HttpResult(code, response.body), method, path, requestBytes, started, response.bytes)
        } catch (error: Exception) {
            if (diagnosticsEnabled) recordRequest(method, path, -1, requestBytes, 0, started, false)
            throw error
        } finally {
            connection.disconnect()
        }
    }

    private fun binaryConnection(method: String, path: String, requestBytes: Long = 0L, transfer: (HttpURLConnection) -> Long): HttpResult {
        val diagnosticsEnabled = SyncDiagnostics.enabled
        val started = if (diagnosticsEnabled) SyncDiagnostics.nowMs() else 0L
        val connection = URL(origin + path).openConnection() as HttpURLConnection
        try {
            configureConnection(connection, method)
            applyRetentionHeader(connection, method)
            if (method == "PUT") {
                connection.setRequestProperty("Content-Type", "application/octet-stream")
                transfer(connection)
                val (result, responseBytes) = result(connection, connection.responseCode)
                return observed(result, method, path, requestBytes, started, responseBytes)
            }
            val code = connection.responseCode
            if (code !in 200..299) {
                val (result, responseBytes) = result(connection, code)
                return observed(result, method, path, requestBytes, started, responseBytes)
            }
            val responseBytes = transfer(connection)
            return observed(HttpResult(code, null), method, path, requestBytes, started, responseBytes)
        } catch (error: GatewayTransportException) {
            if (diagnosticsEnabled) recordRequest(method, path, -1, requestBytes, 0L, started, false)
            throw error
        } catch (error: IOException) {
            // A server can reject before consuming a streaming body; only recover a real rejection.
            val code = try { connection.responseCode } catch (_: IOException) { -1 }
            if (code > 0 && code !in 200..299) {
                val (result, responseBytes) = result(connection, code)
                return observed(result, method, path, requestBytes, started, responseBytes)
            }
            if (diagnosticsEnabled) recordRequest(method, path, -1, requestBytes, 0L, started, false)
            throw GatewayTransportException("transport failure")
        } finally { connection.disconnect() }
    }

    private fun configureConnection(connection: HttpURLConnection, method: String) {
        connection.instanceFollowRedirects = false
        connection.useCaches = false
        connection.requestMethod = method
        connection.connectTimeout = TIMEOUT_MS
        connection.readTimeout = TIMEOUT_MS
        connection.setRequestProperty("Authorization", "Bearer $bearerToken")
        connection.setRequestProperty("Accept", "application/json")
    }

    private fun applyRetentionHeader(connection: HttpURLConnection, method: String) {
        if (method != "GET") retentionHeader?.let { connection.setRequestProperty(it.name, it.value) }
    }

    private fun result(connection: HttpURLConnection, code: Int): Pair<HttpResult, Long> {
        if (code in 200..299) return HttpResult(code, null) to 0L
        val error = try {
            connection.errorStream?.use { readBoundedWithCount(it, MAX_ERROR_BYTES) }
        } catch (_: IOException) {
            null
        }
        return HttpResult(code, error?.body) to (error?.bytes ?: 0L)
    }

    private fun observed(result: HttpResult, method: String? = null, path: String? = null, requestBytes: Long = 0L, started: Long = 0L, responseBytes: Long? = null): HttpResult {
        observeResponse?.invoke(result)
        if (SyncDiagnostics.enabled && method != null && path != null) {
            recordRequest(method, path, result.code, requestBytes, responseBytes ?: 0L, started, true)
        }
        return result
    }

    private fun recordRequest(method: String, path: String, status: Int, requestBytes: Long, responseBytes: Long, started: Long, success: Boolean) {
        if (!SyncDiagnostics.enabled) return
        SyncDiagnostics.record(
            SyncTraceEvent.HTTP_REQUEST,
            SyncTraceField.METHOD to methodCategory(method).code,
            SyncTraceField.ROUTE to routeCategory(path).code,
            SyncTraceField.STATUS to status.toLong(),
            SyncTraceField.REQUEST_BYTES to requestBytes,
            SyncTraceField.RESPONSE_BYTES to responseBytes,
            SyncTraceField.DURATION_MS to (SyncDiagnostics.nowMs() - started),
            SyncTraceField.SUCCESS to if (success) 1 else 0,
        )
    }

    private fun requireV1JsonPath(path: String) {
        require(!path.contains("#"))
        requireV1ResourcePath(path.substringBefore('?'))
    }

    private fun requireV1ResourcePath(path: String) {
        require(path.startsWith("/v1/") && !path.contains("?") && !path.contains("#") && !path.contains("//"))
    }

    companion object {
        const val TIMEOUT_MS = 10_000
        /** Server payload budget (8 MiB) plus framing allowance. */
        const val MAX_RESPONSE_BYTES = 8 * 1024 * 1024 + 64 * 1024
        const val MAX_ERROR_BYTES = 16 * 1024
        const val MAX_MEDIA_BYTES = 33L * 1024 * 1024

        internal fun readBounded(input: InputStream, limit: Int = MAX_RESPONSE_BYTES): String {
            return readBoundedWithCount(input, limit).body
        }

        private fun readStrictBounded(input: InputStream, limit: Int): String {
            val bytes = ByteArrayOutputStream()
            val buffer = ByteArray(4096)
            while (true) {
                val read = input.read(buffer)
                if (read < 0) break
                if (bytes.size() + read > limit) throw GatewayTransportException("response exceeds budget")
                bytes.write(buffer, 0, read)
            }
            return try {
                Charsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(bytes.toByteArray()))
                    .toString()
            } catch (_: java.nio.charset.CharacterCodingException) {
                throw GatewayTransportException("invalid UTF-8 response")
            }
        }

        private fun readBoundedWithCount(input: InputStream, limit: Int = MAX_RESPONSE_BYTES): BoundedBody {
            val out = ByteArrayOutputStream()
            val buffer = ByteArray(16 * 1024)
            while (true) {
                val read = input.read(buffer)
                if (read < 0) break
                if (out.size() + read > limit) throw GatewayTransportException("response exceeds budget")
                out.write(buffer, 0, read)
            }
            return BoundedBody(out.toString(Charsets.UTF_8.name()), out.size().toLong())
        }

        internal fun routeCategory(path: String): GatewayHttpRoute = when (path.substringBefore('?')) {
            "/v1/capabilities" -> GatewayHttpRoute.CAPABILITIES
            "/v1/compaction/capability" -> GatewayHttpRoute.COMPACTION_CAPABILITY
            "/v1/events" -> GatewayHttpRoute.EVENTS
            "/v1/snapshot" -> GatewayHttpRoute.SNAPSHOT
            "/v1/snapshot/records" -> GatewayHttpRoute.SNAPSHOT_RECORDS
            "/v1/vault" -> GatewayHttpRoute.VAULT
            else -> if (path.substringBefore('?').startsWith("/v1/attachments/")) GatewayHttpRoute.ATTACHMENTS else GatewayHttpRoute.OTHER
        }

        private fun methodCategory(method: String): GatewayHttpMethod = when (method) {
            "GET" -> GatewayHttpMethod.GET
            "POST" -> GatewayHttpMethod.POST
            "PUT" -> GatewayHttpMethod.PUT
            "DELETE" -> GatewayHttpMethod.DELETE
            else -> GatewayHttpMethod.OTHER
        }
    }

    private data class BoundedBody(val body: String, val bytes: Long)
}

/** Hosted account transport has the same redirect, timeout, and response-budget boundary as sync. */
internal class HostedHttp(private val bearerToken: String? = null) {
    fun get(path: String) = request("GET", path, null)
    fun postJson(path: String, body: String) = request("POST", path, body)
    fun delete(path: String) = request("DELETE", path, null)

    private fun request(method: String, path: String, body: String?): HttpResult {
        require(path.startsWith("/hosted/v1/") && !path.contains('?') && !path.contains('#') && !path.contains("//"))
        val connection = URL("https://peppy.pro$path").openConnection() as HttpURLConnection
        try {
            connection.instanceFollowRedirects = false; connection.useCaches = false
            connection.requestMethod = method; connection.connectTimeout = GatewayHttp.TIMEOUT_MS; connection.readTimeout = GatewayHttp.TIMEOUT_MS
            connection.setRequestProperty("Accept", "application/json")
            bearerToken?.let { connection.setRequestProperty("Authorization", "Bearer $it") }
            if (body != null) {
                val bytes = body.toByteArray(Charsets.UTF_8)
                connection.doOutput = true; connection.setFixedLengthStreamingMode(bytes.size)
                connection.setRequestProperty("Content-Type", "application/json")
                connection.outputStream.use { it.write(bytes) }
            }
            val code = connection.responseCode
            val stream = if (code in 200..299) connection.inputStream else connection.errorStream
            return HttpResult(code, stream?.use { GatewayHttp.readBounded(it, 256 * 1024) })
        } finally { connection.disconnect() }
    }
}
