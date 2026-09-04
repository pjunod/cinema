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
        var decision: CheckedContinuation<(decision: Decision, caps: DeviceCaps), Error>?
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
                decision = $0
                decisionEntered.fulfill()
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
        try XCTUnwrap(decision).resume(throwing: CancellationError())
        await load.value
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
