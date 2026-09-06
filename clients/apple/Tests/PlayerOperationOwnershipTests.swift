import AVFoundation
import XCTest
@testable import plurx

@MainActor
final class PlayerOperationOwnershipTests: XCTestCase {
    private let caps = DeviceCaps(
        client: ClientInfo(kind: "ios", build: "test", ua: "test"),
        video: [], audio: [], containers: [], transports: [], dvTransport: "none",
        display: DisplayCaps(hdr: false, dolbyVision: false)
    )

    private func start(_ controller: PlayerController, model: AppModel, file: Int = 1) {
        controller.start(model: model, itemId: file, fileId: file, startMs: 0,
                         durationMs: 600_000, title: "Title \(file)")
    }

    func testStartStopStartRestoresOnlyTheNewTitlesPlaybackIntent() {
        let controller = PlayerController()
        let model = AppModel()
        start(controller, model: model)
        controller.stop()
        XCTAssertFalse(controller.wantsPlayback)
        start(controller, model: model, file: 2)
        XCTAssertTrue(controller.wantsPlayback)
        controller.togglePlayPause()
        XCTAssertFalse(controller.wantsPlayback, "only start, not later preparation, grants playback")
        controller.stop()
    }

    func testLateDecisionSuccessCannotPublishIntoANewerTitle() async throws {
        try await checkLateDecision(fails: false)
    }

    func testLateDecisionErrorCannotFailOrClearANewerTitlesIntent() async throws {
        try await checkLateDecision(fails: true)
    }

    func testDeliveryFallbackRecipeSurvivesASupersedingSeekAndPause() {
        for fallback in PlayerController.DeliveryFallback.allCases {
            let controller = PlayerController()
            let before = controller.recipeRevision.desired
            controller.requireDeliveryFallback(fallback)
            let required = controller.recipeRevision.desired
            XCTAssertGreaterThan(required, before)
            controller.requireDeliveryFallback(fallback)
            XCTAssertEqual(controller.recipeRevision.desired, required, "one fallback changes the recipe once")
            controller.seek(toMs: 90_000)
            controller.togglePlayPause()
            XCTAssertEqual(controller.recipeRevision.desired, required)
            XCTAssertTrue(controller.recipeRevision.needsReopen)
            controller.stop()
        }
    }

    func testPreparedSeekRejectsANewerDestinationBeforeMutatingThePlayer() async throws {
        try await checkPreparedSeek(change: "seek")
    }

    func testPreparedSeekRejectsANewerItemBeforeMutatingThePlayer() async throws {
        try await checkPreparedSeek(change: "item")
    }

    func testPreparedSeekIgnoresAStaleReadinessError() async throws {
        try await checkPreparedSeek(change: "error")
    }

    func testPauseDuringResumePreparationRetainsTheResumeDestination() async throws {
        try await checkPreparedSeek(change: "pause")
    }

    private func checkPreparedSeek(change: String) async throws {
        let decisionEntered = expectation(description: "decision suspended")
        var decision: CheckedContinuation<(decision: Decision, caps: DeviceCaps), Error>?
        let readyEntered = expectation(description: "item readiness suspended")
        var readiness: CheckedContinuation<Void, Error>?
        var seeks: [Int] = []
        let itemPreparation = PlayerController.ItemPreparation(ready: { _ in
            try await withCheckedThrowingContinuation {
                readiness = $0
                readyEntered.fulfill()
            }
        }, seek: { _, ms in seeks.append(ms) })
        let controller = PlayerController(requestPlaybackDecision: { _, _, _, _ in
            try await withCheckedThrowingContinuation {
                decision = $0
                decisionEntered.fulfill()
            }
        }, itemPreparation: itemPreparation)
        let model = AppModel()
        start(controller, model: model)
        let load = try XCTUnwrap(controller.loadingTask)
        await fulfillment(of: [decisionEntered], timeout: 3)
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/prepared-seek-owner"))
        controller.player.replaceCurrentItem(with: item)
        let owner = controller.seekPreparationOwner(at: 90_000)
        let operation = Task { try await controller.seekWhenReady(item, ms: 90_000, owner: owner) }
        await fulfillment(of: [readyEntered], timeout: 3)
        switch change {
        case "pause": controller.togglePlayPause()
        case "item": controller.player.replaceCurrentItem(with: AVPlayerItem(url: URL(fileURLWithPath: "/new-owner")))
        default: controller.seek(toMs: 30_000)
        }
        if change == "error" {
            try XCTUnwrap(readiness).resume(throwing: APIError.transport("old item failed"))
        } else {
            try XCTUnwrap(readiness).resume()
        }
        try await operation.value
        XCTAssertEqual(seeks, change == "pause" ? [90_000] : [])
        if change == "pause" { XCTAssertFalse(controller.wantsPlayback) }
        controller.stop()
        try XCTUnwrap(decision).resume(throwing: CancellationError())
        await load.value
    }

