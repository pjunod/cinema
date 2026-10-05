import SwiftUI
import UIKit

// Station artwork for the Live TV guide.
//
// Which address a channel's logo comes from is the shared guide contract's
// answer (`LiveTvGuideReducer.stationLogoURL`, pinned by
// `tests/playback/live-tv-guide-cases.json` `station_logo`). This file only
// fetches and draws it.
//
// The address is HDHomeRun's, not this server's, so it never goes through
// `AuthImage` or any session that carries the bearer token: the loader below
// owns an ephemeral session with no cookies, no credential store and no
// redirect off HTTPS. A logo is decoration — it never delays tuning, and one
// that is missing, refused or undecodable leaves the callsign, which is drawn
// first and stays until an image has actually decoded.

/// Fetches and decodes station logos, once per address.
final class LiveTvStationLogoLoader: @unchecked Sendable {
    static let shared = LiveTvStationLogoLoader()

    /// The longest edge any chip draws a logo at, in points (the fullscreen
    /// identity tile). Decoding to this once lets every surface share one entry.
    static let maxPoints: CGFloat = 112
    /// A logo that failed is not asked for again on every redraw of a 50-row
    /// guide; it is tried again after this long.
    static let failureRetrySeconds: TimeInterval = 10 * 60

    private let images = NSCache<NSString, UIImage>()
    private let lock = NSLock()
    private var flights: [String: Task<UIImage?, Never>] = [:]
    private var failures: [String: Date] = [:]
    private let session: URLSession

    init() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpCookieAcceptPolicy = .never
        configuration.httpShouldSetCookies = false
        configuration.urlCredentialStorage = nil
        configuration.timeoutIntervalForRequest = 15
        configuration.httpMaximumConnectionsPerHost = 4
        session = URLSession(configuration: configuration, delegate: HttpsOnlyRedirects(), delegateQueue: nil)
        images.countLimit = 256
    }

    func cached(_ address: String) -> UIImage? {
        images.object(forKey: address as NSString)
    }

    func image(for address: String) async -> UIImage? {
        if let hit = cached(address) { return hit }
        guard let flight = joinOrStart(address, now: Date()) else { return nil }
        let image = await flight.value
        finish(address, flight: flight, image: image, now: Date())
        return image
    }

    /// The fetch in flight for `address`, a new one, or nil while a recent
    /// failure is still backing off. Synchronous: the lock is never held
    /// across a suspension point.
    private func joinOrStart(_ address: String, now: Date) -> Task<UIImage?, Never>? {
        lock.lock()
        defer { lock.unlock() }
        if let failedAt = failures[address], now.timeIntervalSince(failedAt) < Self.failureRetrySeconds {
            return nil
        }
        if let existing = flights[address] { return existing }
        let flight = Task.detached(priority: .utility) { [session] in
            await Self.fetch(address, session: session)
        }
        flights[address] = flight
        return flight
    }

    /// Every waiter reports; only the first to arrive for this flight records
    /// it, and a newer flight for the same address is never cleared by a
    /// waiter of an older one.
    private func finish(_ address: String, flight: Task<UIImage?, Never>, image: UIImage?, now: Date) {
        lock.lock()
        defer { lock.unlock() }
        guard flights[address] == flight else { return }
        flights[address] = nil
        if let image {
            images.setObject(image, forKey: address as NSString)
            failures[address] = nil
        } else {
            failures[address] = now
        }
    }

    private static func fetch(_ address: String, session: URLSession) async -> UIImage? {
        guard let url = URL(string: address) else { return nil }
        var request = URLRequest(url: url)
        request.setValue("image/*", forHTTPHeaderField: "Accept")
        guard let (data, response) = try? await session.data(for: request),
              let http = response as? HTTPURLResponse, http.statusCode == 200 else { return nil }
        return AuthImageCache.downsample(data, maxPixelSize: maxPoints * AuthImageCache.displayScale)
    }
}

/// The contract only admits `https://`; a redirect must not walk the request
/// off it (the app allows arbitrary loads, for LAN servers).
private final class HttpsOnlyRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(request.url?.scheme?.lowercased() == "https" ? request : nil)
    }
}

/// The station tile: the callsign, replaced by the logo once it has decoded.
///
/// The callsign is the fallback and the layout: it is drawn immediately, the
/// tile never changes size, and an image that never arrives leaves exactly
/// what was there before logos existed.
struct LiveTvStationChip: View {
    let name: String
    let logo: String?
    var width: CGFloat
    var height: CGFloat
    var font: Font = LiveTvType.chip
    var cornerRadius: CGFloat = 6
    var background: Color = Palette.surfaceHi
    var foreground: Color = Palette.muted
    var padding: CGFloat = 5

    /// The decoded logo and the address it is for: a row reused for another
    /// channel must not show the previous station for a frame.
    @State private var loaded: (address: String, image: UIImage)?

    private var shown: UIImage? {
        guard let logo else { return nil }
        if let loaded, loaded.address == logo { return loaded.image }
        // Already decoded elsewhere: draw it on the first frame, so a lazy
        // list that recreates the row does not flash the callsign.
        return LiveTvStationLogoLoader.shared.cached(logo)
    }

    var body: some View {
        let image = shown
        return ZStack {
            Text(name)
                .font(font)
                .foregroundStyle(foreground)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .padding(.horizontal, 4)
                .opacity(image == nil ? 1 : 0)
            if let image {
                Image(uiImage: image)
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fit)
                    .padding(padding)
            }
        }
        .frame(width: width, height: height)
        .background(background)
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius))
        .accessibilityHidden(true)
        .task(id: logo) {
            guard let logo, LiveTvStationLogoLoader.shared.cached(logo) == nil else { return }
            let image = await LiveTvStationLogoLoader.shared.image(for: logo)
            if !Task.isCancelled, let image { loaded = (logo, image) }
        }
    }
}

/// Tile sizes for the surfaces that are not a list row or a grid header
/// (those keep their sizes beside the geometry they belong to).
enum LiveTvStationChipMetrics {
    // `detail` is no taller than the title line it sits beside: the tvOS
    // guide stage is a fixed 302 pt, and a taller row would cost the
    // synopsis a line.
    #if os(tvOS)
    static let detail = CGSize(width: 64, height: 36)
    static let pictureBadge = CGSize(width: 72, height: 34)
    #else
    static let detail = CGSize(width: 40, height: 22)
    static let pictureBadge = CGSize(width: 44, height: 22)
    #endif
    /// The fullscreen identity tile, the largest any chip draws.
    static let identity = CGSize(width: LiveTvStationLogoLoader.maxPoints,
                                 height: LiveTvStationLogoLoader.maxPoints)
}
