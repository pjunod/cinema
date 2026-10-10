import Foundation

/// Cadence from actual samples, scoped to the inspected media interval.
struct PreparedDecodedCadence {
    let frameDurationSeconds: Double
    var itemInterval: Range<Double>?
    func covers(itemPositionMs: Int) -> Bool {
        guard frameDurationSeconds.isFinite, frameDurationSeconds > 0, frameDurationSeconds <= 1 else { return false }
        return itemInterval?.contains(Double(itemPositionMs) / 1000) ?? true
    }
}

enum HLSFragmentCadence {
    private struct Box { let type: String; let body: Int; let end: Int }
    private enum Invalid: Error { case media }
    private static func uint(_ data: Data, _ at: Int) throws -> UInt32 {
        guard at >= 0, at <= data.count - 4 else { throw Invalid.media }
        return (0..<4).reduce(0) { ($0 << 8) | UInt32(data[at + $1]) }
    }
    private static func boxes(_ data: Data, _ range: Range<Int>) throws -> [Box] {
        guard range.lowerBound >= 0, range.upperBound <= data.count else { throw Invalid.media }
        var at = range.lowerBound, result: [Box] = []
        while at < range.upperBound {
            guard range.upperBound - at >= 8, result.count < 8192 else { throw Invalid.media }
            let size = try uint(data, at)
            var header = 8, length = UInt64(size)
            if size == 1 {
                guard range.upperBound - at >= 16 else { throw Invalid.media }
                length = UInt64(try uint(data, at + 8)) << 32 | UInt64(try uint(data, at + 12)); header = 16
            } else if size == 0 { length = UInt64(range.upperBound - at) }
            guard length >= UInt64(header), length <= UInt64(range.upperBound - at) else { throw Invalid.media }
            let type = String(decoding: data[(at + 4)..<(at + 8)], as: UTF8.self)
            let end = at + Int(length)
            result.append(Box(type: type, body: at + header, end: end)); at = end
        }
        return result
    }
    private static func children(_ data: Data, _ box: Box) throws -> [Box] { try boxes(data, box.body..<box.end) }
    private static func word(_ data: Data, _ box: Box, _ offset: Int) throws -> UInt32 {
        guard offset >= 0, box.end - box.body >= offset + 4 else { throw Invalid.media }
        return try uint(data, box.body + offset)
    }