    private func checkLateDecision(fails: Bool) async throws {
        let enteredA = expectation(description: "decision A suspended")
        let enteredB = expectation(description: "decision B suspended")
        var requests: [Int: CheckedContinuation<(decision: Decision, caps: DeviceCaps), Error>] = [:]
        let controller = PlayerController(requestPlaybackDecision: { _, file, _, _ in
            try await withCheckedThrowingContinuation { continuation in
                requests[file] = continuation
                (file == 1 ? enteredA : enteredB).fulfill()
            }
        })
        let model = AppModel()
        start(controller, model: model)
        let loadA = try XCTUnwrap(controller.loadingTask)
        await fulfillment(of: [enteredA], timeout: 3)
        controller.stop()
        start(controller, model: model, file: 2)
        let loadB = try XCTUnwrap(controller.loadingTask)
        await fulfillment(of: [enteredB], timeout: 3)
        controller.seek(toMs: 90_000)
        controller.togglePlayPause()
        let before = controller.pendingPlaybackIntentForTesting
        if fails {
            try XCTUnwrap(requests.removeValue(forKey: 1)).resume(throwing: APIError.transport("old title"))
        } else {
            let decision = try JSONDecoder().decode(Decision.self, from: Data(
                #"{"fileId":1,"method":"direct_play","playUrl":"/old-title"}"#.utf8
            ))
            try XCTUnwrap(requests.removeValue(forKey: 1)).resume(returning: (decision, caps))
        }
        await loadA.value
        XCTAssertNil(controller.decision)
        XCTAssertFalse(controller.failed)
        XCTAssertFalse(controller.wantsPlayback)
        XCTAssertEqual(controller.pendingPlaybackIntentForTesting.targetMs, before.targetMs)
        XCTAssertEqual(controller.pendingPlaybackIntentForTesting.generation, before.generation)
        controller.stop()
        try XCTUnwrap(requests.removeValue(forKey: 2)).resume(throwing: CancellationError())
        await loadB.value
    }

    #if os(iOS)
    func testSameItemOpenOrFailoverReconcilesBInsteadOfCommittingDelayedA() async throws {
        let (path, url) = try makeOfflineAudio()
        defer { try? FileManager.default.removeItem(at: url) }
        let preparedA = expectation(description: "old native selection prepared")
        let committedB = expectation(description: "new selection committed")
        var releaseA: CheckedContinuation<Void, Never>?
        var preparedCount = 0
        var commits: [Int?] = []
        var preparation = PlayerController.MediaSelectionPreparation()
        preparation.audio = { _, _ in nil }
        preparation.native = { index, _, _ in
            preparedCount += 1
            if preparedCount == 1 {
                await withCheckedContinuation { continuation in
                    releaseA = continuation
                    preparedA.fulfill()
                }
            }
            return .init(hasSubtitleOptions: true, apply: {
                commits.append(index)
                if commits.count == 1 { committedB.fulfill() }
                return true
            })
        }
        let controller = PlayerController(mediaSelectionPreparation: preparation, canPlayOffline: { _ in true })
        let model = AppModel()
        var offline = offlineItem(path: path)
        offline.subtitleIndex = 0
        controller.startOffline(model: model, item: offline)
        let opening = try XCTUnwrap(controller.loadingTask)
        await fulfillment(of: [preparedA], timeout: 3)
        controller.selectSubtitle(nil)
        await fulfillment(of: [committedB], timeout: 3)
        try XCTUnwrap(releaseA).resume()
        await opening.value
        XCTAssertEqual(commits, [nil, nil], "the delayed selected-track preparation cannot overwrite Off; open reconciles afresh")
        XCTAssertNil(controller.selectedSubtitle)
        controller.stop()
    }
    #endif

