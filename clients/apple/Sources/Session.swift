import Foundation

/// Live connection state. The origin + token are set once at connect/login and
/// read wherever a request, image, or media URL is built. Two auth shapes:
/// API/image requests carry `Authorization: Bearer`, while AVPlayer URLs (which
/// can't set headers) carry the token inline as `?token=` — both accepted by
/// the server's `AuthUser` extractor.
final class Session: @unchecked Sendable {
    static let shared = Session()

    /// Server origin, no trailing slash, e.g. `http://192.168.1.10:32400`.
    var origin: String = ""
    /// Bearer token, or nil when signed out.
    var token: String?

    private let nodeLock = NSLock()
    private var mediaFailoverOrigins: [String] = []
    private var mediaFailoverIndex = 0

    /// Install the server-advertised alternatives for this exact instance.
    /// Origins are validated again client-side because they become request
    /// authorities; userinfo, paths, queries, and non-HTTP schemes are refused.
    func configureNodeOrigins(_ peers: [String], primary: String) {
        let primary = Self.canonicalOrigin(primary)
        var seen = Set<String>()
        let origins = peers.compactMap(Self.canonicalOrigin).filter { candidate in
            candidate != primary && seen.insert(candidate).inserted
        }
        nodeLock.lock()
        mediaFailoverOrigins = origins
        mediaFailoverIndex = 0
        nodeLock.unlock()
    }

    func resetMediaFailover() {
        nodeLock.lock()
        mediaFailoverIndex = 0
        nodeLock.unlock()
    }

    /// Rebind one server-relative media capability to the next advertised
    /// ingress. This does not mutate account/API origin and therefore cannot
    /// turn a transport retry into a codec fallback or cross-server login.
    func nextMediaFailoverURL(_ path: String, authenticated: Bool) -> URL? {
        guard path.hasPrefix("/"), !path.hasPrefix("//") else { return nil }
        nodeLock.lock()
        guard mediaFailoverIndex < mediaFailoverOrigins.count else {
            nodeLock.unlock()
            return nil
        }
        let candidate = mediaFailoverOrigins[mediaFailoverIndex]
        mediaFailoverIndex += 1
        nodeLock.unlock()
        guard let base = URL(string: candidate + path) else { return nil }
        guard authenticated, let token else { return base }
        var components = URLComponents(url: base, resolvingAgainstBaseURL: false)
        var items = components?.queryItems ?? []
        items.append(URLQueryItem(name: "token", value: token))
        components?.queryItems = items
        return components?.url ?? base
    }

    private static func canonicalOrigin(_ raw: String) -> String? {
        guard var components = URLComponents(string: raw),
              ["http", "https"].contains(components.scheme?.lowercased() ?? ""),
              components.host != nil,
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil,
              components.path.isEmpty || components.path == "/"
        else { return nil }
        components.path = ""
        return components.string?.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
    }

    /// Absolute URL for a server-relative path.
    func url(_ path: String) -> URL? {
        if path.hasPrefix("http") { return URL(string: path) }
        return URL(string: origin + path)
    }

    /// Absolute URL with the token inline — for AVPlayer / `<img>`-style loads
    /// that can't set an Authorization header. Capability-authed HLS playlists
    /// (which already carry an unguessable session id) don't need this.
    func mediaURL(_ path: String) -> URL? {
        guard let base = url(path) else { return nil }
        guard let token, !path.hasPrefix("http") else { return base }
        var comps = URLComponents(url: base, resolvingAgainstBaseURL: false)
        var items = comps?.queryItems ?? []
        items.append(URLQueryItem(name: "token", value: token))
        comps?.queryItems = items
        return comps?.url ?? base
    }

    /// Add the bearer header to an API/image request.
    func authorize(_ request: inout URLRequest) {
        if let token {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
    }
}
