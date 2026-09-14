package tv.plurx.app.data

import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ChannelSubjectWireTest {
    @Test fun sharedSubjectFixtureDecodesAndClearingEncodesNull() {
        val fixture = Json.parseToJsonElement(checkNotNull(javaClass.classLoader?.getResource("channel-subject-wire.json")).readText()).jsonObject
        for (item in fixture.getValue("cases").jsonArray) {
            val recipe = Net.json.decodeFromJsonElement<LibraryChannelRecipe>(item.jsonObject.getValue("recipe"))
            if (item.jsonObject.getValue("name").jsonPrimitive.content == "set") assertEquals("Stand-up performances", recipe.subject.jsonPrimitive.content)
            val encoded = Net.json.parseToJsonElement(Net.json.encodeToString(recipe.copy(subject = JsonNull))).jsonObject
            assertTrue("clear is explicit with the actual production serializer", encoded.containsKey("subject"))
            assertEquals(JsonNull, encoded["subject"])
        }
    }
}