    func testPreferredAudioPreparationCannotOverwriteANewerExplicitChoice() async throws {
        let decisionEntered = expectation(description: "decision suspended")
        var decisions: [CheckedContinuation<(decision: Decision, caps: DeviceCaps), Error>] = []
        let preparedAudio = expectation(description: "preferred audio suspended")
        var releaseAudio: CheckedContinuation<Void, Never>?
        var audioCommits = 0
        var preparation = PlayerController.MediaSelectionPreparation()
        preparation.audio = { _, _ in
            await withCheckedContinuation {
                releaseAudio = $0
                preparedAudio.fulfill()
            }
            return { audioCommits += 1 }
        }
        preparation.native = { _, _, _ in .init(hasSubtitleOptions: true, apply: { true }) }
        let controller = PlayerController(requestPlaybackDecision: { _, _, _, _ in
            try await withCheckedThrowingContinuation {
                decisions.append($0)
                if decisions.count == 1 { decisionEntered.fulfill() }
            }
        }, mediaSelectionPreparation: preparation)
        let model = AppModel()
        start(controller, model: model)
        let load = try XCTUnwrap(controller.loadingTask)
        await fulfillment(of: [decisionEntered], timeout: 3)
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/not-loaded-by-this-selection-test"))
        controller.player.replaceCurrentItem(with: item)
        let opening = Task { await controller.reconcileNativeMediaSelections(to: item) }
        await fulfillment(of: [preparedAudio], timeout: 3)
        controller.selectAudio(7)
        try XCTUnwrap(releaseAudio).resume()
        await opening.value
        XCTAssertEqual(audioCommits, 0)
        XCTAssertEqual(controller.selectedAudio, 7)
        controller.stop()
        for decision in decisions { decision.resume(throwing: CancellationError()) }
        await load.value
    }

    private final class Decisions {
        struct Request {
            let file: Int
            let selection: PrePlaySelection
            let quality: PlaybackQuality
            var continuation: CheckedContinuation<(decision: Decision, caps: DeviceCaps), Error>?
        }
        var requests: [Request] = []

        func request(file: Int, selection: PrePlaySelection, quality: PlaybackQuality) async throws
            -> (decision: Decision, caps: DeviceCaps) {
            try await withCheckedThrowingContinuation {
                requests.append(Request(file: file, selection: selection, quality: quality, continuation: $0))
            }
        }

        func resolve(_ index: Int, with result: Result<(decision: Decision, caps: DeviceCaps), Error>) {
            let continuation = requests[index].continuation
            requests[index].continuation = nil
            continuation?.resume(with: result)
        }

        func cancelAll() {
            for index in requests.indices { resolve(index, with: .failure(CancellationError())) }
        }
    }

    private final class DecisionDeadlines {
        var waits: [CheckedContinuation<Void, Error>?] = []
        func wait() async throws {
            try await withCheckedThrowingContinuation { waits.append($0) }
        }
        func fire(_ index: Int) {
            let continuation = waits[index]
            waits[index] = nil
            continuation?.resume()
        }
        func cancelAll() {
            for index in waits.indices {
                let continuation = waits[index]
                waits[index] = nil
                continuation?.resume(throwing: CancellationError())
            }
        }
    }

    private func waitUntil(
        _ message: String, file: StaticString = #filePath, line: UInt = #line,
        _ condition: @MainActor () -> Bool
    ) async throws {
        for _ in 0..<300 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail(message, file: file, line: line)
        throw APIError.transport(message)
    }

    private func coldDecision(file: Int = 1, mode: String = "transcode") throws -> Decision {
        try JSONDecoder().decode(Decision.self, from: Data("""
        {"fileId":\(file),"method":"\(mode)","playUrl":"/not-fetched",
         "source":{"height":1080,"durationMs":600000},
         "audio":[{"index":0,"codec":"aac","default":true},
                  {"index":7,"codec":"aac","default":false}],
         "subtitles":[],"ladder":[]}
        """.utf8))
    }

