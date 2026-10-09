package tv.plurx.app.remote

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import java.util.UUID

class RemoteReceiverTest {
    private fun fixture(): JsonObject = javaClass.classLoader!!.getResourceAsStream("remote-control-v1.json")!!.use {
        RemoteWire.json.parseToJsonElement(it.readBytes().toString(Charsets.UTF_8)).jsonObject
    }
    private fun command(row: JsonObject) = RemoteWire.command(row.getValue("command").toString().toByteArray())
    @Test fun tvChallengeUsesOriginalMonotonicDeadlineAndRejectsInvalidDuration() {
        val expiry = RemoteChallengeExpiry(1000, 120000)
        assertEquals(119000L, expiry.remaining(2000))
        assertEquals(0L, expiry.remaining(121000))
        assertEquals(0L, expiry.remaining(999))
        assertTrue(runCatching { RemoteChallengeExpiry(1000, 120001) }.isFailure)
        assertTrue(runCatching { RemoteChallengeExpiry(Long.MAX_VALUE, 1) }.isFailure)
    }
    @Test fun retiredSharedPreparationClearsOnlyItsBusyMarkerAndAutoplayCannotRestartClosedPlayer() {
        val preparation = RemoteOwnedAttempt()
        val old = preparation.begin()
        preparation.retire() // physical replacement retires network publish permission
        assertFalse(preparation.accepts(old))
        preparation.finish(old)
        assertFalse(preparation.busy)
        val newer = preparation.begin()
        preparation.finish(old)
        assertTrue(preparation.busy)
        assertTrue(preparation.accepts(newer))
        preparation.finish(newer)
        val autoplay = RemoteOwnedAttempt()
        val endedPlayer = autoplay.begin(markBusy = false)
        autoplay.retire() // Stop/Close/root change
        assertFalse(autoplay.accepts(endedPlayer))
        val replacement = autoplay.begin(markBusy = false)
        assertFalse(autoplay.accepts(endedPlayer))
        assertTrue(autoplay.accepts(replacement))
    }
    @Test fun retainedDeferredAckSurvivesOwnedTransitionButRejectsOtherGrantAndOriginalDeadline() {
        val original = command(fixture().getValue("valid").jsonArray[1].jsonObject)
        val guard = RemoteReceiverGuard()
        val context = RemoteReceiverGuard.Context(original.grantId, original.target, original.controlEpoch, original.contextRevision, original.focusRevision, null)
        guard.setContext(context)
        val credit = guard.mint(original.action.creditKind, 100)!!
        val offered = original.copy(credit = credit.nonce)
        val binding = RemoteDeferredBinding("preplay", "controller", "full-reference")
        val permit = (guard.reserve(offered, binding, 200) as RemoteReservation.Admitted).permit
        assertEquals("applied", guard.complete(permit, binding, 4000, RemoteOutcome.Applied)?.outcome)
        guard.setContext(context.copy(contextRevision = context.contextRevision + 1))
        assertEquals("applied", guard.retainedResult(offered, 4001)?.outcome)
        assertNull(guard.retainedResult(offered.copy(grantId = UUID.randomUUID().toString()), 4002))
        assertNull(guard.retainedResult(offered, 10200))
    }
    @Test fun canonicalWireAndStrictNestedPollCommands() {
        val fixture = fixture()
        fixture.getValue("valid").jsonArray.forEach { row ->
            val value = command(row.jsonObject)
            assertEquals(row.jsonObject.string("credit_kind"), value.action.creditKind.wire)
            assertEquals(value, RemoteWire.command(value.json().toString().toByteArray()))
        }
        fixture.getValue("invalid").jsonArray.forEach { row -> assertTrue(row.jsonObject.string("name"), runCatching { command(row.jsonObject) }.isFailure) }
        val base = fixture.getValue("valid").jsonArray.first().jsonObject.getValue("command").jsonObject
        val malicious = JsonObject(base + ("action" to buildJsonObject { put("type", "select"); put("playing", true) }))
        val body = buildJsonObject { put("commands", JsonArray(listOf(malicious))) }.toString().toByteArray()
        assertTrue(runCatching { RemoteWire.pollCommands(RemoteWire.objectBody(body)) }.isFailure)
        val valid = base.toString()
        assertTrue(runCatching { RemoteWire.command(valid.replace("\"sequence\":1", "\"sequence\":1.0").toByteArray()) }.isFailure)
        assertTrue(runCatching { RemoteWire.command(valid.replace("\"sequence\":1", "\"sequence\":1,\"sequence\":1").toByteArray()) }.isFailure)
    }
    @Test fun rawNegativeZeroIntegerIsRejectedBeforeDecoding() {
        val base = fixture().getValue("valid").jsonArray.first().jsonObject.getValue("command").jsonObject
        val action = JsonObject(base + ("action" to buildJsonObject { put("type", "seek_absolute"); put("position_ms", 0) }))
        val raw = action.toString().replace("\"position_ms\":0", "\"position_ms\":-0")
        assertTrue(runCatching { RemoteWire.command(raw.toByteArray()) }.isFailure)
    }
    @Test fun canonicalReceiverScenariosUseLocalMonotonicCredits() {
        val fixture = fixture()
        val baseline = command(fixture.getValue("valid").jsonArray[1].jsonObject)
        fixture.getValue("scenarios").jsonArray.forEach { value ->
            val row = value.jsonObject
            val guard = RemoteReceiverGuard()
            var context = RemoteReceiverGuard.Context(baseline.grantId, baseline.target, baseline.controlEpoch, baseline.contextRevision, baseline.focusRevision, null)
            assertNull(guard.setContext(context))
            val issued = row.number("issued_ms")
            val kind = RemoteCreditKind.entries.first { it.wire == row.string("credit_kind") }
            val credit = guard.mint(kind, issued)!!
            val raw = row.getValue("command").jsonObject
            val command = RemoteWire.command(JsonObject(raw + ("credit" to JsonPrimitive(credit.nonce))).toString().toByteArray())
            if (row["preapply"]?.jsonPrimitive?.booleanOrNull == true) assertEquals(RemoteOutcome.Applied, guard.apply(command, issued) { RemoteOutcome.Applied })
            row["new_control_epoch"]?.jsonPrimitive?.contentOrNull?.let { epoch -> context = context.copy(controlEpoch = epoch); assertNull(guard.setContext(context)) }
            assertEquals(row.string("name"), row.string("expected"), guard.apply(command, row.number("now_ms")) { RemoteOutcome.Applied }.wire)
        }
    }
    @Test fun consumesSequenceBeforeEffectAndRetainsRejectedOutcome() {
        val command = command(fixture().getValue("valid").jsonArray[1].jsonObject)
        val guard = RemoteReceiverGuard()
        val context = RemoteReceiverGuard.Context(command.grantId, command.target, command.controlEpoch, command.contextRevision, command.focusRevision, null)
        assertNull(guard.setContext(context))
        val credit = guard.mint(RemoteCreditKind.Interaction, 100)!!
        val applied = command.copy(credit = credit.nonce)
        var activations = 0
        assertEquals(RemoteOutcome.Unsupported, guard.apply(applied, 101) {
            assertNull(guard.result(command.controlEpoch, command.sequence, 101))
            assertEquals(RemoteOutcome.Duplicate, guard.apply(applied, 101) { activations++; RemoteOutcome.Applied })
            activations++; RemoteOutcome.Unsupported
        })
        assertEquals(1, activations)
        assertEquals(RemoteOutcome.Unsupported.wire, guard.result(command.controlEpoch, command.sequence, 101)?.outcome)
        guard.deactivate()
        assertEquals(RemoteOutcome.StaleControl, guard.setContext(context))
    }
    @Test fun boundedCreditsClockRollbackAndPhysicalOwnership() {
        val c = command(fixture().getValue("valid").jsonArray[1].jsonObject)
        val guard = RemoteReceiverGuard()
        guard.setContext(RemoteReceiverGuard.Context(c.grantId, c.target, c.controlEpoch, c.contextRevision, c.focusRevision, null))
        repeat(20) { assertNotNull(guard.mint(RemoteCreditKind.Interaction, it.toLong())) }
        assertEquals(16, guard.currentCredits.size)
        assertNull(guard.mint(RemoteCreditKind.Interaction, 1))
        assertTrue(guard.currentCredits.isEmpty())
        val preview = RemotePreviewOwnership()
        assertEquals(123L, preview.cancel(123L))
        preview.claim(123, 456)
        assertEquals(789L, preview.cancel(789L))
        preview.claim(null, 1000)
        assertNull(preview.cancel(1000))
    }
    @Test fun interactionExpiresBehindLivePlaybackCreditAtExactBoundary() {
        val command = command(fixture().getValue("valid").jsonArray[1].jsonObject)
        val guard = RemoteReceiverGuard()
        guard.setContext(RemoteReceiverGuard.Context(command.grantId, command.target, command.controlEpoch, command.contextRevision, command.focusRevision, null))
        val playback = guard.mint(RemoteCreditKind.Playback, 0)!!
        val interaction = guard.mint(RemoteCreditKind.Interaction, 0)!!
        val expired = command.copy(credit = interaction.nonce)
        assertEquals(RemoteOutcome.Expired, guard.apply(expired, 1000) { fail("expired action applied"); RemoteOutcome.Applied })
        assertEquals(listOf(playback), guard.currentCredits)
        assertEquals(RemoteOutcome.Expired, guard.apply(expired, 1500) { fail("expired action applied"); RemoteOutcome.Applied })
    }
    @Test fun structuredStaleControlAndTargetRejectionsArePreserved() {
        val control = RemoteApi.serverFailure(409, buildJsonObject { put("code", "stale_control"); put("message", "changed") })
        assertEquals(RemoteOutcome.StaleControl, control.outcome)
        assertEquals("stale_control", control.code)
        val target = RemoteApi.serverFailure(409, buildJsonObject { put("code", "stale_target") })
        assertEquals(RemoteOutcome.StaleTarget, target.outcome)
        assertEquals(RemoteOutcome.Unauthorized, RemoteApi.serverFailure(401, JsonObject(emptyMap())).outcome)
        assertEquals(RemoteOutcome.Busy, RemoteApi.serverFailure(429, buildJsonObject { put("code", "x".repeat(81)) }).outcome)
    }
    @Test fun lateTvPairStartAndOldApprovalCannotReopenNewSurface() {
        val lifetime = RemotePairingSurfaceLifetime()
        val first = lifetime.begin(); assertTrue(lifetime.accepts(first))
        lifetime.retire(); assertFalse(lifetime.accepts(first))
        val second = lifetime.begin(); assertTrue(lifetime.accepts(second)); assertFalse(lifetime.accepts(first))
        val approval = lifetime.begin(); val newerChallenge = lifetime.begin()
        assertFalse(lifetime.accepts(approval)); assertTrue(lifetime.accepts(newerChallenge))
    }
    @Test fun unchangedNullStateRetainsCreditsUntilOwnerRetirement() {
        val previous = buildJsonObject { put("context_revision", 5); put("credits", JsonArray(listOf(JsonPrimitive("credit")))) }
        assertSame(previous, RemoteStateUpdate.applying(previous, JsonNull))
        assertNull(RemoteStateUpdate.applying(null, JsonNull))
        val replacement = buildJsonObject { put("context_revision", 6) }
        assertSame(replacement, RemoteStateUpdate.applying(previous, replacement))
    }
    @Test fun unknownInactiveWindowBlocksEffectsWhileLocalPairApprovalRemainsLive() {
        assertTrue(RemoteEffectEligibility.sessionAlive(true))
        assertFalse(RemoteEffectEligibility.semanticEffects(true, false, false))
        assertTrue(RemoteEffectEligibility.semanticEffects(true, false, true))
        assertFalse(RemoteEffectEligibility.semanticEffects(false, true, true))
        assertTrue(RemoteEffectEligibility.semanticEffects(true, true, false))
    }
    @Test fun sceneAndSequenceLifetimeBoundaries() {
        val scene = RemoteSceneEligibility()
        scene.transition(true, false); val first = scene.foregroundId
        scene.transition(false, false); assertFalse(scene.eligible)
        scene.transition(true, false); assertEquals(first, scene.foregroundId)
        scene.transition(false, true); scene.transition(true, false); assertNotEquals(first, scene.foregroundId)
        val target = command(fixture().getValue("valid").jsonArray.first().jsonObject).target
        val grant = UUID.randomUUID().toString(); val epoch = UUID.randomUUID().toString()
        val seq = RemoteControlSequence(); assertNull(seq.next(target, grant, epoch))
        seq.acquired(target, grant, epoch); assertEquals(1L, seq.next(target, grant, epoch))
        seq.acquired(target, grant, epoch); assertEquals(2L, seq.next(target, grant, epoch))
        assertNull(RemoteControlSequence().next(target, grant, epoch))
    }
    @Test fun normalizedPresenceBudgetPreservesIdentitiesAndTransport() {
        val tracks = (0 until 64).map { index -> buildJsonObject {
            put("kind", "audio"); put("option_id", "id:$index:" + "\u0001".repeat(120)); put("label", "\u0002".repeat(256))
        } }
        val state = buildJsonObject {
            put("state_revision", 1); put("context_revision", 1); put("focus_revision", 1); put("route", "playback")
            put("capabilities", JsonArray(listOf("set_playing", "stop", "open_tracks", "choose_track").map(::JsonPrimitive)))
            put("focused_label", JsonNull); put("text_nonce", JsonNull); put("credits", JsonArray(emptyList()))
            put("playback", buildJsonObject { put("media", buildJsonObject { put("type", "item"); put("item_id", 1) }); put("title", "Playing"); put("playing", true); put("position_ms", 1); put("duration_ms", 1000); put("tracks", JsonArray(tracks)) })
        }
        val fitted = RemoteStateBudget.fit(state)
        assertTrue(fitted.toString().toByteArray().size <= RemoteStateBudget.maximumBytes)
        assertEquals("playback", fitted.getValue("route").jsonPrimitive.content)
        assertEquals(listOf("set_playing", "stop"), fitted.getValue("capabilities").jsonArray.map { it.jsonPrimitive.content })
        assertEquals(JsonNull, fitted["playback"])
        val small = JsonObject(state + ("playback" to JsonNull))
        assertSame(small, RemoteStateBudget.fit(small))
    }

