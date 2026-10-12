package tv.plurx.app.diagnostics
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Test
class DiagnosticWireTest {
    @Test fun sharedManifestFixturesRejectPrivacyAndEncodingViolations() {
        val bytes=javaClass.classLoader!!.getResourceAsStream("diagnostics-v1.json")!!.use {it.readBytes()}
        val f=Json.parseToJsonElement(bytes.toString(Charsets.UTF_8)).jsonObject
        for(c in f.getValue("cases").jsonArray.map {it.jsonObject}) {
            val valid=runCatching {DiagnosticWire.manifest(c.getValue("json").jsonPrimitive.content.toByteArray())}.isSuccess
            assertEquals(c.getValue("name").jsonPrimitive.content,c.getValue("valid").jsonPrimitive.boolean,valid)
        }
    }
}