    func testColdCommandsComposeIntoTheActualSessionRequestAndRejectStaleDefaults() async throws {
        let decisions = Decisions()
        var creates: [(Int, CreateSessionRequest)] = []
        let controller = PlayerController(requestPlaybackDecision: { _, file, selection, quality in
            try await decisions.request(file: file, selection: selection, quality: quality)
        }, requestHlsSession: { _, file, body in
            creates.append((file, body))
            throw APIError.transport("fixture stops before network attachment")
        })
        let model = AppModel()
        let previousQuality = model.playbackQuality
        model.playbackQuality = .auto
        defer { controller.stop(); decisions.cancelAll(); model.playbackQuality = previousQuality }
        controller.start(model: model, itemId: 1, fileId: 1, startMs: 40_000,
                         durationMs: 600_000, title: "Cold commands")
        let originalLoad = try XCTUnwrap(controller.loadingTask)
        try await waitUntil("initial decision") { decisions.requests.count == 1 }
        controller.selectAudio(7)
        try await waitUntil("audio decision") { decisions.requests.count == 2 }
        controller.selectQuality(720)
        try await waitUntil("quality decision") { decisions.requests.count == 3 }
        controller.selectSubtitle(nil)
        try await waitUntil("explicit Off decision") { decisions.requests.count == 4 }
        controller.seek(toMs: 90_000)
        controller.togglePlayPause()
        let finalLoad = try XCTUnwrap(controller.loadingTask)
        let requested = decisions.requests[3]
        XCTAssertEqual(requested.selection, PrePlaySelection(audioIndex: 7, subtitleIndex: -1))
        XCTAssertEqual(requested.quality, .p720)
        XCTAssertEqual(controller.currentMs, 90_000)
        // Complete superseded requests in a different order, including a
        // cancellation-ignoring successful response with old default tracks.
        decisions.resolve(2, with: .failure(APIError.transport("stale quality error")))
        decisions.resolve(0, with: .success((try coldDecision(), caps)))
        decisions.resolve(1, with: .success((try coldDecision(), caps)))
        await originalLoad.value
        XCTAssertNil(controller.decision)
        XCTAssertFalse(controller.failed)
        XCTAssertTrue(creates.isEmpty)
        decisions.resolve(3, with: .success((try coldDecision(), caps)))
        await finalLoad.value
        XCTAssertEqual(creates.count, 1)
        let (file, body) = try XCTUnwrap(creates.first)
        XCTAssertEqual(file, 1)
        XCTAssertEqual(body.height, 720)
        XCTAssertEqual(body.audio, 7)
        XCTAssertEqual(body.start, 90)
        XCTAssertNil(body.subtitle)
        XCTAssertNil(body.subtitleBurn)
        XCTAssertEqual(controller.selectedAudio, 7)
        XCTAssertEqual(controller.selectedHeight, 720)
        XCTAssertNil(controller.selectedSubtitle)
        XCTAssertFalse(controller.wantsPlayback)
        XCTAssertEqual(controller.pendingPlaybackIntentForTesting.targetMs, 90_000)
    }

    func testColdOriginalAndAutoABARejectTheFirstIdenticalQualityResponse() async throws {
        for desired in [PlaybackQuality.original, .auto] {
            let decisions = Decisions()
            var creates: [CreateSessionRequest] = []
            let controller = PlayerController(requestPlaybackDecision: { _, file, selection, quality in
                try await decisions.request(file: file, selection: selection, quality: quality)
            }, requestHlsSession: { _, _, body in
                creates.append(body)
                throw APIError.transport("fixture stops before network attachment")
            })
            let model = AppModel()
            let previousQuality = model.playbackQuality
            model.playbackQuality = desired
            defer { controller.stop(); decisions.cancelAll(); model.playbackQuality = previousQuality }
            start(controller, model: model)
            let first = try XCTUnwrap(controller.loadingTask)
            try await waitUntil("initial ABA decision") { decisions.requests.count == 1 }
            controller.selectQuality(720)
            try await waitUntil("middle ABA decision") { decisions.requests.count == 2 }
            if desired == .original { controller.selectOriginalQuality() }
            else { controller.selectQuality(nil) }
            try await waitUntil("latest ABA decision") { decisions.requests.count == 3 }
            let latest = try XCTUnwrap(controller.loadingTask)
            XCTAssertEqual(decisions.requests[0].quality, desired)
            XCTAssertEqual(decisions.requests[2].quality, desired)
            decisions.resolve(0, with: .success((try coldDecision(mode: "remux"), caps)))
            await first.value
            XCTAssertNil(controller.decision)
            XCTAssertTrue(creates.isEmpty)
            decisions.resolve(2, with: .success((try coldDecision(mode: "remux"), caps)))
            await latest.value
            let body = try XCTUnwrap(creates.first)
            XCTAssertEqual(creates.count, 1)
            XCTAssertNil(body.height)
            XCTAssertEqual(body.copy, true)
            XCTAssertEqual(body.qualityAuto, desired == .auto)
            XCTAssertEqual(controller.selectedQualityIsOriginal, desired == .original)
        }
    }

