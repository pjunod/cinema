import Foundation

/// AVPlayer's first-resource wait is shorter than a valid growing session's
/// preparation. Resolve the first playlist before giving its URL to the native
/// loader; a successful create alone does not promise publishable media.
enum GrowingPlaylistPreparation {
    private static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 60
        configuration.timeoutIntervalForResource = 60
        return URLSession(configuration: configuration)
    }()

    static func wait(for url: URL, using transport: URLSession? = nil) async throws {
        // A master can be available before its media. All variant requests
        // share the original budget, and a cyclic/malformed master cannot
        // create an unbounded preparation walk.
        let deadline = ProcessInfo.processInfo.systemUptime + 60
        var pending = [url]
        var visited = Set<URL>()
        while !pending.isEmpty {
            try Task.checkCancellation()
            let next = pending.removeFirst()
            guard visited.count < 16, visited.insert(next).inserted else {
                throw PlaybackPreparationError.failed
            }
            let remaining = deadline - ProcessInfo.processInfo.systemUptime
            guard remaining > 0 else { throw URLError(.timedOut) }
            var request = URLRequest(url: next, cachePolicy: .reloadIgnoringLocalCacheData,
                                     timeoutInterval: remaining)
            request.setValue("application/vnd.apple.mpegurl", forHTTPHeaderField: "Accept")
            let (data, response) = try await (transport ?? session).data(for: request)
            try Task.checkCancellation()
            guard let response = response as? HTTPURLResponse else { throw PlaybackPreparationError.failed }
            guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
            guard let text = String(data: data, encoding: .utf8), text.hasPrefix("#EXTM3U") else {
                throw PlaybackPreparationError.failed
            }
            let lines = text.components(separatedBy: .newlines).map {
                $0.trimmingCharacters(in: .whitespaces)
            }.filter { !$0.isEmpty }
            if lines.contains(where: { $0.hasPrefix("#EXT-X-STREAM-INF:") }) {
                for (index, line) in lines.enumerated() where line.hasPrefix("#EXT-X-STREAM-INF:") {
                    guard index + 1 < lines.count, !lines[index + 1].hasPrefix("#"),
                          let child = URL(string: lines[index + 1], relativeTo: response.url ?? next)?.absoluteURL,
                          ["http", "https"].contains(child.scheme?.lowercased() ?? "") else {
                        throw PlaybackPreparationError.failed
                    }
                    pending.append(child)
                }
            } else {
                guard lines.contains(where: { $0.hasPrefix("#EXTINF:") }),
                      lines.contains(where: { !$0.hasPrefix("#") }) else {
                    throw PlaybackPreparationError.failed
                }
            }
        }
    }
}
