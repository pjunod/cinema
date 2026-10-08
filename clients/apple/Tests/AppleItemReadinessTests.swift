import AVFoundation
import XCTest
@testable import plurx

@MainActor
final class AppleItemReadinessTests: XCTestCase {
    private func sample(_ now: Int, published: Bool = false) -> PlayerItemReadiness.Sample {
        var status = PlaybackSessionStatus(id: "growing")
        status.playlistReady = published
        status.producerState = "running"
        status.producedEndMs = 27_527
        status.outTimeMs = 27_527
        status.progressIdleMs = 80
        return .init(status: status, requestedAtMs: now, observedAtMs: now)
    }

    func testGrowingResumeWaitsForPublicationAtTwentyOneSeconds() async throws {
        var now = 0
        var native: AVPlayerItem.Status = .unknown
        let stream = AsyncStream<PlayerItemEvent>.makeStream()
        defer { stream.continuation.finish() }
        var sleeps = 0
        try await PlayerItemReadiness.wait(
            events: stream.stream, status: { native }, failure: { nil },
            context: .init(growingSessionID: "growing", sample: {
                self.sample(now, published: now >= 21_000)
            }), nowMs: { now }, sleep: { ms in
                try Task.checkCancellation()
                now += ms
                sleeps += 1
                if now >= 22_000 {
                    native = .readyToPlay
                    stream.continuation.yield(.status(.readyToPlay))
                }
                await Task.yield()
            }
        )
        XCTAssertEqual(now, 22_000)
        XCTAssertEqual(sleeps, 22)
        XCTAssertEqual(PlayerController.sessionAttachSeekMs(requestedStartMs: 814_473, mediaOriginMs: 813_730), 743)
        XCTAssertNil(PlayerController.sessionAttachSeekMs(requestedStartMs: 822_264, mediaOriginMs: 822_238))
    }

    func testUnknownDirectItemStillTimesOutAtFifteenSeconds() async {
        var now = 0
        let stream = AsyncStream<PlayerItemEvent>.makeStream()
        defer { stream.continuation.finish() }
        do {
            try await PlayerItemReadiness.wait(
                events: stream.stream, status: { .unknown }, failure: { nil },
                context: .init(growingSessionID: nil, sample: { self.sample(now) }), nowMs: { now },
                sleep: { now += $0; await Task.yield() }
            )
            XCTFail("unknown direct item must time out")
        } catch {
            XCTAssertEqual(error as? PlaybackPreparationError, .timedOut)
        }
        XCTAssertEqual(now, 15_000)
    }

    func testOnlyFreshKnownExactSessionProgressExtendsPreparation() {
        let valid = sample(15_000)
        var variants: [PlayerItemReadiness.Sample?] = [nil]
        func altered(_ change: (inout PlaybackSessionStatus) -> Void) -> PlayerItemReadiness.Sample {
            var status = valid.status
            change(&status)
            return .init(status: status, requestedAtMs: 15_000, observedAtMs: 15_000)
        }
        variants += [altered { $0.playlistReady = nil }, altered { $0.outTimeMs = nil; $0.progressIdleMs = 0 },
                     altered { $0.outTimeMs = -1 }, altered { $0.producedEndMs = 0 },
                     altered { $0.producedEndMs = nil }, altered { $0.progressIdleMs = -1 },
                     altered { $0.progressIdleMs = nil }, altered { $0.progressIdleMs = 10_001 },
                     altered { $0.producerState = "held" }, altered { $0.producerState = "failed" }]
        variants.append(.init(status: PlaybackSessionStatus(id: "predecessor"), requestedAtMs: 15_000, observedAtMs: 15_000))
        variants.append(.init(status: valid.status, requestedAtMs: 9_999, observedAtMs: 15_000))
        variants.append(.init(status: valid.status, requestedAtMs: 15_001, observedAtMs: 15_001))
        for evidence in variants {
            var budget = PlayerItemReadiness.Budget(startedAtMs: 0, growingSessionID: "growing")
            XCTAssertGreaterThan(budget.remainingMs(at: 14_999, sample: evidence), 0)
            XCTAssertEqual(budget.remainingMs(at: 15_000, sample: evidence), 0)
        }
        var budget = PlayerItemReadiness.Budget(startedAtMs: 0, growingSessionID: "growing")
        XCTAssertGreaterThan(budget.remainingMs(at: 15_000, sample: valid), 0)
        var delayed = valid.status
        delayed.progressIdleMs = 6_000
        XCTAssertEqual(budget.remainingMs(at: 19_001, sample: .init(
            status: delayed, requestedAtMs: 15_000, observedAtMs: 19_000
        )), 0, "request latency counts against progress freshness")
    }

