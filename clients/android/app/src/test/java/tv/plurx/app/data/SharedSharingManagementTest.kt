package tv.plurx.app.data

import org.junit.After
import org.junit.Assert.*
import org.junit.Test
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer

class SharedSharingManagementTest {
    private val uuid = "11111111-1111-4111-8111-111111111111"
    private val server = "22222222-2222-4222-8222-222222222222"
    private val epoch = "33333333-3333-4333-8333-333333333333"
    private val precise = 9_007_199_254_740_993L
    private var body = "{}"
    private var status = 200
    private var beforeResponse: (() -> Unit)? = null
    private val requests = mutableListOf<okhttp3.Request>()
    private fun client(): SharedSharingManagementClient {
        Session.origin = "https://b.test"; Session.token = "b-token"
        val transport = OkHttpClient.Builder().addInterceptor { chain ->
            requests += chain.request(); assertEquals("b.test", chain.request().url.host)
            assertEquals("Bearer b-token", chain.request().header("Authorization")); beforeResponse?.invoke()
            Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(status).message("fixture")
                .body(body.toResponseBody()).build()
        }.build()
        return SharedSharingManagementClient.forTest(transport)
    }
    private fun requestJSON(): JsonObject { val buffer = Buffer(); requests.last().body!!.writeTo(buffer); return Json.parseToJsonElement(buffer.readUtf8()).jsonObject }
    private fun exportWire(): String = """{"exports":[{"grant":{"id":"$uuid","recipient_server_id":"$server","state":"pending","scope_generation":1,"credential_generation":1,"catalogue_generation":1,"mutation_generation":$precise,"pending_expires_at_ms":1},"recipient_name":"Recipient B","invitation_id":"$uuid","claim_id":"$epoch","library_ids":["$precise"],"pairing_code":"1234567890abcdef"}],"next":null}"""
    @Test fun actualApprovalAndScopeBodiesPreserveExactIdsAndRequireEnteredCode(): Unit = runBlocking {
        val client = client(); body = exportWire(); val row = client.exports().rows[0]
        assertEquals(precise, row.grant.mutation_generation); assertEquals(listOf(precise.toString()), row.library_ids)
        requests.clear(); assertNotNull(runCatching { client.approve(row, "") }.exceptionOrNull()); assertTrue(requests.isEmpty())
        body = """{"updated":true}"""
        client.approve(row, "1234567890abcdef")
        assertEquals("/api/v1/sharing/exports/$uuid/approve", requests.last().url.encodedPath)
        assertEquals(precise, requestJSON().getValue("expected_mutation_generation").jsonPrimitive.long)
        assertEquals("1234567890abcdef", requestJSON().getValue("pairing_code").jsonPrimitive.content)
        client.scope(row, listOf(precise.toString()))
        val scope = requestJSON(); assertTrue(scope.getValue("library_ids").jsonArray[0].jsonPrimitive.isString)
        assertEquals(precise.toString(), scope.getValue("library_ids").jsonArray[0].jsonPrimitive.content)
        requests.clear(); status = 409; body = """{"code":"sharing_conflict","message":"Refresh generation"}"""
        assertNotNull(runCatching { client.scope(row, emptyList()) }.exceptionOrNull()); assertEquals(1, requests.size)
    }
    @Test fun endpointAndAssignmentReadsFailClosedAndMutationBodyIsBounded(): Unit = runBlocking {
        val client = client()
        val endpoint = SharedSharingEndpoint("100.64.1.2", "fd7a:115c:a1e0::1", "cinema.example.ts.net", 8443, "a".repeat(64))
        endpoint.validate(); assertNotNull(runCatching { endpoint.copy(ipv4 = "127.0.0.1").validate() }.exceptionOrNull())
        val row = SharedSharingImportSummary(uuid, server, epoch, "A", uuid, null, "active", precise, 3, 1, null, listOf(endpoint))
        body = """{"state":"active","import_id":"$uuid","server_id":"$server","catalogue_epoch":"$epoch","lifecycle_generation":3,"expected_assignment_generation":$precise,"assignments":[{"library_id":"$precise","user_ids":[$precise]}]}"""
        val snapshot = client.assignments(row); assertEquals(listOf(precise), snapshot.assignments[0].user_ids)
        body = """{"updated":true}"""
        client.saveAssignments(snapshot, row, snapshot.assignments)
        assertEquals(precise, requestJSON().getValue("expected_assignment_generation").jsonPrimitive.long)
        assertEquals(precise, requestJSON().getValue("assignments").jsonArray[0].jsonObject.getValue("user_ids").jsonArray[0].jsonPrimitive.long)
        requests.clear()
        val oversized = (0 until 64).map { SharedSharingAssignmentGroup(it.toString(), (0 until 256).map { index -> precise + index }) }
        assertNotNull(runCatching { client.saveAssignments(snapshot, row, oversized) }.exceptionOrNull()); assertTrue(requests.isEmpty())
        beforeResponse = { Session.token = "replacement" }
        assertNotNull(runCatching { client.endpoints() }.exceptionOrNull())
    }

