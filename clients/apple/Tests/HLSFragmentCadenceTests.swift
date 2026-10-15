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

    // MARK: - Real muxer output

    private typealias Fixtures = HLSFragmentCadenceFixtures

    /// Split a progressive fragmented MP4 into its init (through `moov`) and
    /// the media that follows, as an HLS packager would.
    private func splitInit(_ file: Data) throws -> (initData: Data, media: Data) {
        var at = 0
        while at + 8 <= file.count {
            let size = file[at..<(at + 4)].reduce(0) { ($0 << 8) | Int($1) }
            let type = String(decoding: file[(at + 4)..<(at + 8)], as: UTF8.self)
            at += size
            if type == "moov" { return (Data(file.prefix(at)), Data(file.suffix(from: at))) }
        }
        throw XCTSkip("no moov")
    }

    func testRealFragmentedSegmentsResolveTheirOwnCadence() throws {
        XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: Fixtures.initMP4, fragment: Fixtures.copySegment)),
                       1 / 24.0, accuracy: 1e-9)
        let vod = try splitInit(Fixtures.fragmentedMP4)
        XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: vod.initData, fragment: vod.media)),
                       1001 / 30_000.0, accuracy: 1e-9)
    }

    /// The walk is identical whatever the chunking, retains only `moof`
    /// boxes, and steps over each `mdat` by naming where the next read starts.
    func testFragmentIndexRetainsOnlyMoofsAndStepsOverMedia() throws {
        let vod = try splitInit(Fixtures.fragmentedMP4)
        for size in [1, 7, 188, 4096] {
            var walk = HLSFragmentCadence.FragmentIndex(total: vod.media.count)
            var at = 0
            while at < vod.media.count {
                let end = min(at + size, vod.media.count)
                try walk.consume(Data(vod.media[at..<end]), at: at)
                at = end
            }
            XCTAssertTrue(walk.finished)
            XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: vod.initData, index: walk)),
                           1001 / 30_000.0, accuracy: 1e-9, "chunk \(size)")
        }
        // Hop: read only up to each mdat header, then resume past its payload.
        var walk = HLSFragmentCadence.FragmentIndex(total: vod.media.count)
        var offset = 0, reads = 0, read = 0
        while !walk.finished {
            let end = min(offset + 64, vod.media.count)
            try walk.consume(Data(vod.media[offset..<end]), at: offset)
            reads += 1; read += end - offset
            XCTAssertGreaterThan(walk.resumeOffset, offset)
            offset = walk.resumeOffset
            XCTAssertLessThan(reads, 1_000)
        }
        XCTAssertNil(walk.index.range(of: Data("mdat".utf8)), "the media payload is never retained")
        XCTAssertLessThan(read, vod.media.count, "stepping over mdat skips its payload")
        XCTAssertEqual(try XCTUnwrap(HLSFragmentCadence.minimumDuration(initData: vod.initData, index: walk)),
                       1001 / 30_000.0, accuracy: 1e-9)
        // A resume anywhere but the announced offset is refused.
        var strict = HLSFragmentCadence.FragmentIndex(total: vod.media.count)
        try strict.consume(Data(vod.media.prefix(64)), at: 0)
        XCTAssertThrowsError(try strict.consume(Data(vod.media[100..<120]), at: 100))
    }

    private func transportCadence(_ data: Data, chunk: Int = 4096) -> Double? {
        var stream = TransportStreamCadence()
        var at = 0
        while at < data.count {
            let end = min(at + chunk, data.count)
            guard (try? stream.consume(Data(data[at..<end]))) != nil else { return nil }
            at = end
        }
        return stream.minimumDuration()
    }

    func testTransportStreamSegmentsResolveVideoPESCadence() throws {
        for chunk in [1, 100, 188, 4096, 1 << 20] {
            XCTAssertEqual(try XCTUnwrap(transportCadence(Fixtures.avcTransportSegment, chunk: chunk)), 1 / 24.0, accuracy: 1e-9)
            XCTAssertEqual(try XCTUnwrap(transportCadence(Fixtures.hevcTransportSegment, chunk: chunk)), 1 / 25.0, accuracy: 1e-9)
        }
        // Truncation, a lost video packet and a PES carrying two frames all
        // refuse rather than widen the tolerance.
        XCTAssertNil(transportCadence(Fixtures.avcTransportSegment.prefix(Fixtures.avcTransportSegment.count - 100)))
        var packets = [Data]()
        for at in stride(from: 0, to: Fixtures.avcTransportSegment.count, by: 188) {
            packets.append(Data(Fixtures.avcTransportSegment[at..<(at + 188)]))
        }
        let videoStarts = packets.indices.filter { packets[$0][1] & 0x40 != 0 && packets[$0][1] & 0x1F == 0x01 && packets[$0][2] == 0x00 }
        XCTAssertGreaterThan(videoStarts.count, 3)
        var merged = packets
        merged[videoStarts[2]][1] &= ~0x40
        XCTAssertNil(transportCadence(merged.reduce(Data(), +)), "two access units in one PES cannot prove one frame")
        var lost = packets
        lost.remove(at: videoStarts[2])
        XCTAssertNil(transportCadence(lost.reduce(Data(), +)))
        var notSync = Fixtures.avcTransportSegment
        notSync[188] = 0x48
        XCTAssertNil(transportCadence(notSync))
    }

    // MARK: - The network path

    /// The real copy segment's `moof`, then one `mdat` whose payload is
    /// `payload` zero bytes, served lazily.
    private func largeCopySegment(payload: Int, ranges: Bool) throws -> CadenceStub.Resource {
        let real = Fixtures.copySegment
        var at = 0, mdat = 0
        while at + 8 <= real.count {
            let size = real[at..<(at + 4)].reduce(0) { ($0 << 8) | Int($1) }
            if String(decoding: real[(at + 4)..<(at + 8)], as: UTF8.self) == "mdat" { mdat = at; break }
            at += size
        }
        XCTAssertGreaterThan(mdat, 0)
        var built = Data(real.prefix(mdat))
        built.append(contentsOf: [UInt8((payload + 8) >> 24), UInt8(((payload + 8) >> 16) & 255), UInt8(((payload + 8) >> 8) & 255), UInt8((payload + 8) & 255)])
        built.append(contentsOf: Array("mdat".utf8))
        let head = built
        return CadenceStub.Resource(length: mdat + 8 + payload, ranges: ranges) { range in
            var out = Data()
            if range.lowerBound < head.count { out.append(head[range.lowerBound..<min(range.upperBound, head.count)]) }
            out.append(Data(count: range.count - out.count))
            return out
        }
    }

    private func inspectCopy(_ segment: CadenceStub.Resource) async throws -> (host: String, cadence: PreparedDecodedCadence?) {
        let host = CadenceStub.host()
        CadenceStub.install(host, [
            "/hls/s1/index.m3u8": .text(Self.playlist(map: true)),
            "/hls/s1/init.mp4": .bytes(Fixtures.initMP4),
            "/hls/s1/seg0.m4s": segment,
        ])
        let cadence = await HLSFragmentCadence.inspect(
            url: try XCTUnwrap(URL(string: "http://\(host)/hls/s1/index.m3u8")), itemSeconds: 1, boundMs: 4_000,
            configuration: CadenceStub.configuration())
        return (host, cadence)
    }

    private func refusedFragmentHost(status: Int) -> String {
        let host = CadenceStub.host()
        let data = Fixtures.copySegment
        CadenceStub.install(host, [
            "/hls/s1/index.m3u8": .text(Self.playlist(map: true)),
            "/hls/s1/init.mp4": .bytes(Fixtures.initMP4),
            "/hls/s1/seg0.m4s": .init(length: data.count, ranges: true, statuses: [status, 200]) { Data(data[$0]) },
        ])
        return host
    }

    func testRefusedFragmentsAreTerminalAndCancelBodyTransfer() async throws {
        for status in [503, 401, 403, 410] {
            let host = refusedFragmentHost(status: status)
            let cadence = await HLSFragmentCadence.inspect(
                url: try XCTUnwrap(URL(string: "http://\(host)/hls/s1/index.m3u8")), itemSeconds: 1, boundMs: 2_000,
                configuration: CadenceStub.configuration())
            XCTAssertNil(cadence)
            XCTAssertEqual(CadenceStub.requestedRanges(host, "/hls/s1/seg0.m4s").count, 1,
                           "a refused response must not be hidden by retrying")
            let deadline = ProcessInfo.processInfo.systemUptime + 0.5
            while CadenceStub.cancelled(host, "/hls/s1/seg0.m4s") == 0,
                  ProcessInfo.processInfo.systemUptime < deadline {
                try await Task.sleep(nanoseconds: 5_000_000)
            }
            XCTAssertGreaterThanOrEqual(CadenceStub.cancelled(host, "/hls/s1/seg0.m4s"), 1)
            XCTAssertEqual(CadenceStub.served(host, "/hls/s1/seg0.m4s"), 0)
        }
    }

    func testLargeCopySegmentIsReadThroughItsIndexNotItsPayload() async throws {
        // A 21 Mb/s copy segment of six seconds is ~16 MB; this one is 40 MB.
        let inspected = try await inspectCopy(try largeCopySegment(payload: 40_000_000, ranges: true))
        let cadence = try XCTUnwrap(inspected.cadence)
        XCTAssertEqual(cadence.frameDurationSeconds, 1 / 24.0, accuracy: 1e-9)
        XCTAssertEqual(cadence.itemInterval, 0..<6)
        XCTAssertLessThanOrEqual(CadenceStub.served(inspected.host, "/hls/s1/seg0.m4s"), HLSFragmentCadence.indexWindowBytes)
    }

    func testTinyRangedCadenceIndexDoesNotFetchAQuarterMegabyte() async throws {
        let inspected = try await inspectCopy(try largeCopySegment(payload: 40_000_000, ranges: true))
        XCTAssertEqual(try XCTUnwrap(inspected.cadence).frameDurationSeconds, 1 / 24.0, accuracy: 1e-9)
        XCTAssertEqual(CadenceStub.requestedRanges(inspected.host, "/hls/s1/seg0.m4s"), ["bytes=0-4095"])
        XCTAssertLessThanOrEqual(CadenceStub.served(inspected.host, "/hls/s1/seg0.m4s"), 4_096)
    }

    func testLargeAndMultipleMoofsContinueAdaptivelyWithoutReadingMedia() async throws {
        func indexedMoof(padding: Int, decodeTime: UInt32) -> Data {
            let track = box("tfhd", words([0x20000, 7]))
                + box("tfdt", words([0, decodeTime]))
                + box("trun", words([0x100, 2, 1000, 1000]))
            return box("moof", box("traf", track) + box("free", Data(count: padding)))
        }
        let payload = 40_000_000
        let first = indexedMoof(padding: 3_000_000, decodeTime: 0)
            + words([UInt32(payload + 8)]) + Data("mdat".utf8)
        let second = indexedMoof(padding: 800_000, decodeTime: 2000)
            + words([UInt32(payload + 8)]) + Data("mdat".utf8)
        let secondOffset = first.count + payload
        let resource = CadenceStub.Resource(length: secondOffset + second.count + payload, ranges: true) { range in
            var result = Data(count: range.count)
            for (offset, bytes) in [(0, first), (secondOffset, second)] {
                let start = max(range.lowerBound, offset), end = min(range.upperBound, offset + bytes.count)
                if start < end {
                    result.replaceSubrange((start - range.lowerBound)..<(end - range.lowerBound),
                                           with: bytes[(start - offset)..<(end - offset)])
                }
            }
            return result
        }
        let host = CadenceStub.host()
        CadenceStub.install(host, [
            "/hls/s1/index.m3u8": .text(Self.playlist(map: true)),
            "/hls/s1/init.mp4": .bytes(initialization()),
            "/hls/s1/seg0.m4s": resource,
        ])
        let cadence = await HLSFragmentCadence.inspect(
            url: try XCTUnwrap(URL(string: "http://\(host)/hls/s1/index.m3u8")), itemSeconds: 1, boundMs: 4_000,
            configuration: CadenceStub.configuration())
        XCTAssertEqual(try XCTUnwrap(cadence).frameDurationSeconds, 1 / 24.0, accuracy: 1e-9)
        let ranges = CadenceStub.requestedRanges(host, "/hls/s1/seg0.m4s")
        XCTAssertLessThan(ranges.count, 32, "large metadata must not exhaust the original64-request allowance")
        XCTAssertTrue(ranges.contains { header in
            let ends = header.dropFirst(6).split(separator: "-").compactMap { Int($0) }
            return ends.count == 2 && ends[1] - ends[0] + 1 > 4_096
        })
        XCTAssertLessThanOrEqual(CadenceStub.served(host, "/hls/s1/seg0.m4s"), first.count + second.count + 8_192)
    }

    func testFragmentIndexWalkFailsClosedPastSixtyFourRequestsOnManyFragments() async throws {
        func media(fragmentCount: Int) -> Data {
            var result = Data()
            for index in 0..<fragmentCount {
                let track = box("tfhd", words([0x20000, 7]))
                    + box("tfdt", words([0, UInt32(index * 2000)]))
                    + box("trun", words([0x100, 2, 1000, 1000]))
                result.append(box("moof", box("traf", track)))
                result.append(box("mdat", Data(count: 8_192)))
            }
            return result
        }
        func inspect(fragmentCount: Int) async throws -> (PreparedDecodedCadence?, [String]) {
            let host = CadenceStub.host()
            CadenceStub.install(host, [
                "/hls/s1/index.m3u8": .text(Self.playlist(map: true)),
                "/hls/s1/init.mp4": .bytes(initialization()),
                "/hls/s1/seg0.m4s": .bytes(media(fragmentCount: fragmentCount)),
            ])
            let cadence = await HLSFragmentCadence.inspect(
                url: try XCTUnwrap(URL(string: "http://\(host)/hls/s1/index.m3u8")),
                itemSeconds: 1, boundMs: 4_000, configuration: CadenceStub.configuration())
            return (cadence, CadenceStub.requestedRanges(host, "/hls/s1/seg0.m4s"))
        }

        // The same well-formed representation resolves when the index walk fits.
        let small = try await inspect(fragmentCount: 2)
        XCTAssertEqual(try XCTUnwrap(small.0).frameDurationSeconds, 1 / 24.0, accuracy: 1e-9)
        XCTAssertLessThan(small.1.count, 64)
        // Small metadata separated by media payload requires further real Range
        // fetches; the terminal budget refuses rather than publishing a prefix.
        XCTAssertNotNil(HLSFragmentCadence.minimumDuration(
            initData: initialization(), fragment: media(fragmentCount: 80)),
            "The complete many-fragment sample is valid; only the bounded network walk refuses it")
        let many = try await inspect(fragmentCount: 80)
        XCTAssertNil(many.0)
        XCTAssertEqual(many.1.count, 64)
    }

    /// A server that ignores Range answers with the whole object. Its body is
    /// walked as it arrives, keeping only `moof` bytes, up to the byte cap
    /// the earlier whole-segment read allowed; past the cap it is refused.
    func testServerIgnoringRangeIsWalkedContiguouslyWithinTheCap() async throws {
        let walked = try await inspectCopy(try largeCopySegment(payload: 12_000_000, ranges: false))
        let cadence = try XCTUnwrap(walked.cadence)
        XCTAssertEqual(cadence.frameDurationSeconds, 1 / 24.0, accuracy: 1e-9)
        XCTAssertEqual(cadence.itemInterval, 0..<6)
        let refused = try await inspectCopy(try largeCopySegment(payload: HLSFragmentCadence.maximumUnrangedFragmentBytes, ranges: false))
        XCTAssertNil(refused.cadence)
    }

    func testTransportStreamSuccessorWithoutAMapResolvesItsCadence() async throws {
        let host = CadenceStub.host()
        CadenceStub.install(host, [
            "/hls/s2/index.m3u8": .text(Self.playlist(map: false, segment: "seg0.ts")),
            "/hls/s2/seg0.ts": .bytes(Fixtures.avcTransportSegment),
        ])
        let inspected = await HLSFragmentCadence.inspect(
            url: try XCTUnwrap(URL(string: "http://\(host)/hls/s2/index.m3u8")), itemSeconds: 2, boundMs: 4_000,
            configuration: CadenceStub.configuration())
        let cadence = try XCTUnwrap(inspected)
        XCTAssertEqual(cadence.frameDurationSeconds, 1 / 24.0, accuracy: 1e-9)
        XCTAssertEqual(cadence.itemInterval, 0..<6)
    }

    /// A TS read stops once a complete prefix proves the cadence just past
    /// the rendezvous, and the proof is scoped to that prefix.
    func testTransportStreamSuccessorStopsAtACompletePrefix() async throws {
        let host = CadenceStub.host()
        CadenceStub.install(host, [
            "/hls/s3/index.m3u8": .text(Self.playlist(map: false, segment: "seg0.ts", seconds: "1.000000")),
            "/hls/s3/seg0.ts": .bytes(Fixtures.avcTransportSegment),
        ])
        let inspected = await HLSFragmentCadence.inspect(
            url: try XCTUnwrap(URL(string: "http://\(host)/hls/s3/index.m3u8")), itemSeconds: 0.2, boundMs: 4_000,
            configuration: CadenceStub.configuration())
        let cadence = try XCTUnwrap(inspected)
        XCTAssertEqual(cadence.frameDurationSeconds, 1 / 24.0, accuracy: 1e-9)
        let interval = try XCTUnwrap(cadence.itemInterval)
        XCTAssertEqual(interval.lowerBound, 0)
        XCTAssertGreaterThanOrEqual(interval.upperBound, 0.2 + HLSFragmentCadence.transportStreamProofMarginSeconds)
        XCTAssertLessThan(interval.upperBound, 1, "the proof is scoped to the prefix it read")
        XCTAssertTrue(cadence.covers(itemPositionMs: 200))
    }

    func testTransportStreamPrefixProofReadsLessThanTheSegment() throws {
        for (segment, rate) in [(Fixtures.avcTransportSegment, 24.0), (Fixtures.hevcTransportSegment, 25.0)] {
            var stream = TransportStreamCadence()
            var consumed = 0
            var proof: (duration: Double, span: Double)?
            while consumed < segment.count, proof == nil {
                try stream.consume(Data(segment[consumed..<(consumed + 188)]))
                consumed += 188
                proof = stream.completePrefix(covering: 0.5)
            }
            let found = try XCTUnwrap(proof)
            XCTAssertEqual(found.duration, 1 / rate, accuracy: 1e-9)
            XCTAssertGreaterThanOrEqual(found.span, 0.5)
            XCTAssertLessThan(consumed, segment.count)
        }
        // A two-frame PES inside the prefix still refuses before any proof.
        var packets = [Data]()
        for at in stride(from: 0, to: Fixtures.avcTransportSegment.count, by: 188) {
            packets.append(Data(Fixtures.avcTransportSegment[at..<(at + 188)]))
        }
        let videoStarts = packets.indices.filter { packets[$0][1] & 0x40 != 0 && packets[$0][1] & 0x1F == 0x01 && packets[$0][2] == 0x00 }
        packets[videoStarts[2]][1] &= ~0x40
        var stream = TransportStreamCadence()
        var refused = false
        for packet in packets {
            if (try? stream.consume(packet)) == nil { refused = true; break }
            XCTAssertNil(stream.completePrefix(covering: 0.5))
        }
        XCTAssertTrue(refused)
    }

    private static func playlist(map: Bool, segment: String = "seg0.m4s", seconds: String = "6.000000") -> String {
        """
        #EXTM3U
        #EXT-X-VERSION:7
        #EXT-X-TARGETDURATION:6
        #EXT-X-MEDIA-SEQUENCE:0
        \(map ? "#EXT-X-MAP:URI=\"init.mp4\"" : "#EXT-X-INDEPENDENT-SEGMENTS")
        #EXTINF:\(seconds),
        \(segment)
        #EXT-X-ENDLIST

        """
    }
}

