import Foundation
import XCTest
@testable import plurx

final class HLSFragmentCadenceTests: XCTestCase {
    private func words(_ values: [UInt32]) -> Data {
        var data = Data()
        for value in values {
            data.append(contentsOf: [UInt8((value >> 24) & 255), UInt8((value >> 16) & 255), UInt8((value >> 8) & 255), UInt8(value & 255)])
        }
        return data
    }
    private func box(_ type: String, _ body: Data) -> Data { words([UInt32(body.count + 8)]) + Data(type.utf8) + body }
    private func initialization() -> Data {
        func track(_ id: UInt32, _ handler: UInt32, _ scale: UInt32) -> Data {
            let header = box("tkhd", words([0, 0, 0, id]) + Data(repeating: 0, count: 68))
            let media = box("mdhd", words([0, 0, 0, scale, 0, 0])) + box("hdlr", words([0, 0, handler, 0, 0, 0]) + Data([0]))
            return box("trak", header + box("mdia", media))
        }
        return box("moov", track(3, 0x736f756e, 48_000) + track(7, 0x76696465, 24_000)
                   + box("mvex", box("trex", words([0, 7, 1, 1000, 1, 0]))))
    }
    private func fragment(_ durations: [UInt32]?, override: UInt32? = nil) -> Data {
        let header = box("tfhd", words(override.map { [0x20008, 7, $0] } ?? [0x20000, 7]))
        let run = box("trun", words(durations.map { [0x100, UInt32($0.count)] + $0 } ?? [0, 2]))
        // A much shorter audio duration must never determine video tolerance.
        let audio = box("traf", box("tfhd", words([0x20008, 3, 1])) + box("trun", words([0, 2])))
        return box("moof", audio + box("traf", header + box("tfdt", words([0, 0])) + run)) + box("mdat", Data(repeating: 1, count: 8))
    }
    func testFragmentCadenceResolvesRealVideoSampleDurationsAndDefaults() throws {
        let initData = initialization()
        XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: initData, fragment: fragment([1000, 500, 1500]))), 1 / 48.0, accuracy: 1e-12)
        XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: initData, fragment: fragment(nil))), 1 / 24.0, accuracy: 1e-12)
        XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: initData, fragment: fragment(nil, override: 800))), 1 / 30.0, accuracy: 1e-12)
        XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: initData, fragment: fragment([600, 1200], override: 800))), 1 / 40.0, accuracy: 1e-12)
    }
    func testMissingMalformedAndOutOfIntervalCadenceCannotAuthorizeHandoff() {
        let initData = initialization()
        XCTAssertNil(HLSFragmentCadence.minimumDuration(initData: initData, fragment: fragment([0, 1000])))
        XCTAssertNil(HLSFragmentCadence.minimumDuration(initData: initData, fragment: fragment([24001])))
        XCTAssertNil(HLSFragmentCadence.minimumDuration(initData: initData, fragment: fragment([])))
        XCTAssertNil(HLSFragmentCadence.minimumDuration(initData: initData, fragment: Data([0, 0, 0])))
        XCTAssertNil(HLSFragmentCadence.minimumDuration(initData: Data(), fragment: fragment([1000])))
        let valid = fragment([1000, 1000])
        // Every truncation in the sample table must refuse, never trap or use
        // an unresolved default. Cuts inside mdat are also incomplete boxes.
        for count in 0..<valid.count {
            XCTAssertNil(HLSFragmentCadence.minimumDuration(initData: initData, fragment: Data(valid.prefix(count))))
        }
        let scoped = PreparedDecodedCadence(frameDurationSeconds: 1 / 24, itemInterval: 8..<10)
        XCTAssertTrue(scoped.covers(itemPositionMs: 8000))
        XCTAssertTrue(scoped.covers(itemPositionMs: 9999))
        XCTAssertFalse(scoped.covers(itemPositionMs: 7999))
        XCTAssertFalse(scoped.covers(itemPositionMs: 10000))
        XCTAssertFalse(PreparedDecodedCadence(frameDurationSeconds: .nan).covers(itemPositionMs: 8000))
    }
    func testCadenceMediaReferencesCannotEscapeTheirOwnedNamespace() throws {
        let root = try XCTUnwrap(URL(string: "https://lab.example/api/v1/hls/session/master.m3u8?cap=owned"))
        let playlist = try XCTUnwrap(URL(string: "https://lab.example/api/v1/hls/session/video/index.m3u8"))
        XCTAssertEqual(HLSFragmentCadence.mediaChildURL("seg00001.m4s?cap=child", of: playlist, root: root)?.path,
                       "/api/v1/hls/session/video/seg00001.m4s")
        for reference in ["https://other.example/api/v1/hls/session/seg.m4s", "//other.example/seg.m4s",
                          "../../other/seg.m4s", "/api/v1/hls/session-other/seg.m4s",
                          "https://user:password@lab.example/api/v1/hls/session/seg.m4s",
                          "seg.m4s#fragment", "%2e%2e/%2e%2e/other/seg.m4s", "%252e%252e/seg.m4s",
                          "../%2fother/seg.m4s", "file:///api/v1/hls/session/seg.m4s"] {
            XCTAssertNil(HLSFragmentCadence.mediaChildURL(reference, of: playlist, root: root), reference)
        }
    }

    func testCompositionOffsetsCannotWidenDecodedFrameTolerance() throws {
        let header = box("tfhd", words([0x20000, 7])) + box("tfdt", words([0, 0]))
        let run = box("trun", words([0x01000900, 3, 1000, 0,
            1000, UInt32(bitPattern: -999), 1000, 0]))
        let media = box("moof", box("traf", header + run)) + box("mdat", Data([1, 2, 3]))
        XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: initialization(), fragment: media)),
                       1 / 24_000.0, accuracy: 1e-12)
        let duplicate = box("trun", words([0x01000900, 2, 1000, 0, 1000, UInt32(bitPattern: -1000)]))
        XCTAssertNil(HLSFragmentCadence.minimumDuration(initData: initialization(),
            fragment: box("moof", box("traf", header + duplicate)) + box("mdat", Data([1, 2]))))
    }

}