    @Test fun latePhonePairApprovalCannotRedirectAfterCloseOrTvSwitch() {
        val target = command(fixture().getValue("valid").jsonArray.first().jsonObject).target
        assertTrue(RemotePairingAdmission.accepts("first", "first", "tv1", "tv1", target, target))
        assertFalse(RemotePairingAdmission.accepts("closed", "first", null, "tv1", null, target))
        assertFalse(RemotePairingAdmission.accepts("second", "first", "tv2", "tv1", target, target))
        assertFalse(RemotePairingAdmission.accepts("first", "first", "tv1", "tv1", target.copy(receiver_epoch = UUID.randomUUID().toString()), target))
    }

    @Test fun dedicatedPhysicalPlayPauseStopRetireCreditsAndPreservePhysicalScrub() {
        listOf(android.view.KeyEvent.KEYCODE_MEDIA_PLAY, android.view.KeyEvent.KEYCODE_MEDIA_PAUSE, android.view.KeyEvent.KEYCODE_MEDIA_STOP).forEach { key ->
            assertTrue(RemotePhysicalInput.retiresCredits(android.view.KeyEvent.ACTION_DOWN, key))
            assertFalse(RemotePhysicalInput.retiresCredits(android.view.KeyEvent.ACTION_UP, key))
            val preview = RemotePreviewOwnership()
            assertEquals(500L, preview.cancel(500L))
            preview.claim(null, 1000L); assertNull(preview.cancel(1000L))
        }
    }