/// Serves fixed resources to `HLSFragmentCadence.inspect`, honouring
/// single `bytes=a-b` ranges when a resource allows them, and counts the
/// body bytes it actually delivers.
final class CadenceStub: URLProtocol, @unchecked Sendable {
    struct Resource {
        let length: Int
        let ranges: Bool
        let bytes: @Sendable (Range<Int>) -> Data
        let statuses: [Int]
        init(length: Int, ranges: Bool, statuses: [Int] = [], bytes: @escaping @Sendable (Range<Int>) -> Data) {
            self.length = length; self.ranges = ranges; self.statuses = statuses; self.bytes = bytes
        }
        static func bytes(_ data: Data) -> Resource {
            Resource(length: data.count, ranges: true) { Data(data[$0]) }
        }
        static func text(_ value: String) -> Resource { bytes(Data(value.utf8)) }
    }
    private static let lock = NSLock()
    nonisolated(unsafe) private static var resources: [String: [String: Resource]] = [:]
    nonisolated(unsafe) private static var counts: [String: Int] = [:]
    nonisolated(unsafe) private static var requests: [String: [String]] = [:]
    nonisolated(unsafe) private static var cancellations: [String: Int] = [:]
    private let stateLock = NSLock()
    private var stopped = false
    private var isStopped: Bool {
        stateLock.lock(); defer { stateLock.unlock() }
        return stopped
    }

