import Foundation
import XCTest
import UIKit
import ImageIO
import UniformTypeIdentifiers
@testable import plurx

private final class ArtworkHTTP: URLProtocol {
    static var respond: ((URLRequest) throws -> (String, Data)?)!
    static var stopped: (() -> Void)?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            guard let (mime, data) = try Self.respond(request) else { return }
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: ["Content-Type": mime])!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data); client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() { Self.stopped?() }
}
final class SharedArtworkTests: XCTestCase {
    private let source = SharedLibraryIdentity(importId: "11111111-1111-4111-8111-111111111111", serverId: "22222222-2222-4222-8222-222222222222", catalogueEpoch: "33333333-3333-4333-8333-333333333333", libraryId: "9007199254740993")
    private var reference: SharedPlaybackReference { source.reference("9223372036854775807") }
    private var url: String { "/api/v1/shared/imports/\(source.importId)/art/" + String(repeating: "A", count: 272) }
    override func setUp() { Session.shared.setCredentials(origin: "https://b.test", token: "art-bearer") }
    override func tearDown() { ArtworkHTTP.respond = nil; ArtworkHTTP.stopped = nil; Session.shared.setCredentials(origin: "", token: nil) }
    func testClosedArtworkGrammarAliasesAndSourceContainment() throws {
        let row = SharedArtworkDescriptor(kind: "poster", variant: "w300", url: url)
        try SharedArtworkDescriptor.validate([row], poster: url, backdrop: nil, reference: reference)
        for bad in ["https://a.test" + url, url + "?token=x", url + "#x", url.replacingOccurrences(of: "A", with: "%41"), String(url.dropLast(129)), url.replacingOccurrences(of: source.importId, with: "44444444-4444-4444-8444-444444444444")] {
            XCTAssertThrowsError(try SharedArtworkDescriptor(kind: "poster", variant: "w300", url: bad).validate(reference: reference))
        }
        XCTAssertThrowsError(try SharedArtworkDescriptor.validate([row, row], poster: url, backdrop: nil, reference: reference))
        XCTAssertThrowsError(try SharedArtworkDescriptor.validate([row], poster: nil, backdrop: url, reference: reference))
        XCTAssertThrowsError(try SharedArtworkDescriptor.validate([], poster: url, backdrop: nil, reference: reference))
    }
    func testNonqueuedBudgetFollowsRetainedOwner() throws {
        let budget = SharedArtworkBudget(bytes: 64, operations: 4)
        var leases = try (0..<4).map { _ in try budget.acquire(15) }
        XCTAssertEqual(budget.retainedBytes, 60); XCTAssertThrowsError(try budget.acquire(1))
        leases.removeFirst()
        var retained: SharedArtworkBudget.Lease? = try budget.acquire(19)
        XCTAssertEqual(budget.retainedBytes, 64); XCTAssertThrowsError(try budget.acquire(1))
        withExtendedLifetime(retained) {}; retained = nil; leases.removeAll(); XCTAssertEqual(budget.retainedBytes, 0)
    }
    private func png() -> Data {
        let context = CGContext(data: nil, width: 600, height: 400, bitsPerComponent: 8, bytesPerRow: 2400, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.setFillColor(UIColor.red.cgColor); context.fill(CGRect(x: 0, y: 0, width: 600, height: 400))
        let data = NSMutableData(); let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, context.makeImage()!, nil); XCTAssertTrue(CGImageDestinationFinalize(destination)); return data as Data
    }
    func testActualThumbnailPixelsRemainChargedThroughRetainedCGImage() throws {
        let compressedBefore = SharedArtworkBudget.compressed.retainedBytes, pixelsBefore = SharedArtworkBudget.bitmaps.retainedBytes
        var payload: SharedArtworkBytes? = try SharedArtworkBytes(data: png(), mime: "image/png", admission: SharedArtworkBudget.compressed.acquire(SharedArtworkBudget.assetLimit))
        var image: UIImage? = try sharedArtworkThumbnail(payload!, maximum: 300)
        XCTAssertEqual(image?.cgImage?.width, 300); XCTAssertEqual(image?.cgImage?.height, 200)
        var retained: CGImage? = image?.cgImage
        image = nil; payload = nil
        XCTAssertEqual(SharedArtworkBudget.compressed.retainedBytes, compressedBefore)
        XCTAssertGreaterThan(SharedArtworkBudget.bitmaps.retainedBytes, pixelsBefore)
        withExtendedLifetime(retained) {}; retained = nil
        XCTAssertEqual(SharedArtworkBudget.bitmaps.retainedBytes, pixelsBefore)
        let wrong = try SharedArtworkBytes(data: png(), mime: "image/jpeg", admission: SharedArtworkBudget.compressed.acquire(SharedArtworkBudget.assetLimit))
        XCTAssertThrowsError(try sharedArtworkThumbnail(wrong, maximum: 300)); XCTAssertEqual(SharedArtworkBudget.bitmaps.retainedBytes, pixelsBefore)
    }
    func testAuthenticatedDetailCreatesOnlyBArtworkAndRetiresOldAccount() async throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let ref = try JSONSerialization.jsonObject(with: encoder.encode(reference))
        let detail: [String: Any] = ["item": ["source": "shared", "reference": ref, "title": "Source A", "kind": "movie", "genres": [], "art": [["kind": "poster", "variant": "w300", "url": url]], "poster_url": url], "files": [], "delivery_status": "unavailable"]
        var reads = 0
        ArtworkHTTP.respond = { request in
            XCTAssertEqual(request.url?.host, "b.test"); XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer art-bearer")
            if request.url?.path == self.url { reads += 1; return ("image/png", Data([1, 2, 3])) }
            return ("application/json", try JSONSerialization.data(withJSONObject: detail))
        }
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [ArtworkHTTP.self]
        let item = try await SharedLibraryClient(testTransport: URLSession(configuration: config)).detail(reference).item
        let subject = try XCTUnwrap(item.artworkSubject); let descriptor = try XCTUnwrap(subject.descriptor(backdrop: false))
        let before = SharedArtworkBudget.compressed.retainedBytes
        var bytes: SharedArtworkBytes? = try await subject.read(descriptor)
        XCTAssertEqual(bytes?.length, 3); XCTAssertEqual(bytes?.mime, "image/png"); XCTAssertEqual(reads, 1)
        XCTAssertEqual(SharedArtworkBudget.compressed.retainedBytes, before + SharedArtworkBudget.assetLimit)
        bytes = nil
        for _ in 0..<100 where SharedArtworkBudget.compressed.retainedBytes != before { try await Task.sleep(nanoseconds: 1_000_000) }
        XCTAssertEqual(SharedArtworkBudget.compressed.retainedBytes, before)
        Session.shared.setCredentials(origin: "https://b.test", token: "replacement")
        do { _ = try await subject.read(descriptor); XCTFail("old artwork context accepted") } catch {}
        XCTAssertEqual(reads, 1)
    }

    func testActualInFlightTaskCancellationRetiresURLSessionAndAdmission() async throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let ref = try JSONSerialization.jsonObject(with: encoder.encode(reference))
        let detail: [String: Any] = ["item": ["source": "shared", "reference": ref, "title": "Source A", "kind": "movie", "genres": [], "art": [["kind": "poster", "variant": "w300", "url": url]], "poster_url": url], "files": [], "delivery_status": "unavailable"]
        let started = expectation(description: "actual object request started")
        let stopped = expectation(description: "actual object transport stopped")
        ArtworkHTTP.respond = { request in
            if request.url?.path == self.url { started.fulfill(); return nil }
            return ("application/json", try JSONSerialization.data(withJSONObject: detail))
        }
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [ArtworkHTTP.self]
        let item = try await SharedLibraryClient(testTransport: URLSession(configuration: config)).detail(reference).item
        let subject = try XCTUnwrap(item.artworkSubject); let descriptor = try XCTUnwrap(subject.descriptor(backdrop: false))
        ArtworkHTTP.stopped = { stopped.fulfill() }
        let before = SharedArtworkBudget.compressed.retainedBytes
        let task = Task { try await subject.read(descriptor) }
        await fulfillment(of: [started], timeout: 5)
        task.cancel()
        do { _ = try await task.value; XCTFail("cancelled object published") } catch {}
        await fulfillment(of: [stopped], timeout: 5)
        for _ in 0..<100 where SharedArtworkBudget.compressed.retainedBytes != before { try await Task.sleep(nanoseconds: 1_000_000) }
        XCTAssertEqual(SharedArtworkBudget.compressed.retainedBytes, before)
    }

    func testUntrustedMimeAndAccountChangeDuringObjectResponseRefusePublication() async throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let ref = try JSONSerialization.jsonObject(with: encoder.encode(reference))
        let detail: [String: Any] = ["item": ["source": "shared", "reference": ref, "title": "Source A", "kind": "movie", "genres": [], "art": [["kind": "poster", "variant": "w300", "url": url]], "poster_url": url], "files": [], "delivery_status": "unavailable"]
        var mode = 0
        ArtworkHTTP.respond = { request in
            if request.url?.path == self.url {
                if mode == 1 { Session.shared.setCredentials(origin: "https://b.test", token: "replaced-during-response") }
                return (mode == 0 ? "text/html" : "image/png", Data([1, 2, 3]))
            }
            return ("application/json", try JSONSerialization.data(withJSONObject: detail))
        }
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [ArtworkHTTP.self]
        let item = try await SharedLibraryClient(testTransport: URLSession(configuration: config)).detail(reference).item
        let subject = try XCTUnwrap(item.artworkSubject); let descriptor = try XCTUnwrap(subject.descriptor(backdrop: false))
        let before = SharedArtworkBudget.compressed.retainedBytes
        do { _ = try await subject.read(descriptor); XCTFail("untrusted MIME accepted") } catch {}
        mode = 1
        do { _ = try await subject.read(descriptor); XCTFail("retired response published") } catch {}
        for _ in 0..<100 where SharedArtworkBudget.compressed.retainedBytes != before { try await Task.sleep(nanoseconds: 1_000_000) }
        XCTAssertEqual(SharedArtworkBudget.compressed.retainedBytes, before)
    }

    func testOversizeActualObjectBodyCannotPublishOrRetainAdmission() async throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let ref = try JSONSerialization.jsonObject(with: encoder.encode(reference))
        let detail: [String: Any] = ["item": ["source": "shared", "reference": ref, "title": "Source A", "kind": "movie", "genres": [], "art": [["kind": "poster", "variant": "w300", "url": url]], "poster_url": url], "files": [], "delivery_status": "unavailable"]
        ArtworkHTTP.respond = { request in
            if request.url?.path == self.url { return ("image/png", Data(repeating: 0, count: SharedArtworkBudget.assetLimit + 1)) }
            return ("application/json", try JSONSerialization.data(withJSONObject: detail))
        }
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [ArtworkHTTP.self]
        let item = try await SharedLibraryClient(testTransport: URLSession(configuration: config)).detail(reference).item
        let subject = try XCTUnwrap(item.artworkSubject); let descriptor = try XCTUnwrap(subject.descriptor(backdrop: false))
        let before = SharedArtworkBudget.compressed.retainedBytes
        do { _ = try await subject.read(descriptor); XCTFail("oversize body published") } catch {}
        for _ in 0..<100 where SharedArtworkBudget.compressed.retainedBytes != before { try await Task.sleep(nanoseconds: 1_000_000) }
        XCTAssertEqual(SharedArtworkBudget.compressed.retainedBytes, before)
    }
}