    func testColdDecisionDeadlineAllowsRetryWithoutLosingSeekPauseOrRecipe() async throws {
        let decisions = Decisions()
        let deadlines = DecisionDeadlines()
        var creates: [CreateSessionRequest] = []
        let controller = PlayerController(requestPlaybackDecision: { _, file, selection, quality in
            try await decisions.request(file: file, selection: selection, quality: quality)
        }, waitInitialDecisionDeadline: { try await deadlines.wait() }, requestHlsSession: { _, _, body in
            creates.append(body)
            throw APIError.transport("fixture stops before network attachment")
        })
        let model = AppModel()
        defer { controller.stop(); decisions.cancelAll(); deadlines.cancelAll() }
        controller.start(model: model, itemId: 1, fileId: 1, startMs: 40_000,
                         durationMs: 600_000, title: "Retry", initialHeight: 720)
        let timedOut = try XCTUnwrap(controller.loadingTask)
        try await waitUntil("decision and deadline") { decisions.requests.count == 1 && deadlines.waits.count == 1 }
        controller.seek(toMs: 90_000)
        controller.togglePlayPause()
        deadlines.fire(0)
        try await waitUntil("decision times out") { controller.failed }
        XCTAssertTrue(controller.canRetryPlaybackFailure)
        XCTAssertNil(controller.decision)
        XCTAssertEqual(controller.currentMs, 90_000)
        XCTAssertFalse(controller.wantsPlayback)
        controller.retryAfterPlaybackFailure()
        let retry = try XCTUnwrap(controller.loadingTask)
        try await waitUntil("retry request and deadline") { decisions.requests.count == 2 && deadlines.waits.count == 2 }
        XCTAssertFalse(controller.failed)
        XCTAssertEqual(decisions.requests[1].quality, .p720)
        decisions.resolve(0, with: .success((try coldDecision(), caps)))
        await timedOut.value
        XCTAssertNil(controller.decision, "an expired owner cannot return after Retry")
        XCTAssertTrue(creates.isEmpty)
        decisions.resolve(1, with: .success((try coldDecision(), caps)))
        await retry.value
        let body = try XCTUnwrap(creates.first)
        XCTAssertEqual(creates.count, 1)
        XCTAssertEqual(body.height, 720)
        XCTAssertEqual(body.start, 90)
        XCTAssertFalse(controller.wantsPlayback)
        XCTAssertEqual(controller.pendingPlaybackIntentForTesting.targetMs, 90_000)
    }

    func testSupersededDecisionDeadlineCannotFailTheNewRecipe() async throws {
        let decisions = Decisions()
        let deadlines = DecisionDeadlines()
        let controller = PlayerController(requestPlaybackDecision: { _, file, selection, quality in
            try await decisions.request(file: file, selection: selection, quality: quality)
        }, waitInitialDecisionDeadline: { try await deadlines.wait() })
        let model = AppModel()
        defer { controller.stop(); decisions.cancelAll(); deadlines.cancelAll() }
        start(controller, model: model)
        try await waitUntil("first deadline") { decisions.requests.count == 1 && deadlines.waits.count == 1 }
        controller.selectAudio(7)
        try await waitUntil("replacement deadline") { decisions.requests.count == 2 && deadlines.waits.count == 2 }
        deadlines.fire(0)
        for _ in 0..<10 { await Task.yield() }
        XCTAssertFalse(controller.failed)
        XCTAssertEqual(controller.selectedAudio, 7)
        deadlines.fire(1)
        try await waitUntil("current deadline fails") { controller.failed }
        controller.stop()
        XCTAssertFalse(controller.canRetryPlaybackFailure)
    }