    /// Resolve trun / tfhd / trex defaults on the actual video track. Reject
    /// missing or zero durations, malformed boxes and unbounded sample tables.
    static func minimumDuration(initData: Data, fragment: Data) -> Double? {
        do {
            guard initData.startIndex == 0, fragment.startIndex == 0,
                  initData.count <= 262_144, fragment.count <= 16_777_216,
                  let moov = try boxes(initData, 0..<initData.count).first(where: { $0.type == "moov" }) else { return nil }
            let movie = try children(initData, moov)
            var videoID: UInt32?, timescale: UInt32 = 0, defaultDuration: UInt32 = 0
            for trak in movie where trak.type == "trak" {
                let track = try children(initData, trak)
                guard let mdia = track.first(where: { $0.type == "mdia" }), let tkhd = track.first(where: { $0.type == "tkhd" }) else { continue }
                let media = try children(initData, mdia)
                guard let handler = media.first(where: { $0.type == "hdlr" }), try word(initData, handler, 8) == 0x76696465,
                      let mdhd = media.first(where: { $0.type == "mdhd" }) else { continue }
                guard videoID == nil else { return nil }
                let trackVersion = try word(initData, tkhd, 0) >> 24
                let mediaVersion = try word(initData, mdhd, 0) >> 24
                guard trackVersion <= 1, mediaVersion <= 1 else { return nil }
                videoID = try word(initData, tkhd, trackVersion == 1 ? 20 : 12)
                timescale = try word(initData, mdhd, mediaVersion == 1 ? 20 : 12)
            }
            guard let videoID, videoID != 0, (1...1_000_000).contains(timescale) else { return nil }
            if let mvex = movie.first(where: { $0.type == "mvex" }) {
                for trex in try children(initData, mvex) where trex.type == "trex" {
                    if try word(initData, trex, 4) == videoID { defaultDuration = try word(initData, trex, 12) }
                }
            }
            let top = try boxes(fragment, 0..<fragment.count)
            guard top.contains(where: { $0.type == "mdat" && $0.end > $0.body }) else { return nil }
            var minimum = UInt32.max, samples = 0
            var presentationTimes: [Int64] = []
            for moof in top where moof.type == "moof" {
                for traf in try children(fragment, moof) where traf.type == "traf" {
                    let track = try children(fragment, traf)
                    guard let tfhd = track.first(where: { $0.type == "tfhd" }), try word(fragment, tfhd, 4) == videoID else { continue }
                    guard let tfdt = track.first(where: { $0.type == "tfdt" }) else { return nil }
                    let decodeVersion = try word(fragment, tfdt, 0) >> 24
                    guard decodeVersion <= 1 else { return nil }
                    let base = decodeVersion == 0 ? UInt64(try word(fragment, tfdt, 4))
                        : (UInt64(try word(fragment, tfdt, 4)) << 32 | UInt64(try word(fragment, tfdt, 8)))
                    guard base <= UInt64(Int64.max) else { return nil }
                    var decodeTime = Int64(base)
                    let flags = try word(fragment, tfhd, 0) & 0x00ff_ffff
                    var offset = 8, duration = defaultDuration
                    if flags & 1 != 0 { offset += 8 }
                    if flags & 2 != 0 { offset += 4 }
                    if flags & 8 != 0 { duration = try word(fragment, tfhd, offset); offset += 4 }
                    if flags & 0x10 != 0 { offset += 4 }
                    if flags & 0x20 != 0 { offset += 4 }
                    guard offset <= tfhd.end - tfhd.body else { return nil }
                    for trun in track where trun.type == "trun" {
                        let full = try word(fragment, trun, 0), count = try word(fragment, trun, 4)
                        guard full >> 24 <= 1, count > 0, count <= 100_000, samples <= 100_000 - Int(count) else { return nil }
                        let flags = full & 0x00ff_ffff
                        var at = 8
                        if flags & 1 != 0 { at += 4 }
                        if flags & 4 != 0 { at += 4 }
                        for _ in 0..<count {
                            var sampleDuration = duration
                            if flags & 0x100 != 0 { sampleDuration = try word(fragment, trun, at); at += 4 }
                            if flags & 0x200 != 0 { at += 4 }
                            if flags & 0x400 != 0 { at += 4 }
                            var compositionOffset: Int64 = 0
                            if flags & 0x800 != 0 {
                                let encoded = try word(fragment, trun, at); at += 4
                                compositionOffset = full >> 24 == 1 ? Int64(Int32(bitPattern: encoded)) : Int64(encoded)
                            }
                            guard at <= trun.end - trun.body, sampleDuration > 0, sampleDuration <= timescale else { return nil }
                            let (presentationTime, presentationOverflow) = decodeTime.addingReportingOverflow(compositionOffset)
                            let (nextDecodeTime, decodeOverflow) = decodeTime.addingReportingOverflow(Int64(sampleDuration))
                            guard !presentationOverflow, !decodeOverflow else { return nil }
                            presentationTimes.append(presentationTime); decodeTime = nextDecodeTime
                            minimum = min(minimum, sampleDuration); samples += 1
                        }
                        guard at == trun.end - trun.body else { return nil }
                    }
                }
            }
            guard samples > 0 else { return nil }
            // Reordered / variable-cadence samples can have presentation
            // spacing smaller than decode duration. Never widen the decoded
            // frame tolerance by ignoring composition offsets.
            presentationTimes.sort()
            for pair in zip(presentationTimes, presentationTimes.dropFirst()) {
                let (spacing, overflow) = pair.1.subtractingReportingOverflow(pair.0)
                guard !overflow, spacing > 0 else { return nil }
                if spacing < Int64(minimum) { minimum = UInt32(spacing) }
            }
            return Double(minimum) / Double(timescale)
        } catch { return nil }
    }

