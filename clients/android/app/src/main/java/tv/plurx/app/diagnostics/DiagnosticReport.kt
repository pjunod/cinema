package tv.plurx.app.diagnostics

import android.os.Build
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import tv.plurx.app.BuildConfig
import java.io.ByteArrayOutputStream
import java.util.zip.CRC32
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

@Serializable
internal data class DiagnosticComponent(val name: String, val state: String, val reason: String? = null) {
    fun valid() = name in setOf("client","server","workers","connection","platform","clock") && state in setOf("complete","partial","unavailable") && if (state == "complete") reason == null else reason in DiagnosticWire.reasons
}
@Serializable internal data class DiagnosticPrivacy(val titles_included: Boolean, val attachments_included: Boolean = false)
@Serializable internal data class DiagnosticBinding(val report_id: String, val sha256: String, val length: Int, val privacy: DiagnosticPrivacy)
@Serializable internal data class DiagnosticOffer(val id: String, val manual_code: String, val url: String, val expires_in_ms: Long)
internal data class DiagnosticArchive(val bytes: ByteArray, val binding: DiagnosticBinding, val manifest: JsonObject)

internal object DiagnosticReport {
    private fun label(text: String) = text.filter { it.isLetterOrDigit() && it.code < 128 || it in "._+-" }.take(64).ifEmpty { "unknown" }
    fun summary(m: JsonObject): String {
        val c = m.getValue("client").jsonObject; val capture = m.getValue("capture").jsonObject
        val components = m.getValue("components").jsonArray.joinToString("\n") { part -> val p = part.jsonObject; p.getValue("name").jsonPrimitive.content + ": " + p.getValue("state").jsonPrimitive.content + (p["reason"]?.jsonPrimitive?.content?.let { " ($it)" } ?: "") }
        return "Cinema diagnostic report ${m.getValue("report_id").jsonPrimitive.content}\nCaptured: ${m.getValue("captured_at").jsonPrimitive.content}\nCategory: ${m.getValue("category").jsonPrimitive.content}\nClient: ${c.getValue("platform").jsonPrimitive.content} ${c.getValue("app_version").jsonPrimitive.content} build ${c.getValue("build").jsonPrimitive.content}\nCapture: ${capture.getValue("mode").jsonPrimitive.content} (${capture.getValue("window_ms").jsonPrimitive.content} ms); dropped ${capture.getValue("dropped_events").jsonPrimitive.content}; persistence ${capture.getValue("persistence").jsonPrimitive.content}\n$components\nComplete describes collection scope, not retained history.\n"
    }
    fun archive(draft: DiagnosticDraft, television: Boolean): DiagnosticArchive {
        require(draft.category in setOf("playback","connection","interface","other"))
        require(draft.description.codePointCount(0,draft.description.length) <= 4000 && draft.description.toByteArray().size <= 16384 && draft.description.all { it.code >= 32 || it in "\t\r\n" })
        val title = draft.title?.filter { !it.isISOControl() }?.let { String(it.codePoints().limit(256).toArray(), 0, it.codePointCount(0,it.length).coerceAtMost(256)) }
        val platform=if (draft.includePlatform) draft.platform else emptyList()
        val privacy = DiagnosticPrivacy(title != null,platform.isNotEmpty())
        val components = mutableListOf(DiagnosticComponent("client", if ((!draft.capture.historyEnabled && draft.capture.mode != "reproduction") || draft.capture.dropped != "0") "partial" else "complete", if (!draft.capture.historyEnabled && draft.capture.mode != "reproduction") "history_disabled" else if (draft.capture.dropped != "0") "overflow" else null))
        components += draft.components.filter { it.name in setOf("server","workers") && it.valid() }
        if (components.none { it.name == "server" }) components += DiagnosticComponent("server","unavailable","context_unavailable")
        components += listOf(DiagnosticComponent("connection", if (draft.connection.isEmpty()) "unavailable" else "complete", if (draft.connection.isEmpty()) "not_requested" else null), DiagnosticComponent("platform",if (platform.isEmpty()) "unavailable" else "partial",(if (platform.isEmpty() && draft.platform.isNotEmpty()) "not_requested" else draft.platformReason) ?: if (Build.VERSION.SDK_INT >= 30) "not_delivered" else "unsupported"), DiagnosticComponent("clock",if (draft.connection.any { "clock_uncertainty_ms" in it.facts }) "complete" else "unavailable",if (draft.connection.any { "clock_uncertainty_ms" in it.facts }) null else "clock_unknown"))
        var manifest = buildJsonObject {
            put("version", DiagnosticWire.VERSION); put("report_id",draft.id); put("captured_at",draft.capture.capturedAt); put("category",draft.category); put("scope",if (draft.capture.namespace == "anonymous") "anonymous" else "viewer")
            put("client",buildJsonObject { put("platform",if (television) "android_tv" else "android"); put("app_version",label(BuildConfig.VERSION_NAME)); put("build",BuildConfig.VERSION_CODE.toString()); put("os_version",Build.VERSION.SDK_INT.toString()); put("device_model",label(Build.MODEL)) })
            put("capture",buildJsonObject { put("mode",draft.capture.mode); put("window_ms",draft.capture.windowMs.coerceIn(0,900000)); put("dropped_events",draft.capture.dropped); put("persistence",draft.capture.persistence) })
            put("components", DiagnosticJson.encodeToJsonElement(components)); put("privacy",DiagnosticJson.encodeToJsonElement(privacy)); put("files",JsonArray(emptyList()))
        }
        val entries = linkedMapOf("summary.txt" to summary(manifest).toByteArray(), "description.txt" to draft.description.toByteArray(), "client-events.jsonl" to jsonl(draft.capture.events))
        val playback = draft.capture.events.filter { it.domain == "playback" }
        if (title != null) entries["playback.json"] = buildJsonObject { put("events",DiagnosticJson.encodeToJsonElement(playback)); put("title",title) }.toString().toByteArray()
        else if (playback.isNotEmpty()) entries["playback.json"] = DiagnosticJson.encodeToJsonElement(playback).toString().toByteArray()
        val system=draft.serverEvents.filter { "active_producers" in it.facts }
        if (system.isNotEmpty()) entries["server-snapshot.json"]=DiagnosticJson.encodeToJsonElement(system).toString().toByteArray()
        if (draft.serverEvents.isNotEmpty()) entries["server-events.jsonl"] = jsonl(draft.serverEvents)
        if (platform.isNotEmpty()) { require(platform.size <= 16 && platform.all { it.valid() }); entries["diagnostics/platform.json"] = DiagnosticJson.encodeToJsonElement(platform).toString().toByteArray() }
        if (draft.connection.isNotEmpty()) entries["connection.json"] = DiagnosticJson.encodeToJsonElement(draft.connection).toString().toByteArray()
        manifest = JsonObject(manifest + ("files" to JsonArray(entries.map { (name,bytes) -> buildJsonObject { put("name",name); put("size_bytes",bytes.size); put("sha256",DiagnosticFields.hash(bytes)) } })))
        val encoded = manifest.toString().toByteArray(); DiagnosticWire.manifest(encoded); entries["manifest.json"] = encoded
        val bytes = zip(entries); return DiagnosticArchive(bytes,DiagnosticBinding(draft.id,DiagnosticFields.hash(bytes),bytes.size,privacy),manifest)
    }
    fun jsonl(events: List<DiagnosticEvent>): ByteArray {
        require(events.size <= 20000 && events.all { it.valid() })
        val ordered = events.distinctBy { it.run_id + ":" + it.sequence }.sortedWith(compareBy<DiagnosticEvent> { it.run_id }.thenBy { it.sequence.toULong() })
        return ordered.joinToString(separator = "\n", postfix = if (ordered.isEmpty()) "" else "\n") { DiagnosticJson.encodeToString(DiagnosticEvent.serializer(),it) }.toByteArray().also { require(it.size <= 8388608) }
    }
    fun zip(entries: Map<String,ByteArray>): ByteArray {
        require(entries.size <= 32 && entries.keys.all { it in DiagnosticWire.names || it == "manifest.json" } && entries.values.all { it.size <= 8388608 })
        require(entries.values.sumOf { it.size.toLong() } < 16700000)
        val output = ByteArrayOutputStream()
        ZipOutputStream(output).use { zip -> entries.forEach { (name,bytes) ->
            if (Thread.currentThread().isInterrupted) throw kotlinx.coroutines.CancellationException()
            val entry = ZipEntry(name).apply { method = ZipEntry.STORED; size = bytes.size.toLong(); compressedSize = size; crc = CRC32().apply { update(bytes) }.value; time = 315532800000 }
            zip.putNextEntry(entry); zip.write(bytes); zip.closeEntry(); require(output.size() <= 16777216)
        } }
        return output.toByteArray().also { require(it.size <= 16777216) }
    }
}
