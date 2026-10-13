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
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 60)
        request.setValue("application/vnd.apple.mpegurl", forHTTPHeaderField: "Accept")
        let (data, response) = try await (transport ?? session).data(for: request)
        try Task.checkCancellation()
        guard let response = response as? HTTPURLResponse else { throw PlaybackPreparationError.failed }
        guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
        guard data.starts(with: Data("#EXTM3U".utf8)) else { throw PlaybackPreparationError.failed }
    }
}
