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
    private enum Invalid: Error { case media, unavailable }
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

    /// One whole media segment, walked exactly as the network path walks it.
    static func minimumDuration(initData: Data, fragment: Data) -> Double? {
        var walk = FragmentIndex(total: fragment.count)
        guard (try? walk.consume(fragment, at: 0)) != nil else { return nil }
        return minimumDuration(initData: initData, index: walk)
    }

    /// Resolve trun / tfhd / trex defaults on the actual video track. Reject
    /// missing or zero durations, malformed boxes and unbounded sample tables.
    /// Only the segment's `moof` boxes are read: sample timing lives there,
    /// and the media payload is never needed to prove it.
    static func minimumDuration(initData: Data, index walk: FragmentIndex) -> Double? {
        guard walk.finished, walk.sawMedia else { return nil }
        let fragment = walk.index
        do {
            guard initData.startIndex == 0, fragment.startIndex == 0,
                  initData.count <= 262_144,
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

    // MARK: - Bounded chunked transport

    /// One ranged read of a segment's box index. A `moof` and the next box
    /// header almost always fit in the first window; a large `mdat` is
    /// stepped over by the next window's offset rather than downloaded.
    static let indexWindowBytes = 262_144
    /// Retained `moof` bytes across one segment.
    static let maximumIndexBytes = 4_194_304
    /// Windows per segment: one per `moof`/`mdat` pair in practice.
    static let maximumIndexRequests = 64
    /// A segment from a server that ignores Range is walked contiguously as
    /// it arrives; this bounds the bytes such a server may cost.
    static let maximumUnrangedFragmentBytes = 16_777_216
    /// An MPEG-TS segment has no index, so it is read in arrival order,
    /// keeping only timestamps, and only until it proves the cadence just
    /// past the rendezvous. This bounds the bytes it may cost.
    static let maximumTransportStreamBytes = 16_777_216
    /// How far past the rendezvous an MPEG-TS prefix must prove its cadence.
    static let transportStreamProofMarginSeconds = 0.5

    /// Routes each task's response and body chunks to the one reader waiting
    /// on it, and refuses redirects so a read never leaves the item's own
    /// media namespace.
    private final class ChunkedLoads: NSObject, URLSessionDataDelegate, @unchecked Sendable {
        enum Event { case response(URLResponse), bytes(Data) }
        private let lock = NSLock()
        private var readers: [Int: AsyncThrowingStream<Event, Error>.Continuation] = [:]

        func events(for request: URLRequest, on session: URLSession) -> (task: URLSessionDataTask, stream: AsyncThrowingStream<Event, Error>) {
            let (stream, continuation) = AsyncThrowingStream<Event, Error>.makeStream()
            let task = session.dataTask(with: request)
            lock.lock(); readers[task.taskIdentifier] = continuation; lock.unlock()
            continuation.onTermination = { _ in task.cancel() }
            task.resume()
            return (task, stream)
        }
        private func reader(_ task: URLSessionTask, removing: Bool = false) -> AsyncThrowingStream<Event, Error>.Continuation? {
            lock.lock(); defer { lock.unlock() }
            return removing ? readers.removeValue(forKey: task.taskIdentifier) : readers[task.taskIdentifier]
        }
        func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
            reader(dataTask)?.yield(.response(response))
            // Refused status bodies cannot contribute media evidence. Cancel
            // at the response boundary, before allowing any body transfer.
            let status = (response as? HTTPURLResponse)?.statusCode
            completionHandler(status == 200 || status == 206 ? .allow : .cancel)
        }
        func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
            reader(dataTask)?.yield(.bytes(data))
        }
        func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
            let reader = reader(task, removing: true)
            if let error { reader?.finish(throwing: error) } else { reader?.finish() }
        }
        func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                        newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
    }

    /// Hand each body chunk to `chunk` as it arrives; `false` ends the read
    /// early and cancels its transfer. The byte limit, the deadline and
    /// cancellation are checked per chunk.
    private static func fetch(_ url: URL, range: Range<Int>? = nil, limit: Int, deadline: TimeInterval,
                              session: URLSession, loads: ChunkedLoads,
                              chunk: (HTTPURLResponse, Data) throws -> Bool) async throws -> HTTPURLResponse {
        while true {
            do {
                return try await fetchAttempt(url, range: range, limit: limit, deadline: deadline,
                                              session: session, loads: loads, chunk: chunk)
            } catch Invalid.unavailable {
                // 503 is an unavailable response, not permission to extend the
                // preparation or sample proof. The same URL/Range gets only
                // what remains of the original absolute deadline.
                let remaining = deadline - ProcessInfo.processInfo.systemUptime
                guard remaining > 0, !Task.isCancelled else { throw Invalid.media }
                try await Task.sleep(nanoseconds: UInt64(min(0.1, remaining) * 1_000_000_000))
            }
        }
    }

    private static func fetchAttempt(_ url: URL, range: Range<Int>?, limit: Int, deadline: TimeInterval,
                                     session: URLSession, loads: ChunkedLoads,
                                     chunk: (HTTPURLResponse, Data) throws -> Bool) async throws -> HTTPURLResponse {
        let remaining = deadline - ProcessInfo.processInfo.systemUptime
        guard remaining > 0, !Task.isCancelled else { throw Invalid.media }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: remaining)
        if let range {
            guard range.lowerBound >= 0, !range.isEmpty else { throw Invalid.media }
            request.setValue("bytes=\(range.lowerBound)-\(range.upperBound - 1)", forHTTPHeaderField: "Range")
        }
        var response: HTTPURLResponse?
        var received = 0
        let transfer = loads.events(for: request, on: session)
        defer { transfer.task.cancel() }
        for try await event in transfer.stream {
            guard !Task.isCancelled, ProcessInfo.processInfo.systemUptime < deadline else { throw Invalid.media }
            switch event {
            case .response(let value):
                guard response == nil, let http = value as? HTTPURLResponse, http.url == url else { throw Invalid.media }
                if http.statusCode == 503 { throw Invalid.unavailable }
                guard http.statusCode == 200 || (range != nil && http.statusCode == 206),
                      http.expectedContentLength <= limit else { throw Invalid.media }
                response = http
            case .bytes(let data):
                guard let response else { throw Invalid.media }
                received += data.count
                guard received <= limit else { throw Invalid.media }
                if try !chunk(response, data) { return response }
            }
        }
        guard let response, !Task.isCancelled, ProcessInfo.processInfo.systemUptime < deadline,
              response.expectedContentLength < 0 || response.expectedContentLength == received else { throw Invalid.media }
        return response
    }

    private static func load(_ url: URL, limit: Int, deadline: TimeInterval, session: URLSession, loads: ChunkedLoads) async throws -> Data {
        var data = Data()
        _ = try await fetch(url, limit: limit, deadline: deadline, session: session, loads: loads) { data.append($1); return true }
        return data
    }

    /// `bytes first-last/total` of a 206 answering exactly `offset`.
    static func contentRange(_ header: String?, offset: Int, count: Int) -> Int? {
        guard let header, header.hasPrefix("bytes "), count > 0 else { return nil }
        let parts = header.dropFirst(6).split(separator: "/", maxSplits: 1)
        guard parts.count == 2, let total = Int(parts[1]) else { return nil }
        let span = parts[0].split(separator: "-", maxSplits: 1)
        guard span.count == 2, let first = Int(span[0]), let last = Int(span[1]),
              first == offset, last == offset + count - 1, last < total else { return nil }
        return total
    }

    /// Walk the segment's top-level boxes through bounded range windows,
    /// retaining every `moof` and stepping over each `mdat`. A server that
    /// ignores the range answers 200 with the whole object: that body is
    /// walked contiguously as it arrives — still keeping only `moof` bytes —
    /// and the transfer stops as soon as the boxes tile the segment.
    private static func fragmentIndex(_ url: URL, deadline: TimeInterval, session: URLSession, loads: ChunkedLoads) async throws -> FragmentIndex {
        var walk: FragmentIndex?
        var offset = 0
        for _ in 0..<maximumIndexRequests {
            var window = Data()
            var whole: FragmentIndex?
            let response = try await fetch(url, range: offset..<(offset + indexWindowBytes), limit: maximumUnrangedFragmentBytes,
                                           deadline: deadline, session: session, loads: loads) { response, chunk in
                if response.statusCode == 206 {
                    window.append(chunk)
                    guard window.count <= indexWindowBytes else { throw Invalid.media }
                    return true
                }
                guard offset == 0, response.expectedContentLength >= 0 else { throw Invalid.media }
                var current = whole ?? FragmentIndex(total: Int(response.expectedContentLength))
                try current.consume(chunk, at: current.needed)
                whole = current
                return !current.finished
            }
            guard response.statusCode == 206 else {
                guard let whole, whole.finished else { throw Invalid.media }
                return whole
            }
            guard let total = contentRange(response.value(forHTTPHeaderField: "Content-Range"), offset: offset, count: window.count) else { throw Invalid.media }
            var current = walk ?? FragmentIndex(total: total)
            guard current.total == total else { throw Invalid.media }
            try current.consume(window, at: offset)
            walk = current
            if current.finished { return current }
            guard current.resumeOffset > offset else { throw Invalid.media }
            offset = current.resumeOffset
        }
        throw Invalid.media
    }

    /// Read an MPEG-TS segment only until a complete prefix proves the
    /// cadence `seconds` into it; a segment that ends first is judged whole.
    /// The answer's `span` scopes the proof when it came from a prefix.
    private static func transportStreamCadence(_ url: URL, proving seconds: Double, deadline: TimeInterval,
                                               session: URLSession, loads: ChunkedLoads) async throws -> (duration: Double, span: Double?)? {
        var stream = TransportStreamCadence()
        var prefix: (duration: Double, span: Double)?
        _ = try await fetch(url, limit: maximumTransportStreamBytes, deadline: deadline, session: session, loads: loads) { _, chunk in
            try stream.consume(chunk)
            prefix = stream.completePrefix(covering: seconds)
            return prefix == nil
        }
        if let prefix { return (prefix.duration, prefix.span) }
        return stream.minimumDuration().map { ($0, nil) }
    }

    /// One bounded init/fragment inspection of this item's own playlist.
    /// Redirects and URLs outside its media namespace are refused.
    static func inspect(url: URL, itemSeconds: Double, boundMs: Int,
                        configuration: URLSessionConfiguration = .ephemeral) async -> PreparedDecodedCadence? {
        guard itemSeconds.isFinite, itemSeconds >= 0, boundMs > 0 else { return nil }
        let deadline = ProcessInfo.processInfo.systemUptime + Double(boundMs) / 1000
        configuration.urlCache = nil; configuration.timeoutIntervalForResource = Double(boundMs) / 1000
        let loads = ChunkedLoads()
        let session = URLSession(configuration: configuration, delegate: loads, delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        do {
            var playlistURL = url
            var text = String(decoding: try await load(url, limit: 1_048_576, deadline: deadline, session: session, loads: loads), as: UTF8.self)
            guard text.hasPrefix("#EXTM3U") else { return nil }
            if text.contains("#EXT-X-STREAM-INF:") {
                let lines = text.components(separatedBy: .newlines).map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                let variants = lines.enumerated().filter { $0.element.hasPrefix("#EXT-X-STREAM-INF:") }
                guard variants.count == 1, let index = variants.first?.offset, index + 1 < lines.count,
                      let resolved = mediaChildURL(lines[index + 1], of: url, root: url) else { return nil }
                playlistURL = resolved
                text = String(decoding: try await load(resolved, limit: 1_048_576, deadline: deadline, session: session, loads: loads), as: UTF8.self)
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
            guard pending == nil, let fragmentURL, let interval else { return nil }
            if let initURL {
                let initData = try await load(initURL, limit: 262_144, deadline: deadline, session: session, loads: loads)
                let index = try await fragmentIndex(fragmentURL, deadline: deadline, session: session, loads: loads)
                guard let duration = minimumDuration(initData: initData, index: index) else { return nil }
                return PreparedDecodedCadence(frameDurationSeconds: duration, itemInterval: interval)
            }
            // No EXT-X-MAP: an MPEG-TS media playlist (RFC 8216 §3.3).
            let proving = itemSeconds - interval.lowerBound + transportStreamProofMarginSeconds
            guard let proof = try await transportStreamCadence(fragmentURL, proving: proving, deadline: deadline,
                                                               session: session, loads: loads) else { return nil }
            let scoped = proof.span.map { interval.lowerBound..<min(interval.upperBound, interval.lowerBound + $0) } ?? interval
            return PreparedDecodedCadence(frameDurationSeconds: proof.duration, itemInterval: scoped)
        } catch { return nil }
    }
}

extension HLSFragmentCadence {
    /// An incremental walk over one fMP4 media segment's top-level boxes.
    ///
    /// Chunks arrive at absolute offsets: either contiguously, or at
    /// `resumeOffset` after stepping over a box. Only `moof` boxes are
    /// retained; everything else (`mdat`, `styp`, `sidx`, …) is stepped over,
    /// so the media payload is never held. Every box must end inside the
    /// segment's known length, and the walk is finished only when the boxes
    /// tile the segment exactly.
    struct FragmentIndex {
        let total: Int
        /// Absolute offset of the next byte this walk has not seen.
        private(set) var needed = 0
        /// Complete `moof` boxes, in segment order.
        private(set) var index = Data()
        private(set) var sawMedia = false
        private var header = Data()
        private var headerStart = 0
        private var moof: Data?
        private var moofEnd = 0
        private var skipEnd: Int?
        private var boxes = 0

        init(total: Int) { self.total = total }

        /// Where the next read must start: past a box being stepped over.
        var resumeOffset: Int { skipEnd ?? needed }
        var finished: Bool { header.isEmpty && moof == nil && resumeOffset == total && total >= 0 }

        private struct Malformed: Error {}

        mutating func consume(_ chunk: Data, at offset: Int) throws {
            if offset != needed {
                guard let skipEnd, offset == skipEnd else { throw Malformed() }
                needed = skipEnd
                self.skipEnd = nil
            }
            let bytes = [UInt8](chunk)
            guard needed + bytes.count <= total else { throw Malformed() }
            var at = 0
            while at < bytes.count {
                if let end = skipEnd {
                    let take = min(end - needed, bytes.count - at)
                    at += take; needed += take
                    if needed == end { skipEnd = nil }
                    continue
                }
                if moof != nil {
                    let take = min(moofEnd - needed, bytes.count - at)
                    moof?.append(contentsOf: bytes[at..<(at + take)])
                    at += take; needed += take
                    if needed == moofEnd, let complete = moof { index.append(complete); moof = nil }
                    continue
                }
                if header.isEmpty { headerStart = needed }
                header.append(bytes[at]); at += 1; needed += 1
                guard header.count >= 8 else { continue }
                let size = header.prefix(4).reduce(0) { ($0 << 8) | Int($1) }
                if size == 1 && header.count < 16 { continue }
                let headerLength = size == 1 ? 16 : 8
                let length: Int
                if size == 1 {
                    let large = header[8..<16].reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
                    guard large <= UInt64(Int.max) else { throw Malformed() }
                    length = Int(large)
                } else if size == 0 {
                    length = total - headerStart
                } else {
                    length = size
                }
                boxes += 1
                guard boxes <= 8192, length >= headerLength, length <= total - headerStart else { throw Malformed() }
                let type = String(decoding: header[4..<8], as: UTF8.self)
                let end = headerStart + length
                if type == "moof" {
                    guard index.count + length <= HLSFragmentCadence.maximumIndexBytes else { throw Malformed() }
                    if end == needed { index.append(header) } else { moof = header; moofEnd = end }
                } else {
                    if type == "mdat" && length > headerLength { sawMedia = true }
                    if end > needed { skipEnd = end }
                }
                header = Data()
            }
        }
    }
}

/// Presentation cadence of one MPEG-TS media segment, from the video
/// elementary stream's own PES timestamps.
///
/// MPEG-TS has no sample table. Its timing proof is the PTS on each video
/// PES, and that is only a per-frame proof when every PES carries exactly one
/// access unit — counted here by its access unit delimiter (H.264 type 9,
/// HEVC type 35), which ffmpeg's TS muxer always writes. A PES with two
/// frames would double the apparent spacing and widen the decoded-frame
/// tolerance, so it refuses rather than guessing. Fed in chunks; retains
/// only timestamps and at most one partial packet.
struct TransportStreamCadence {
    private static let packetLength = 188
    private static let maximumFrames = 100_000
    private var partial: [UInt8] = []
    private var pmtPID: Int?
    private var videoPID: Int?
    private var hevc = false
    private var unclassified = Set<Int>()
    private var continuity: Int?
    private var times: [Int64] = []
    private var firstPTS: Int64?
    /// Decode time of the most recently opened PES. Decode order is
    /// strictly increasing and every frame presents no earlier than it
    /// decodes, so every frame not yet read presents at or after this.
    private var lastDTS: Int64?
    private var lowestPTS = Int64.max
    private var inPES = false
    private var accessUnits = 0
    private var zeros = 0
    private var startCode = false
    private var failed = false

    private struct Malformed: Error {}

    mutating func consume(_ chunk: Data) throws {
        guard !failed else { throw Malformed() }
        do {
            var bytes = [UInt8](chunk)
            if !partial.isEmpty {
                let take = min(Self.packetLength - partial.count, bytes.count)
                partial.append(contentsOf: bytes[0..<take])
                bytes.removeFirst(take)
                guard partial.count == Self.packetLength else { return }
                try packet(partial[...])
                partial = []
            }
            var at = 0
            while bytes.count - at >= Self.packetLength {
                try packet(bytes[at..<(at + Self.packetLength)])
                at += Self.packetLength
            }
            partial = Array(bytes[at...])
        } catch {
            failed = true
            throw error
        }
    }

    /// The minimum spacing between distinct presentation times, or nil when
    /// the segment did not prove one.
    mutating func minimumDuration() -> Double? {
        guard !failed, partial.isEmpty, videoPID != nil, (try? closePES()) != nil, times.count >= 2 else { return nil }
        let sorted = times.sorted()
        var minimum = Int64.max
        for pair in zip(sorted, sorted.dropFirst()) {
            let spacing = pair.1 - pair.0
            guard spacing > 0 else { return nil }
            minimum = min(minimum, spacing)
        }
        guard minimum <= 90_000 else { return nil }
        return Double(minimum) / 90_000
    }

    /// The cadence of the segment's complete prefix, once that prefix spans
    /// `seconds` from the segment's first presented frame.
    ///
    /// Frames are read in decode order, so a prefix of the bytes is not a
    /// prefix of presentation. Every unread frame presents at or after the
    /// last decode time read, so the frames presenting before it are all
    /// present; the proof uses only those, and only the span between their
    /// first and last presentation, where every frame's duration is one of
    /// the measured spacings. Each of them sits in a PES already closed, so
    /// the one-access-unit, continuity and discontinuity rules hold for all
    /// of it exactly as for a whole segment.
    mutating func completePrefix(covering seconds: Double) -> (duration: Double, span: Double)? {
        guard !failed, seconds.isFinite, seconds > 0, let lastDTS,
              Double(lastDTS - lowestPTS) / 90_000 >= seconds else { return nil }
        let complete = times.filter { $0 < lastDTS }.sorted()
        guard complete.count >= 2, let first = complete.first, let last = complete.last,
              Double(last - first) / 90_000 >= seconds else { return nil }
        var minimum = Int64.max
        for pair in zip(complete, complete.dropFirst()) {
            let spacing = pair.1 - pair.0
            guard spacing > 0 else { failed = true; return nil }
            minimum = min(minimum, spacing)
        }
        guard minimum <= 90_000 else { failed = true; return nil }
        return (Double(minimum) / 90_000, Double(last - first) / 90_000)
    }

    private mutating func packet(_ p: ArraySlice<UInt8>) throws {
        let b = Array(p)
        guard b[0] == 0x47, b[1] & 0x80 == 0, b[3] & 0xC0 == 0 else { throw Malformed() }
        let start = b[1] & 0x40 != 0
        let pid = Int(b[1] & 0x1F) << 8 | Int(b[2])
        let control = (b[3] >> 4) & 3
        guard control != 0 else { throw Malformed() }
        var payload = 4
        var discontinuity = false
        if control & 2 != 0 {
            let length = Int(b[4])
            guard length <= 183 else { throw Malformed() }
            if length > 0 { discontinuity = b[5] & 0x80 != 0 }
            payload = 5 + length
        }
        guard pid != 0x1FFF, control & 1 != 0, payload < Self.packetLength else { return }
        let body = b[payload...]
        if pid == 0 {
            if start { try programAssociation(body) }
            return
        }
        if pid == pmtPID {
            if start { try programMap(body) }
            return
        }
        guard let videoPID else { unclassified.insert(pid); return }
        guard pid == videoPID else { return }
        // A lost video packet can drop a frame's timestamp and widen the
        // spacing; a signalled discontinuity breaks the timeline itself.
        let counter = Int(b[3] & 0x0F)
        guard !discontinuity, continuity.map({ (($0 + 1) & 15) == counter }) ?? true else { throw Malformed() }
        continuity = counter
        if start {
            try closePES()
            try openPES(body)
        } else {
            guard inPES else { throw Malformed() }
            scan(body)
        }
    }

    private func section(_ body: ArraySlice<UInt8>, table: UInt8) throws -> ArraySlice<UInt8> {
        guard let pointer = body.first else { throw Malformed() }
        let start = body.startIndex + 1 + Int(pointer)
        guard start + 3 <= body.endIndex, body[start] == table else { throw Malformed() }
        let length = Int(body[start + 1] & 0x0F) << 8 | Int(body[start + 2])
        // Sections that continue into a further packet are not followed.
        guard length >= 9, start + 3 + length <= body.endIndex else { throw Malformed() }
        return body[start..<(start + 3 + length - 4)]
    }

    private mutating func programAssociation(_ body: ArraySlice<UInt8>) throws {
        let table = try section(body, table: 0x00)
        var at = table.startIndex + 8
        var programs = Set<Int>()
        while at + 4 <= table.endIndex {
            let number = Int(table[at]) << 8 | Int(table[at + 1])
            let pid = Int(table[at + 2] & 0x1F) << 8 | Int(table[at + 3])
            if number != 0 { programs.insert(pid) }
            at += 4
        }
        guard programs.count == 1, let pid = programs.first, pmtPID == nil || pmtPID == pid else { throw Malformed() }
        pmtPID = pid
    }

    private mutating func programMap(_ body: ArraySlice<UInt8>) throws {
        let table = try section(body, table: 0x02)
        guard table.count >= 12 else { throw Malformed() }
        let infoLength = Int(table[table.startIndex + 10] & 0x0F) << 8 | Int(table[table.startIndex + 11])
        var at = table.startIndex + 12 + infoLength
        var video: [(pid: Int, hevc: Bool)] = []
        while at + 5 <= table.endIndex {
            let type = table[at]
            let pid = Int(table[at + 1] & 0x1F) << 8 | Int(table[at + 2])
            let infoLength = Int(table[at + 3] & 0x0F) << 8 | Int(table[at + 4])
            switch type {
            case 0x1B: video.append((pid, false))
            case 0x24: video.append((pid, true))
            // Other video coding: no delimiter rule is known for it here.
            case 0x01, 0x02, 0x10, 0x42, 0xEA: throw Malformed()
            default: break
            }
            at += 5 + infoLength
        }
        guard at == table.endIndex, video.count == 1, let chosen = video.first,
              videoPID == nil || videoPID == chosen.pid else { throw Malformed() }
        // A frame delivered before its stream was identified was not counted.
        guard !unclassified.contains(chosen.pid) else { throw Malformed() }
        videoPID = chosen.pid
        hevc = chosen.hevc
    }

    private mutating func openPES(_ body: ArraySlice<UInt8>) throws {
        let p = Array(body)
        guard p.count >= 14, p[0] == 0, p[1] == 0, p[2] == 1, (0xE0...0xEF).contains(p[3]),
              p[6] & 0xC0 == 0x80 else { throw Malformed() }
        let flags = p[7] >> 6
        let headerLength = Int(p[8])
        guard flags == 2 || flags == 3, headerLength >= (flags == 3 ? 10 : 5), 9 + headerLength <= p.count,
              p[9] >> 4 == (flags == 3 ? 3 : 2), p[9] & 1 == 1, p[11] & 1 == 1, p[13] & 1 == 1 else { throw Malformed() }
        func timestamp(_ at: Int) -> Int64 {
            Int64(p[at] >> 1 & 7) << 30 | Int64(p[at + 1]) << 22 | Int64(p[at + 2] >> 1) << 15 | Int64(p[at + 3]) << 7 | Int64(p[at + 4] >> 1)
        }
        let pts = timestamp(9)
        var dts = pts
        if flags == 3 {
            guard p[14] >> 4 == 1, p[14] & 1 == 1, p[16] & 1 == 1, p[18] & 1 == 1 else { throw Malformed() }
            dts = timestamp(14)
        }
        let first = firstPTS ?? pts
        firstPTS = first
        // 33-bit clock: place each timestamp nearest the segment's first.
        func relative(_ value: Int64) -> Int64 {
            let wrapped = (value - first) & ((1 << 33) - 1)
            return wrapped >= 1 << 32 ? wrapped - (1 << 33) : wrapped
        }
        let presentation = relative(pts), decode = relative(dts)
        guard decode <= presentation, lastDTS.map({ decode > $0 }) ?? true,
              times.count < Self.maximumFrames else { throw Malformed() }
        lastDTS = decode
        lowestPTS = min(lowestPTS, presentation)
        times.append(presentation)
        inPES = true
        accessUnits = 0
        zeros = 0
        startCode = false
        scan(body.dropFirst(9 + headerLength))
    }

    private mutating func closePES() throws {
        guard inPES else { return }
        guard accessUnits == 1 else { throw Malformed() }
        inPES = false
    }

    private mutating func scan(_ bytes: ArraySlice<UInt8>) {
        for byte in bytes {
            if startCode {
                let type = hevc ? (byte >> 1) & 0x3F : byte & 0x1F
                if type == (hevc ? 35 : 9) { accessUnits += 1 }
                startCode = false
            }
            if byte == 0 {
                zeros += 1
            } else {
                startCode = byte == 1 && zeros >= 2
                zeros = 0
            }
        }
    }
}