    @Test fun playbackOwnerModalFenceAndOptionChangesInvalidateExactContext() {
        val adapter = RemotePlaybackAdapter()
        var allowed = true; var option = "first"; var position = 0; var applied = 0
        adapter.attach(RemotePlaybackAdapter.Owner("owner", "route", { setOf("set_playing") }, {
            buildJsonObject { put("media", buildJsonObject { put("type", "item"); put("item_id", 1) }); put("position_ms", position); put("tracks", JsonArray(listOf(buildJsonObject { put("option_id", option) }))) }
        }, { applied++; RemoteOutcome.Applied }, {}, { allowed }))
        val original = adapter.contextFingerprint("route")
        position = 10; assertEquals(original, adapter.contextFingerprint("route"))
        option = "replacement"; assertNotEquals(original, adapter.contextFingerprint("route"))
        allowed = false
        assertEquals(RemoteOutcome.Restricted, adapter.dispatch(RemoteAction("set_playing", buildJsonObject { put("playing", false) }), "route"))
        assertEquals(0, applied)
        adapter.detach("owner")
        assertEquals(RemoteOutcome.Unavailable, adapter.dispatch(RemoteAction("stop", buildJsonObject {}), "route"))
    }

    @Test fun unchangedReceiverPollCannotRetireFreshPublishedCredits() {
        val command = command(fixture().getValue("valid").jsonArray.first().jsonObject)
        val guard = RemoteReceiverGuard()
        guard.setContext(RemoteReceiverGuard.Context(command.grantId, command.target, command.controlEpoch, command.contextRevision, command.focusRevision, null))
        val restrictions = RemoteRestrictionLifetime()
        val credit = guard.mint(command.action.creditKind, 0)!!
        if (restrictions.transition(false)) guard.invalidate()
        assertEquals(listOf(credit), guard.currentCredits)
        if (restrictions.transition(true)) guard.invalidate()
        assertTrue(guard.currentCredits.isEmpty())
        assertFalse(restrictions.transition(true))
        assertTrue(restrictions.transition(false))
    }

