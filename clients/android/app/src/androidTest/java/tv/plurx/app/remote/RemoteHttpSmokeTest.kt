package tv.plurx.app.remote

import android.os.SystemClock
import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import tv.plurx.app.data.Session
import java.util.UUID

/** Opt-in isolated synthetic Router seam. Never requires a real account/device. */
@RunWith(AndroidJUnit4::class)
class RemoteHttpSmokeTest {
    @Test fun productionTransportPresencePairingGuardAndEncryptedProofRoundTrip() = runBlocking {
        val encoded = InstrumentationRegistry.getArguments().getString("cinemaRemoteFixture")
        assumeTrue("Synthetic fixture argument required", encoded != null)
        val fixture = RemoteWire.objectBody(Base64.decode(encoded, Base64.NO_WRAP))
        require(fixture["synthetic_only"]?.jsonPrimitive?.boolean == true)
        val saved = Session.playbackAuthorization()
        Session.origin = fixture.string("base_url"); Session.token = fixture.string("tv_token")
        val api = RemoteApi(Session.playbackAuthorization())
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val vault = RemoteSecretStorage(context, "cinema-synthetic-smoke:" + UUID.randomUUID())
        var receiverId: String? = null
        try {
            val installation = api.register("Android instrumented synthetic receiver")
            receiverId = RemoteWire.uuid(installation.string("receiver_id"))
            val receiverSecret = installation.string("receiver_secret")
            vault.saveReceiver(RemoteSecretStorage.Receiver(receiverId, receiverSecret))
            assertTrue(vault.receiver?.secret == receiverSecret)
            val target = RemoteWire.target(api.session(receiverId, UUID.randomUUID().toString(), receiverSecret).getValue("target").jsonObject)
            val pairing = api.pairStart(target, receiverSecret)
            assertEquals(target, RemoteWire.target(pairing.getValue("target").jsonObject))
            assertTrue(Regex("[0-9]{8}").matches(pairing.string("code")))
            val claim = api.pairClaim(target, null, pairing.string("code"), "Android instrumented phone")
            val pending = RemoteWire.uuid(claim.string("pending_id"))
            assertTrue(api.pairApprove(target, pending, true, receiverSecret).getValue("accepted").jsonPrimitive.boolean)
            val result = api.pairResult(target, pending, claim.string("poll_secret"))
            assertEquals("approved", result.string("status"))
            val grant = RemoteSecretStorage.Grant(receiverId, RemoteWire.uuid(result.string("grant_id")), result.string("grant_secret"))
            vault.saveGrant(grant); assertTrue(vault.grants.single().secret == grant.secret)
            val control = api.control(target, grant.id, "acquire", null, grant.secret)
            val lease = control.getValue("control").jsonObject
            assertEquals(setOf("control_epoch", "active_grant_id", "controller_name"), lease.keys)
            val epoch = RemoteWire.uuid(lease.string("control_epoch"))
            val navigation = RemoteNavigationCoordinator()
            navigation.enter("synthetic-playback", "playback") { true }
            val snapshot = navigation.snapshot()
            val guard = RemoteReceiverGuard()
            guard.setContext(RemoteReceiverGuard.Context(grant.id, target, epoch, snapshot.context.revision, snapshot.context.focusRevision, null))
            val credit = guard.mint(RemoteCreditKind.Interaction, SystemClock.elapsedRealtime())!!
            val playback = buildJsonObject {
                put("media", buildJsonObject { put("type", "item"); put("item_id", fixture.number("item_id")) })
                put("title", "Synthetic native playback"); put("playing", true); put("position_ms", 0); put("duration_ms", 1000)
                put("tracks", JsonArray(listOf(buildJsonObject { put("kind", "audio"); put("option_id", "server-audio:1"); put("label", "English") })))
            }
            val state = RemotePresenceState.encode(1, snapshot, setOf("select", "set_playing", "stop", "open_tracks", "choose_track"), guard.currentCredits, playback)
            assertTrue(api.presence(target, state, receiverSecret).getValue("accepted").jsonPrimitive.boolean)
            val first = api.state(target, grant.id, 0, grant.secret, waitMs = 0)
            assertEquals(state, first.getValue("state"))
            val unchanged = api.state(target, grant.id, first.number("response_revision"), grant.secret, waitMs = 0)
            assertEquals(JsonNull, unchanged.getValue("state"))
            val command = RemoteCommand(target, grant.id, epoch, 1, credit.nonce, snapshot.context.revision, snapshot.context.focusRevision, RemoteAction("select", buildJsonObject {}))
            assertTrue(api.send(command, grant.secret).getValue("queued").jsonPrimitive.boolean)
            val poll = api.poll(target, 0, 0, receiverSecret)
            val delivered = RemoteWire.pollCommands(poll).single()
            var applied = 0
            val outcome = guard.apply(delivered, SystemClock.elapsedRealtime()) { applied++; RemoteOutcome.Applied }
            assertEquals(RemoteOutcome.Applied, outcome); assertEquals(1, applied)
            api.ack(target, listOf(RemoteReceiverGuard.Ack(epoch, 1, outcome.wire)), receiverSecret)
            val acknowledged = api.state(target, grant.id, 0, grant.secret, waitMs = 0)
            assertEquals("applied", acknowledged.getValue("outcomes").jsonArray.single().jsonObject.string("outcome"))
            val live = JsonObject(playback + ("media" to buildJsonObject { put("type", "live_channel"); put("channel_id", "synthetic-live") }) + ("duration_ms" to JsonPrimitive(0)))
            assertTrue(api.presence(target, RemotePresenceState.encode(2, snapshot, setOf("set_playing", "stop"), emptyList(), live), receiverSecret).getValue("accepted").jsonPrimitive.boolean)
        } finally {
            receiverId?.let { api.unregister(it) }
            vault.clear(); Session.token = saved.token; Session.origin = saved.origin
        }
    }
}