    func testPublicationObservationHasOneReadinessBudgetAndAnAbsoluteCap() {
        var budget = PlayerItemReadiness.Budget(startedAtMs: 0, growingSessionID: "growing")
        let published = sample(16_000, published: true)
        let delayed = PlayerItemReadiness.Sample(status: published.status, requestedAtMs: 16_000, observedAtMs: 20_900)
        XCTAssertEqual(budget.remainingMs(at: 20_900, sample: delayed), 15_000)
        XCTAssertEqual(budget.remainingMs(at: 30_000, sample: sample(30_000, published: true)), 5_900)
        XCTAssertEqual(budget.remainingMs(at: 35_900, sample: sample(35_900, published: true)), 0)
        XCTAssertEqual(budget.remainingMs(at: 35_900, sample: sample(35_900)), 0, "producer restart cannot renew publication")
        var late = PlayerItemReadiness.Budget(startedAtMs: 0, growingSessionID: "growing")
        XCTAssertGreaterThan(late.remainingMs(at: 58_000, sample: sample(58_000)), 0)
        XCTAssertEqual(late.remainingMs(at: 59_000, sample: sample(59_000, published: true)), 1_000)
        XCTAssertEqual(late.remainingMs(at: 60_000, sample: sample(60_000, published: true)), 0)
    }

    func testHealthyProducerCannotHoldActualWaitPastSixtySeconds() async {
        var now = 0
        let stream = AsyncStream<PlayerItemEvent>.makeStream()
        defer { stream.continuation.finish() }
        do {
            try await PlayerItemReadiness.wait(
                events: stream.stream, status: { .unknown }, failure: { nil },
                context: .init(growingSessionID: "growing", sample: { self.sample(now) }),
                nowMs: { now }, sleep: { now += $0; await Task.yield() }
            )
            XCTFail("healthy production still has an absolute cap")
        } catch { XCTAssertEqual(error as? PlaybackPreparationError, .timedOut) }
        XCTAssertEqual(now, 60_000)
    }

    func testNativeFailureWinsBeforeAfterAndAlongsidePublication() async {
        for failureAt in [10_000, 21_000, 25_000] {
            var now = 0
            var native: AVPlayerItem.Status = .unknown
            let error = NSError(domain: NSURLErrorDomain, code: -1008)
            let stream = AsyncStream<PlayerItemEvent>.makeStream()
            defer { stream.continuation.finish() }
            do {
                try await PlayerItemReadiness.wait(
                    events: stream.stream, status: { native }, failure: { error },
                    context: .init(growingSessionID: "growing", sample: {
                        self.sample(now, published: now >= 21_000)
                    }), nowMs: { now }, sleep: { ms in
                        now += ms
                        if now >= failureAt {
                            // Both events are queued, but the native item is now failed.
                            stream.continuation.yield(.status(.readyToPlay))
                            native = .failed
                            stream.continuation.yield(.status(.failed))
                        }
                        await Task.yield()
                    }
                )
                XCTFail("native failure cannot be extended by server health")
            } catch let result as NSError {
                XCTAssertEqual(result.domain, error.domain)
                XCTAssertEqual(result.code, error.code)
            }
            XCTAssertEqual(now, failureAt)
        }
    }

    func testCancellationAndReplacementRetireTheWait() async {
        for replace in [false, true] {
            let entered = expectation(description: "timer entered")
            let stream = AsyncStream<PlayerItemEvent>.makeStream()
            defer { stream.continuation.finish() }
            var owned = true
            let task = Task {
                try await PlayerItemReadiness.wait(
                    events: stream.stream, status: { .unknown }, failure: { nil },
                    context: .init(growingSessionID: "growing", ownsItem: { owned }),
                    sleep: { _ in
                        entered.fulfill()
                        if replace { owned = false } else { try await Task.sleep(for: .seconds(60)) }
                    }
                )
            }
            await fulfillment(of: [entered], timeout: 3)
            if !replace { task.cancel() }
            do { try await task.value; XCTFail("retired wait must cancel") }
            catch { XCTAssertTrue(error is CancellationError) }
        }
    }

    func testPlaylistReadinessDecodesOptionallyForOlderServers() throws {
        // AppModel uses convertFromSnakeCase through APIClient.
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let current = try decoder.decode(PlaybackSessionStatus.self, from: Data("""
        {"id":"growing","playlist_ready":false,"producer_state":"running","out_time_ms":27527}
        """.utf8))
        XCTAssertEqual(current.playlistReady, false)
        XCTAssertEqual(current.outTimeMs, 27_527)
        let old = try decoder.decode(PlaybackSessionStatus.self, from: Data("{\"id\":\"old\"}".utf8))
        XCTAssertNil(old.playlistReady)
    }
}