    func testRetryAfterFirstCreateFailurePreservesPauseAndResumeAcrossRepeatedFailures() async throws {
        var creates: [CreateSessionRequest] = []
        let answer = try coldDecision()
        let controller = PlayerController(requestPlaybackDecision: { [caps] _, _, _, _ in (answer, caps) },
            requestHlsSession: { _, _, body in
                creates.append(body)
                throw APIError.transport("first session unavailable")
            })
        let model = AppModel()
        defer { controller.stop() }
        controller.start(model: model, itemId: 1, fileId: 1, startMs: 40_000,
                         durationMs: 600_000, title: "Cold create retry", initialHeight: 720)
        controller.togglePlayPause()
        await controller.loadingTask?.value
        XCTAssertTrue(controller.failed)
        XCTAssertNotNil(controller.decision)
        XCTAssertNil(controller.player.currentItem)
        XCTAssertEqual(controller.currentMs, 40_000)
        for count in 2...3 {
            controller.retryAfterPlaybackFailure()
            try await waitUntil("retry create completes") { creates.count == count && controller.failed }
            XCTAssertFalse(controller.wantsPlayback)
            XCTAssertEqual(controller.currentMs, 40_000)
            XCTAssertEqual(creates.last?.start, 40)
            XCTAssertEqual(creates.last?.height, 720)
        }
    }

    func testStopStartWhileReadingControlSequenceCannotCreateOldRecipeForNewFile() async throws {
        let decisions = Decisions()
        let controlReads = DecisionDeadlines()
        var reads = 0
        var files: [Int] = []
        let controller = PlayerController(requestPlaybackDecision: { _, file, selection, quality in
            try await decisions.request(file: file, selection: selection, quality: quality)
        }, requestHlsSession: { _, file, _ in
            files.append(file)
            throw APIError.transport("fixture stops before network attachment")
        }, readControlSequence: { _ in
            reads += 1
            if reads == 1 { try? await controlReads.wait() }
            return nil
        })
        let model = AppModel()
        defer { controller.stop(); decisions.cancelAll(); controlReads.cancelAll() }
        start(controller, model: model)
        let old = try XCTUnwrap(controller.loadingTask)
        try await waitUntil("old decision") { decisions.requests.count == 1 }
        decisions.resolve(0, with: .success((try coldDecision(), caps)))
        try await waitUntil("old control read") { controlReads.waits.count == 1 }
        controller.stop()
        start(controller, model: model, file: 2)
        let latest = try XCTUnwrap(controller.loadingTask)
        try await waitUntil("new decision") { decisions.requests.count == 2 }
        controlReads.fire(0)
        await old.value
        XCTAssertTrue(files.isEmpty, "revoked recipe cannot issue a create, even against its original file")
        XCTAssertNil(controller.decision)
        XCTAssertFalse(controller.failed)
        decisions.resolve(1, with: .success((try coldDecision(file: 2), caps)))
        await latest.value
        XCTAssertEqual(files, [2])
    }

    func testNoItemAndOldTitleTimerCallbacksCannotOverwriteColdResumeOrRetry() async throws {
        let decisions = Decisions()
        let deadlines = DecisionDeadlines()
        let controller = PlayerController(requestPlaybackDecision: { _, file, selection, quality in
            try await decisions.request(file: file, selection: selection, quality: quality)
        }, waitInitialDecisionDeadline: { try await deadlines.wait() })
        let model = AppModel()
        defer { controller.stop(); decisions.cancelAll(); deadlines.cancelAll() }
        start(controller, model: model)
        let oldTimer = controller.makePeriodicPlaybackObservation()
        controller.stop()
        controller.start(model: model, itemId: 2, fileId: 2, startMs: 40_000,
                         durationMs: 600_000, title: "Retained resume")
        let timer = controller.makePeriodicPlaybackObservation()
        try await waitUntil("held resume decision") { decisions.requests.count == 1 && deadlines.waits.count == 1 }
        oldTimer()
        timer()
        XCTAssertEqual(controller.currentMs, 40_000)
        controller.selectQuality(720)
        try await waitUntil("held quality decision") { decisions.requests.count == 2 && deadlines.waits.count == 2 }
        timer()
        deadlines.fire(1)
        try await waitUntil("resume decision timeout") { controller.failed }
        XCTAssertEqual(controller.currentMs, 40_000)
        controller.retryAfterPlaybackFailure()
        try await waitUntil("resume Retry decision") { decisions.requests.count == 3 }
        XCTAssertEqual(controller.currentMs, 40_000)
        // The predecessor callback must also be rejected once the new title
        // has an item; a no-item guard alone does not cover that lifecycle.
        controller.player.replaceCurrentItem(with: AVPlayerItem(url: URL(fileURLWithPath: "/not-loaded")))
        oldTimer()
        XCTAssertEqual(controller.currentMs, 40_000)
    }