    @Test fun monotonicPairingDeadlineRejectsSlowClaimAndLateApprovedResult() {
        val deadline = RemotePairingDeadline(1000)
        assertTrue(deadline.admits(1000)); assertTrue(deadline.admits(120999))
        assertFalse(deadline.admits(121000)); assertFalse(deadline.admits(200000))
        assertEquals(1L, deadline.remaining(120999)); assertEquals(0L, deadline.remaining(121000))
        assertFalse(deadline.admits(999))
        assertFalse(RemotePairingDeadline(Long.MAX_VALUE).admits(Long.MAX_VALUE))
        // The same admission runs after claim, after poll and immediately before save.
        var saved = false
        if (deadline.admits(121000)) saved = true
        assertFalse(saved)
    }
    @Test fun exactAckBefore202CannotBecomePendingOrAffectAnotherCommand() {
        val results = RemoteCommandResult()
        results.begin("epoch1", 1)
        assertNull(results.known("epoch1", 1))
        assertTrue(results.observe("epoch1", 1, RemoteOutcome.Applied))
        assertEquals(RemoteOutcome.Applied, results.known("epoch1", 1))
        results.begin("epoch1", 2)
        assertFalse(results.observe("epoch1", 1, RemoteOutcome.Restricted))
        assertNull(results.known("epoch1", 2))
        assertTrue(results.observe("epoch1", 2, RemoteOutcome.Unsupported))
        assertEquals(RemoteOutcome.Unsupported, results.known("epoch1", 2))
        results.reset(); assertNull(results.known("epoch1", 2))
    }

