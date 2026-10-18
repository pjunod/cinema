import AVKit
import Combine
import XCTest
@testable import plurx

#if os(iOS)
@MainActor
final class PictureInPictureAttachmentTests: XCTestCase {
    func testUnsupportedAttachmentDoesNotRepublishDuringViewUpdates() throws {
        guard !AVPictureInPictureController.isPictureInPictureSupported() else {
            throw XCTSkip("This regression exercises a device without PiP support")
        }
        let owner = PictureInPictureController()
        let layer = AVPlayerLayer(player: AVPlayer())
        var publications = 0
        let observation = owner.objectWillChange.sink { publications += 1 }
        defer { observation.cancel(); owner.detach() }
        owner.attach(to: layer)
        let initial = publications
        for _ in 0..<10 { owner.attach(to: layer) }
        XCTAssertFalse(owner.isSupported)
        XCTAssertEqual(publications, initial,
            "Repeated SwiftUI updates must not keep invalidating an unsupported attachment")
        owner.detach()
        let detached = publications
        owner.attach(to: layer)
        XCTAssertGreaterThan(publications, detached, "An actual reattachment still refreshes support")
    }
}
#endif