    static func host() -> String { "cadence-\(UUID().uuidString.lowercased()).test" }
    static func install(_ host: String, _ table: [String: Resource]) {
        lock.lock(); resources[host] = table; lock.unlock()
    }
    static func served(_ host: String, _ path: String) -> Int {
        lock.lock(); defer { lock.unlock() }
        return counts[host + path] ?? 0
    }
    static func requestedRanges(_ host: String, _ path: String) -> [String] {
        lock.lock(); defer { lock.unlock() }
        return requests[host + path] ?? []
    }
    static func cancelled(_ host: String, _ path: String) -> Int {
        lock.lock(); defer { lock.unlock() }
        return cancellations[host + path] ?? 0
    }
    static func configuration() -> URLSessionConfiguration {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [CadenceStub.self]
        return configuration
    }
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host?.hasSuffix(".test") == true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let url = request.url, let host = url.host else { return }
        Self.lock.lock()
        let resource = Self.resources[host]?[url.path]
        let key = host + url.path
        let attempt = Self.requests[key, default: []].count
        Self.requests[key, default: []].append(request.value(forHTTPHeaderField: "Range") ?? "none")
        Self.lock.unlock()
        guard let resource else {
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: url, statusCode: 404, httpVersion: "HTTP/1.1", headerFields: [:])!,
                                cacheStoragePolicy: .notAllowed)
            client?.urlProtocolDidFinishLoading(self)
            return
        }
        var range = 0..<resource.length
        var status = resource.statuses.isEmpty ? 200 : resource.statuses[min(attempt, resource.statuses.count - 1)]
        var headers: [String: String] = [:]
        if status != 200 {
            // Advertise a body, but hold it until cancellation. A refused
            // response must cancel its actual task rather than wait for EOF.
            headers["Content-Length"] = String(resource.length)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!,
                                cacheStoragePolicy: .notAllowed)
            return
        }
        if resource.ranges, let header = request.value(forHTTPHeaderField: "Range"), header.hasPrefix("bytes=") {
            let span = header.dropFirst(6).split(separator: "-").compactMap { Int($0) }
            if span.count == 2, span[0] < resource.length {
                range = span[0]..<min(span[1] + 1, resource.length)
                status = 206
                headers["Content-Range"] = "bytes \(range.lowerBound)-\(range.upperBound - 1)/\(resource.length)"
            }
        }
        headers["Content-Length"] = String(range.count)
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!,
                            cacheStoragePolicy: .notAllowed)
        var at = range.lowerBound
        while at < range.upperBound, !isStopped {
            let end = min(at + 65_536, range.upperBound)
            client?.urlProtocol(self, didLoad: resource.bytes(at..<end))
            Self.lock.lock(); Self.counts[host + url.path, default: 0] += end - at; Self.lock.unlock()
            at = end
        }
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {
        stateLock.lock(); let first = !stopped; stopped = true; stateLock.unlock()
        if first, let url = request.url, let host = url.host {
            Self.lock.lock(); Self.cancellations[host + url.path, default: 0] += 1; Self.lock.unlock()
        }
    }
}
