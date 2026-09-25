import AVFoundation
import XCTest
@testable import plurx

final class PlayerItemObserverTests: XCTestCase {
    func testFatalErrorTakesPrecedenceOverItemAndLog() {
        let fatal = NSError(domain: "fatal", code: 11)
        let item = NSError(domain: "item", code: 22)
        let detail = PlayerItemFailure.classify(
            fatal: fatal,
            item: item,
            log: [.init(uri: "https://example.test/segment", domain: "log", status: 503, comment: nil)],
            failedURI: "https://example.test/segment"
        )
        XCTAssertEqual(detail?.error, fatal)
        XCTAssertNil(detail?.eventDomain)
    }

    func testItemErrorTakesPrecedenceOverUnrelatedLog() {
        let item = NSError(domain: "item", code: 22)
        let detail = PlayerItemFailure.classify(
            fatal: nil,
            item: item,
            log: [.init(uri: "https://example.test/subtitle", domain: "log", status: 404, comment: nil)],
            failedURI: "https://example.test/video"
        )
        XCTAssertEqual(detail?.error, item)
        XCTAssertNil(detail?.eventDomain)
    }

    func testOnlyMatchingLogEntryCanClassifyFailure() {
        let subtitle = PlayerItemFailure.LogEntry(
            uri: "https://example.test/subtitle", domain: "subtitle", status: 404, comment: "benign"
        )
        let video = PlayerItemFailure.LogEntry(
            uri: "https://example.test/video", domain: "network", status: 503, comment: "unavailable"
        )
        XCTAssertNil(PlayerItemFailure.classify(
            fatal: nil, item: nil, log: [subtitle], failedURI: "https://example.test/video"
        ))
        let detail = PlayerItemFailure.classify(
            fatal: nil, item: nil, log: [subtitle, video], failedURI: "https://example.test/video"
        )
        XCTAssertNil(detail?.error)
        XCTAssertEqual(detail?.eventDomain, "network")
        XCTAssertEqual(detail?.eventStatus, 503)
    }

    @MainActor
    func testCancelTwiceFinishesItemStream() async {
        let player = AVPlayer()
        let item = AVPlayerItem(url: URL(string: "https://example.test/video.m3u8")!)
        let observer = AVPlayerItemObserver(item: item, player: player)
        observer.cancel()
        observer.cancel()
        let finished = expectation(description: "item stream finished")
        Task {
            for await _ in observer.events { }
            finished.fulfill()
        }
        await fulfillment(of: [finished], timeout: 2)
    }
}