    @Test fun deferredPermitRetainsOriginalDeadlineConsumesSequenceAndBoundsCleanup() {
        val original = command(fixture().getValue("valid").jsonArray[1].jsonObject)
        val guard = RemoteReceiverGuard()
        val context = RemoteReceiverGuard.Context(original.grantId, original.target, original.controlEpoch, original.contextRevision, original.focusRevision, null)
        assertNull(guard.setContext(context))
        val credit = guard.mint(original.action.creditKind, 100)!!
        val command = original.copy(credit = credit.nonce)
        val binding = RemoteDeferredBinding("view", "controller", "full-reference")
        val permit = (guard.reserve(command, binding, 100) as RemoteReservation.Admitted).permit
        assertTrue(guard.permits(permit, binding, 101))
        assertEquals(RemoteOutcome.Duplicate, (guard.reserve(command, binding, 101) as RemoteReservation.Refused).outcome)
        assertFalse(guard.permits(permit, binding.copy(controller = "replacement"), 101))
        guard.retireDeferred()
        assertFalse(guard.permits(permit, binding, 101))
        assertEquals(RemoteOutcome.Unavailable, (guard.reserve(command.copy(sequence = command.sequence + 1), binding, 101) as RemoteReservation.Refused).outcome)
        assertNull(guard.complete(permit, binding, 101, RemoteOutcome.Applied))
        val next = (guard.reserve(command.copy(sequence = command.sequence + 2), binding, 101) as RemoteReservation.Admitted).permit
        assertFalse(guard.permits(next, binding, next.deadline))
        assertEquals(RemoteOutcome.Applied.wire, guard.complete(next, binding, next.deadline, RemoteOutcome.Applied)?.outcome)
        assertNull(guard.result(command.controlEpoch, next.command.sequence, next.resultDeadline))
    }
    @Test fun deferredAckCompletesBeforeOwnerTransitionAndStaleCompletionCannotAffectReplacement() {
        val original = command(fixture().getValue("valid").jsonArray[1].jsonObject)
        val guard = RemoteReceiverGuard()
        val context = RemoteReceiverGuard.Context(original.grantId, original.target, original.controlEpoch, original.contextRevision, original.focusRevision, null)
        guard.setContext(context)
        val command = original.copy(credit = guard.mint(original.action.creditKind, 100)!!.nonce)
        val binding = RemoteDeferredBinding("view", "controller", "full-reference")
        val permit = (guard.reserve(command, binding, 100) as RemoteReservation.Admitted).permit
        assertNotNull(guard.complete(permit, binding, 101, RemoteOutcome.Applied))
        guard.setContext(context.copy(contextRevision = context.contextRevision + 1))
        assertEquals(RemoteOutcome.Applied.wire, guard.result(command.controlEpoch, command.sequence, 102)?.outcome)
        val next = command.copy(sequence = command.sequence + 1, contextRevision = context.contextRevision + 1, credit = guard.mint(original.action.creditKind, 102)!!.nonce)
        val newer = (guard.reserve(next, binding, 102) as RemoteReservation.Admitted).permit
        assertNull(guard.complete(permit, binding, 103, RemoteOutcome.Unavailable))
        assertTrue(guard.permits(newer, binding, 103))
        guard.invalidate(); assertFalse(guard.permits(newer, binding, 103))
    }