    #if os(iOS)
    private func makeOfflineAudio() throws -> (String, URL) {
        let path = "tmp/\(UUID().uuidString).wav"
        let url = OfflineCatalog.localURL(for: path)
        let format = try XCTUnwrap(AVAudioFormat(standardFormatWithSampleRate: 8_000, channels: 1))
        let buffer = try XCTUnwrap(AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 80_000))
        buffer.frameLength = buffer.frameCapacity
        buffer.floatChannelData?[0].initialize(repeating: 0, count: Int(buffer.frameLength))
        let file = try AVAudioFile(forWriting: url, settings: format.settings)
        try file.write(from: buffer)
        return (path, url)
    }

    private func offlineItem(path: String) -> OfflineItem {
        OfflineItem(
            id: UUID().uuidString, requestId: "test", serverInstanceId: "test", userId: 1,
            itemId: 1, fileId: 1, packageId: nil, leaseToken: nil, manifestURL: nil,
            title: "Offline", context: nil, durationMs: 10_000, posterFile: nil,
            requestedHeight: 720, actualHeight: 720, audioLabel: nil, subtitleLabel: nil,
            subtitleIndex: nil, state: .downloaded, phase: nil, bytesDownloaded: 0,
            bytesTotal: nil, localAssetRelativePath: path, markers: [], positionMs: 0,
            recordedAt: nil, pendingProgress: false, errorMessage: nil, updatedAt: Date()
        )
    }

    func testOfflineAttachmentHonorsPauseAndSeeksWithoutAnOnlineRecipeReopen() async throws {
        let (path, url) = try makeOfflineAudio()
        defer { try? FileManager.default.removeItem(at: url) }
        var preparation = PlayerController.MediaSelectionPreparation()
        preparation.audio = { _, _ in nil }
        preparation.native = { _, _, _ in .init(hasSubtitleOptions: true, apply: { true }) }
        var offlineLoads = 0
        let controller = PlayerController(mediaSelectionPreparation: preparation, canPlayOffline: { _ in
            offlineLoads += 1
            return true
        })
        let model = AppModel()
        controller.startOffline(model: model, item: offlineItem(path: path))
        controller.togglePlayPause()
        await controller.loadingTask?.value
        XCTAssertFalse(controller.wantsPlayback)
        XCTAssertFalse(controller.isPlaying)
        XCTAssertEqual(controller.player.rate, 0)
        XCTAssertFalse(controller.recipeRevision.needsReopen)
        let originalItem = try XCTUnwrap(controller.player.currentItem)
        controller.seek(toMs: 2_000)
        for _ in 0..<100 {
            if abs(controller.realPositionMs() - 2_000) < 100 { break }
            try await Task.sleep(for: .milliseconds(50))
        }
        XCTAssertTrue(controller.player.currentItem === originalItem)
        XCTAssertEqual(offlineLoads, 1)
        XCTAssertEqual(controller.realPositionMs(), 2_000, accuracy: 100)
        XCTAssertFalse(controller.wantsPlayback)
        controller.stop()
    }

    func testCancelledOfflineStartCannotAttachIntoANewerOnlineTitle() async throws {
        var offlineLoads = 0
        let controller = PlayerController(canPlayOffline: { _ in offlineLoads += 1; return true })
        let model = AppModel()
        controller.startOffline(model: model, item: offlineItem(path: "tmp/not-used.wav"))
        let offlineLoad = try XCTUnwrap(controller.loadingTask)
        controller.stop()
        start(controller, model: model, file: 2)
        controller.stop()
        await offlineLoad.value
        XCTAssertEqual(offlineLoads, 0)
        XCTAssertNil(controller.player.currentItem)
    }
    #endif
}
