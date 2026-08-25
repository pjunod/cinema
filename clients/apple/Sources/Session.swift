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
    ///
    /// Every candidate is validated again here because it becomes a request
    /// authority. A candidate whose scheme is weaker than the one this session
    /// is already using is refused: a direct-play failover carries the account
    /// token in the query string, and an `http` sibling in an `https`
    /// household would put it on the wire in cleartext.
    func configureNodeOrigins(_ peers: [String], primary: String) {
        let primary = Self.canonicalOrigin(primary)
        let requiresTLS = primary?.hasPrefix("https://") ?? false
        var seen = Set<String>()
        let origins = peers.compactMap(Self.canonicalOrigin).filter { candidate in
            candidate != primary
                && (!requiresTLS || candidate.hasPrefix("https://"))
                && seen.insert(candidate).inserted
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

    /// This session's own origin in the same canonical form the failover list
    /// uses, so `https://h` and `https://h:443` do not read as two servers.
    var canonicalPrimaryOrigin: String? { Self.canonicalOrigin(origin) }

    /// Scheme and host lowercased, a default port removed, an IPv6 literal
    /// re-bracketed — so two spellings of one address compare equal. Userinfo,
    /// a path, a query, a fragment, an out-of-range port, or a non-HTTP scheme
    /// make it unusable: this string becomes a request authority.
    ///
    /// `internal` rather than `private` so the tests can state the rules
    /// directly; the Android client's `Session.canonicalOrigin` must agree
    /// case for case, and there is no shared implementation to lean on.
    static func canonicalOrigin(_ raw: String) -> String? {
        guard let components = URLComponents(string: raw),
              let scheme = components.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              let host = components.host?
                  .lowercased()
                  .trimmingCharacters(in: CharacterSet(charactersIn: "[]")),
              !host.isEmpty,
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil,
              components.path.isEmpty || components.path == "/"
        else { return nil }
        let defaultPort = scheme == "https" ? 443 : 80
        var port = ""
        if let explicit = components.port {
            guard explicit > 0, explicit <= 65_535 else { return nil }
            if explicit != defaultPort { port = ":\(explicit)" }
        }
        // Foundation has returned an IPv6 literal both with and without its
        // brackets depending on OS version, so they are stripped above and
        // put back exactly once here — an origin missing them is
        // unparseable, and one with two sets is a different string.
        let authority = host.contains(":") ? "[\(host)]" : host
        return "\(scheme)://\(authority)\(port)"
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