    @Test fun deferredTerminalDeadlineDoesNotExtendAndGrantRetirementFencesOldWork() {
        val original = command(fixture().getValue("valid").jsonArray[1].jsonObject)
        val guard = RemoteReceiverGuard()
        val context = RemoteReceiverGuard.Context(original.grantId, original.target, original.controlEpoch, original.contextRevision, original.focusRevision, null)
        guard.setContext(context)
        val command = original.copy(credit = guard.mint(original.action.creditKind, 100)!!.nonce)
        val binding = RemoteDeferredBinding("view", "controller", "full-reference")
        val permit = (guard.reserve(command, binding, 100) as RemoteReservation.Admitted).permit
        assertEquals(10100L, permit.resultDeadline)
        assertNull(guard.complete(permit, binding, 10100, RemoteOutcome.Applied))
        val next = command.copy(sequence = command.sequence + 1, credit = guard.mint(original.action.creditKind, 10100)!!.nonce)
        val pending = (guard.reserve(next, binding, 10100) as RemoteReservation.Admitted).permit
        assertEquals(RemoteOutcome.StaleControl, guard.setContext(context.copy(grantId = UUID.randomUUID().toString())))
        assertFalse(guard.permits(pending, binding, 10101))
        assertNull(guard.complete(pending, binding, 10101, RemoteOutcome.Applied))
    }

    @Test fun transportAdmissionAllowsStaleWireFocusButSubsequentOwnerFocusStillFencesPermit() {
        val original = command(fixture().getValue("valid").jsonArray[1].jsonObject)
        val guard = RemoteReceiverGuard()
        val context = RemoteReceiverGuard.Context(original.grantId, original.target, original.controlEpoch, original.contextRevision, original.focusRevision + 1, null)
        guard.setContext(context)
        val transport = original.copy(action = RemoteAction("set_playing", buildJsonObject { put("playing", true) }), credit = guard.mint(RemoteCreditKind.Playback, 100)!!.nonce)
        val binding = RemoteDeferredBinding("view", "controller", "full-reference")
        val permit = (guard.reserve(transport, binding, 100) as RemoteReservation.Admitted).permit
        assertTrue(guard.permits(permit, binding, 101))
        guard.setContext(context.copy(focusRevision = context.focusRevision + 1))
        assertFalse(guard.permits(permit, binding, 102))
    }

}