    private final class RefuseRedirects: NSObject, URLSessionTaskDelegate {
        func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                        newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
    }
    static func mediaChildURL(_ text: String, of base: URL, root: URL) -> URL? {
        guard !text.isEmpty, text.utf8.count <= 4096, !text.contains("\\"),
              let url = URL(string: text, relativeTo: base)?.absoluteURL,
              url.scheme == root.scheme, url.host == root.host, url.port == root.port,
              url.user == nil, url.password == nil, url.fragment == nil,
              !url.pathComponents.contains(where: { $0 == "." || $0 == ".." }),
              !url.path(percentEncoded: true).contains("%"),
              url.path.hasPrefix(root.deletingLastPathComponent().path + "/") else { return nil }
        return url
    }
    private static func load(_ url: URL, limit: Int, deadline: TimeInterval, session: URLSession) async throws -> Data {
        let remaining = deadline - ProcessInfo.processInfo.systemUptime
        guard remaining > 0, !Task.isCancelled else { throw Invalid.media }
        let request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: remaining)
        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200, http.url == url,
              http.expectedContentLength <= limit else { throw Invalid.media }
        var data = Data()
        for try await byte in bytes {
            guard data.count < limit else { throw Invalid.media }
            data.append(byte)
            if data.count & 4095 == 0, Task.isCancelled || ProcessInfo.processInfo.systemUptime >= deadline { throw Invalid.media }
        }
        guard !Task.isCancelled, ProcessInfo.processInfo.systemUptime < deadline,
              http.expectedContentLength < 0 || http.expectedContentLength == data.count else { throw Invalid.media }
        return data
    }
    /// One bounded init/fragment inspection of this item's own playlist.
    /// Redirects and URLs outside its media namespace are refused.
    static func inspect(url: URL, itemSeconds: Double, boundMs: Int) async -> PreparedDecodedCadence? {
        guard itemSeconds.isFinite, itemSeconds >= 0, boundMs > 0 else { return nil }
        let deadline = ProcessInfo.processInfo.systemUptime + Double(boundMs) / 1000
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil; configuration.timeoutIntervalForResource = Double(boundMs) / 1000
        let session = URLSession(configuration: configuration, delegate: RefuseRedirects(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        do {
            var playlistURL = url
            var text = String(decoding: try await load(url, limit: 1_048_576, deadline: deadline, session: session), as: UTF8.self)
            guard text.hasPrefix("#EXTM3U") else { return nil }
            if text.contains("#EXT-X-STREAM-INF:") {
                let lines = text.components(separatedBy: .newlines).map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                let variants = lines.enumerated().filter { $0.element.hasPrefix("#EXT-X-STREAM-INF:") }
                guard variants.count == 1, let index = variants.first?.offset, index + 1 < lines.count,
                      let resolved = mediaChildURL(lines[index + 1], of: url, root: url) else { return nil }
                playlistURL = resolved
                text = String(decoding: try await load(resolved, limit: 1_048_576, deadline: deadline, session: session), as: UTF8.self)
            }
            guard text.hasPrefix("#EXTM3U"), !text.contains("#EXT-X-BYTERANGE"), !text.contains("#EXT-X-KEY"),
                  !text.components(separatedBy: .newlines).contains(where: { $0.trimmingCharacters(in: .whitespacesAndNewlines) == "#EXT-X-DISCONTINUITY" }) else { return nil }
            var initURL: URL?, fragmentURL: URL?, interval: Range<Double>?, pending: Double?, elapsed = 0.0
            for raw in text.components(separatedBy: .newlines) {
                let line = raw.trimmingCharacters(in: .whitespacesAndNewlines)
                if line.hasPrefix("#EXT-X-MEDIA-SEQUENCE:") || line.hasPrefix("#EXT-X-DISCONTINUITY-SEQUENCE:") {
                    guard let value = line.split(separator: ":", maxSplits: 1).last, Int(value) == 0 else { return nil }
                } else if line.hasPrefix("#EXT-X-MAP:") {
                    guard initURL == nil, !line.contains("BYTERANGE"),
                          let start = line.range(of: "URI=\""), let end = line[start.upperBound...].firstIndex(of: "\""),
                          let resolved = mediaChildURL(String(line[start.upperBound..<end]), of: playlistURL, root: url) else { return nil }
                    initURL = resolved
                } else if line.hasPrefix("#EXTINF:") {
                    guard pending == nil, let rawDuration = line.dropFirst(8).split(separator: ",", maxSplits: 1).first,
                          let duration = Double(rawDuration),
                          duration.isFinite, duration > 0, duration <= 120 else { return nil }
                    pending = duration
                } else if !line.isEmpty && !line.hasPrefix("#") {
                    guard let duration = pending, let resolved = mediaChildURL(line, of: playlistURL, root: url) else { return nil }
                    if (elapsed..<(elapsed + duration)).contains(itemSeconds) {
                        fragmentURL = resolved; interval = elapsed..<(elapsed + duration)
                    }
                    elapsed += duration; pending = nil
                }
            }
            guard pending == nil, let initURL, let fragmentURL, let interval else { return nil }
            let initData = try await load(initURL, limit: 262_144, deadline: deadline, session: session)
            let fragment = try await load(fragmentURL, limit: 16_777_216, deadline: deadline, session: session)
            guard let duration = minimumDuration(initData: initData, fragment: fragment) else { return nil }
            return PreparedDecodedCadence(frameDurationSeconds: duration, itemInterval: interval)
        } catch { return nil }
    }
}