    @After fun cleanup() { Session.origin = ""; Session.token = null }
    @Test fun actualInvitationImportRePairRotationAndExplicitDisconnectRoutes(): Unit = runBlocking {
        val client = client(); val token = "cinema-share-v1:Zml4dHVyZQ"
        body = """{"id":"$uuid","invitation":"$token","expires_at_ms":1000}"""
        client.invite(listOf(precise.toString()))
        assertEquals("/api/v1/sharing/invitations", requests.last().url.encodedPath)
        assertEquals(precise.toString(), requestJSON().getValue("library_ids").jsonArray[0].jsonPrimitive.content)
        val wire = """{"import":{"id":"$uuid","source_server_id":"$server","catalogue_epoch":"$epoch","source_name":"Source A","claim_id":"$uuid","remote_grant_id":"44444444-4444-4444-8444-444444444444","state":"active","assignment_generation":2,"lifecycle_generation":4,"endpoint_generation":2,"observed_endpoint_revision":null,"endpoints":[{"ipv4":"100.64.1.2","ipv6":null,"ts_fqdn":"cinema.example.ts.net","port":8443,"spki_sha256":"${"a".repeat(64)}"}]},"pairing_code":"1234567890abcdef"}"""
        body = wire; val imported = client.importSource(token)
        assertEquals(token, requestJSON().getValue("invitation").jsonPrimitive.content)
        client.rePair(imported.`import`, token)
        assertEquals("/api/v1/sharing/imports/$uuid/re-pair", requests.last().url.encodedPath)
        assertEquals(4L, requestJSON().getValue("expected_lifecycle_generation").jsonPrimitive.long)
        client.rotate(imported.`import`); assertTrue(requestJSON().isEmpty())
        body = wire.replace(server, "44444444-4444-4444-8444-444444444444")
        assertNotNull(runCatching { client.rePair(imported.`import`, token) }.exceptionOrNull())
        requests.clear(); body = """{"disabled":true}"""; client.disconnect(uuid)
        assertEquals("DELETE", requests.last().method); assertEquals("/api/v1/sharing/imports/$uuid", requests.last().url.encodedPath)
        body = """{"revoked":true}"""; client.revoke(uuid)
        body = """{"cancelled":true}"""; client.cancelInvitation(uuid)
        assertEquals(3, requests.size)
    }
    @Test fun actualAuthorizationChangesNotifyOutsideLockAndRemovalStopsNotifications() {
        Session.origin = "https://b.test"; Session.token = "b-token"
        val values = mutableListOf<Long>()
        val worker = java.util.concurrent.Executors.newSingleThreadExecutor()
        val failures = java.util.concurrent.atomic.AtomicReference<Throwable?>()
        val observer = Session.observeAuthorizationChanges { generation ->
            // Java monitors are reentrant: a same-thread read would not prove lock release.
            val observed = runCatching {
                worker.submit<Long> { Session.playbackAuthorization().generation }.get(2, java.util.concurrent.TimeUnit.SECONDS)
            }
            observed.exceptionOrNull()?.let { failures.set(it) }
            if (observed.getOrNull() != generation) failures.compareAndSet(null, AssertionError("observer ran while authorization was locked"))
            values += generation
        }
        Session.origin = "https://b.test"; Session.token = "b-token"; assertTrue(values.isEmpty())
        Session.token = "replacement"; assertEquals(listOf(observer.generation + 1), values)
        Session.removeAuthorizationObserver(observer.id)
        Session.origin = "https://other.test"; assertEquals(1, values.size)
        worker.shutdownNow(); assertNull(failures.get())
    }
    @Test fun transientSecretsRetireOnLogoutLeaveAndRegistrationRace() {
        Session.origin = "https://b.test"; Session.token = "b-token"
        val draft = SharedSharingSecretDraft(); draft.edit(invitation = "secret", pairingCode = "1234567890abcdef")
        val revision = draft.snapshot()!!.revision
        Session.token = "b-token"; assertTrue(draft.accepts(revision))
        Session.token = null; assertNull(draft.snapshot()); assertFalse(draft.accepts(revision))
        Session.token = "b-token"; assertNull(draft.snapshot())
        val leaving = SharedSharingSecretDraft(); leaving.edit(invitation = "secret"); leaving.leave(); assertNull(leaving.snapshot())
        val raced = SharedSharingSecretDraft.forTest { Session.token = "replacement" }; assertNull(raced.snapshot())
        draft.leave(); raced.leave()
    }
    @Test fun olderAcknowledgementCannotClearNewerPairingEdit() {
        Session.origin = "https://b.test"; Session.token = "b-token"
        val draft = SharedSharingSecretDraft()
        try {
            draft.edit(invitation = "old-secret"); val old = draft.snapshot()!!
            draft.edit(invitation = "new-secret"); draft.clear(old.revision)
            assertEquals("new-secret", draft.snapshot()?.invitation); assertFalse(draft.accepts(old.revision))
            draft.clear(draft.snapshot()!!.revision); assertEquals("", draft.snapshot()?.invitation)
        } finally { draft.leave() }
    }
}
