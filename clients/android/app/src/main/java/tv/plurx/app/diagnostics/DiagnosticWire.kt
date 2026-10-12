package tv.plurx.app.diagnostics

import kotlinx.serialization.json.*
import tv.plurx.app.remote.StrictRemoteJson
import java.time.Instant
import java.util.UUID
import java.math.BigInteger

internal object DiagnosticWire {
    const val VERSION="cinema.diagnostics.v1"
    val reasons=setOf("connection_failed","timeout","unsupported","context_unavailable","telemetry_disabled","storage_denied","storage_full","overflow","peer_unavailable","cancelled","not_requested","interrupted","clock_unknown","revoked","history_disabled","not_delivered","node_limit")
    val names=setOf("summary.txt","description.txt","client-events.jsonl","playback.json","connection.json","server-events.jsonl","server-snapshot.json","diagnostics/platform.json")
    private val json=Json { isLenient=false; ignoreUnknownKeys=false }
    private fun obj(v:JsonElement?, required:Set<String>,optional:Set<String> = emptySet()):JsonObject {
        val o=v as? JsonObject ?: error("object required");require(o.keys.containsAll(required) && (required+optional).containsAll(o.keys));return o
    }
    private fun text(v:JsonElement?):String { val p=v as? JsonPrimitive ?: error("text required");require(p.isString);return p.content }
    private fun one(v:JsonElement?,values:Set<String>) { require(text(v) in values) }
    private fun integer(v:JsonElement?,max:Long):Long {val p=v as? JsonPrimitive ?: error("integer required");require(!p.isString);val n=requireNotNull(p.longOrNull);require(n in 0..max);return n}
    fun decimal(s:String)=Regex("^(0|[1-9][0-9]{0,19})$").matches(s) && BigInteger(s)<=BigInteger("18446744073709551615")
    private fun bounded(v:JsonElement,depth:Int,budget:IntArray) {
        require(budget[0]-- > 0 && depth<=16 && v!=JsonNull)
        when(v) {
            is JsonObject -> v.forEach {(k,x)->require(k.toByteArray().size<=128);bounded(x,depth+1,budget)}
            is JsonArray -> v.forEach {bounded(it,depth+1,budget)}
            is JsonPrimitive -> if(v.isString) {
                val s=v.content;require(s.toByteArray().size<=16384)
                var i=0;while(i<s.length) {val c=s[i++];if(c.isHighSurrogate())require(i<s.length && s[i++].isLowSurrogate());else require(!c.isLowSurrogate())}
            }
        }
    }
    fun manifest(bytes:ByteArray):JsonObject {
        require(bytes.size<=65536)
        val text=Charsets.UTF_8.newDecoder().onMalformedInput(java.nio.charset.CodingErrorAction.REPORT).decode(java.nio.ByteBuffer.wrap(bytes)).toString()
        StrictRemoteJson.check(text);val raw=json.parseToJsonElement(text);bounded(raw,0,intArrayOf(60000))
        val m=obj(raw,setOf("version","report_id","captured_at","category","scope","client","capture","components","privacy","files"))
        require(text(m["version"])==VERSION);val id=text(m["report_id"]);require(UUID.fromString(id).toString()==id)
        val time=text(m["captured_at"]);require(time.length==20 && Instant.parse(time).toString()==time)
        one(m["category"],setOf("playback","connection","interface","other"));one(m["scope"],setOf("anonymous","viewer","administrator"))
        val c=obj(m["client"],setOf("platform","app_version","build","os_version","device_model"));one(c["platform"],setOf("web","browser_tv","ios","ipados","tvos","android","android_tv"))
        for(k in listOf("app_version","build","os_version","device_model"))require(Regex("^[A-Za-z0-9._+-]{1,64}$").matches(text(c[k])))
        val capture=obj(m["capture"],setOf("mode","window_ms","dropped_events","persistence"));one(capture["mode"],setOf("ordinary","reproduction"));integer(capture["window_ms"],900000);require(decimal(text(capture["dropped_events"])));one(capture["persistence"],setOf("available","memory_only","unavailable"))
        val components=m["components"] as? JsonArray ?: error("components");require(components.size in 1..16);val seen=mutableSetOf<String>()
        for(raw in components) {val part=obj(raw,setOf("name","state"),setOf("reason"));one(part["name"],setOf("client","server","workers","connection","platform","clock"));require(seen.add(text(part["name"])));one(part["state"],setOf("complete","partial","unavailable"));if(text(part["state"])=="complete")require("reason" !in part) else one(part["reason"],reasons)}
        require("client" in seen);val privacy=obj(m["privacy"],setOf("titles_included","attachments_included"));privacy.values.forEach {require(it is JsonPrimitive && !it.isString && it.booleanOrNull!=null)}
        val files=m["files"] as? JsonArray ?: error("files");require(files.size in 3..31);seen.clear();var total=0L
        for(raw in files) {val f=obj(raw,setOf("name","size_bytes","sha256"));one(f["name"],names);require(seen.add(text(f["name"])));total+=integer(f["size_bytes"],8388608);require(Regex("^[0-9a-f]{64}$").matches(text(f["sha256"])))}
        require(total<=67108864 && seen.containsAll(setOf("summary.txt","description.txt","client-events.jsonl")) && privacy.getValue("attachments_included").jsonPrimitive.boolean==("diagnostics/platform.json" in seen))
        return m
    }
}
