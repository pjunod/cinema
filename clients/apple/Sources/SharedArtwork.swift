import Foundation
import ImageIO
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
import SwiftUI
import Combine
#endif

enum SharedArtworkError: Error, LocalizedError {
    case capacity
    var errorDescription: String? { "Shared artwork capacity is busy." }
}

struct SharedArtworkDescriptor: Decodable, Equatable {
    let kind: String
    let variant: String
    let url: String
    func validate(reference: SharedPlaybackReference) throws {
        try reference.validate()
        let prefix = "/api/v1/shared/imports/\(reference.importId)/art/"
        guard ["poster", "backdrop"].contains(kind), ["original", "w300", "w500", "w780"].contains(variant),
              url.hasPrefix(prefix), PlaybackFileContext.matches(String(url.dropFirst(prefix.count)), "^[A-Za-z0-9_-]{272}$") else { throw APIError.badURL }
    }
    static func validate(_ rows: [Self]?, poster: String?, backdrop: String?, reference: SharedPlaybackReference) throws {
        let rows = rows ?? []
        guard rows.count <= 8, Set(rows.map { $0.kind + "|" + $0.variant }).count == rows.count else { throw APIError.badURL }
        try rows.forEach { try $0.validate(reference: reference) }
        if let poster { guard rows.contains(where: { $0.kind == "poster" && $0.variant == "w300" && $0.url == poster }) else { throw APIError.badURL } }
        if let backdrop { guard rows.contains(where: { $0.kind == "backdrop" && $0.variant == "w780" && $0.url == backdrop }) else { throw APIError.badURL } }
    }
}

/// No queue and no eviction assumption. Credits follow their actual owners.
final class SharedArtworkBudget {
    static let compressed = SharedArtworkBudget(bytes: 64 * 1024 * 1024, operations: 4)
    static let bitmaps = SharedArtworkBudget(bytes: 64 * 1024 * 1024, operations: Int.max)
    static let assetLimit = 15 * 1024 * 1024
    private let lock = NSLock()
    private let limit: Int
    private let operationLimit: Int
    private var used = 0
    private var active = 0
    init(bytes: Int, operations: Int) { limit = bytes; operationLimit = operations }
    func acquire(_ bytes: Int) throws -> Lease {
        lock.lock(); defer { lock.unlock() }
        guard bytes > 0, bytes <= limit, active < operationLimit, used <= limit - bytes else { throw SharedArtworkError.capacity }
        used += bytes; active += 1; return Lease(owner: self, bytes: bytes)
    }
    private func release(_ bytes: Int) { lock.lock(); used -= bytes; active -= 1; lock.unlock() }
    var retainedBytes: Int { lock.lock(); defer { lock.unlock() }; return used }
    final class Lease: @unchecked Sendable {
        private let owner: SharedArtworkBudget
        let bytes: Int
        fileprivate init(owner: SharedArtworkBudget, bytes: Int) { self.owner = owner; self.bytes = bytes }
        deinit { owner.release(bytes) }
    }
}

final class SharedArtworkBytes: Sendable {
    fileprivate let data: Data
    let mime: String
    var length: Int { data.count }
    // Admission survives fetch completion and remains held during decoding.
    private let admission: SharedArtworkBudget.Lease
    init(data: Data, mime: String, admission: SharedArtworkBudget.Lease) { self.data = data; self.mime = mime; self.admission = admission }
}

/// This allocation is owned by CGDataProvider, including any retained image or
/// SwiftUI consumer. Removing a row cannot return credits while it holds pixels.
#if canImport(UIKit)
private final class SharedArtworkPixels {
    let pointer: UnsafeMutableRawPointer
    let lease: SharedArtworkBudget.Lease
    init(bytes: Int) throws {
        lease = try SharedArtworkBudget.bitmaps.acquire(bytes)
        pointer = UnsafeMutableRawPointer.allocate(byteCount: bytes, alignment: 64)
    }
    deinit { pointer.deallocate() }
}
func sharedArtworkThumbnail(_ bytes: SharedArtworkBytes, maximum: Int) throws -> UIImage {
    guard [300, 780].contains(maximum), bytes.data.count <= SharedArtworkBudget.assetLimit,
          ["image/png", "image/jpeg", "image/webp"].contains(bytes.mime) else { throw APIError.badURL }
    let workspace = try SharedArtworkBudget.bitmaps.acquire(maximum * maximum * 8)
    defer { withExtendedLifetime(workspace) {} }
    return try autoreleasepool {
        guard let source = CGImageSourceCreateWithData(bytes.data as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
              CGImageSourceGetCount(source) == 1,
              let type = CGImageSourceGetType(source) as String?,
              type == ["image/png": UTType.png.identifier, "image/jpeg": UTType.jpeg.identifier, "image/webp": UTType.webP.identifier][bytes.mime],
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int,
              width > 0, height > 0, width <= 8192, height <= 8192, width <= 16_777_216 / height else { throw APIError.badURL }
        // Reserve decoder workspace before ImageIO can materialize pixels.
        let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true, kCGImageSourceThumbnailMaxPixelSize: maximum,
            kCGImageSourceShouldCacheImmediately: true]
        guard let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary),
              thumbnail.width <= maximum, thumbnail.height <= maximum else { throw APIError.badURL }
        let row = ((thumbnail.width * 4 + 63) / 64) * 64
        let size = row * thumbnail.height
        let pixels = try SharedArtworkPixels(bytes: size)
        let info = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
        guard let context = CGContext(data: pixels.pointer, width: thumbnail.width, height: thumbnail.height,
                                      bitsPerComponent: 8, bytesPerRow: row, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info.rawValue) else { throw APIError.badURL }
        context.draw(thumbnail, in: CGRect(x: 0, y: 0, width: thumbnail.width, height: thumbnail.height))
        let retained = Unmanaged.passRetained(pixels)
        guard let provider = CGDataProvider(dataInfo: retained.toOpaque(), data: pixels.pointer, size: size,
            releaseData: { info, _, _ in if let info { Unmanaged<SharedArtworkPixels>.fromOpaque(info).release() } }) else { retained.release(); throw APIError.badURL }
        guard let image = CGImage(width: thumbnail.width, height: thumbnail.height, bitsPerComponent: 8, bitsPerPixel: 32,
                                  bytesPerRow: row, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info, provider: provider,
                                  decode: nil, shouldInterpolate: true, intent: .defaultIntent) else { throw APIError.badURL }
        return UIImage(cgImage: image)
    }
}

#endif

/// Streaming operation retains admission through actual task/body retirement.
final class SharedArtworkReadOperation: NSObject, URLSessionDataDelegate {
    private let request: URLRequest
    private let configuration: URLSessionConfiguration
    private let current: () throws -> Void
    private let admission: SharedArtworkBudget.Lease
    private let lock = NSLock()
    private var data = Data()
    private var mime = ""
    private var completed = false
    private var continuation: CheckedContinuation<(Data, String), Error>?
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var observer: UUID?
    init(plan: SharedArtworkRequest) {
        request = plan.request; configuration = plan.configuration; admission = plan.admission; current = plan.current
        super.init(); data.reserveCapacity(SharedArtworkBudget.assetLimit)
    }
    func read() async throws -> (Data, String) {
        try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { continuation in start(continuation) }
        }, onCancel: { [weak self] in self?.finish(.failure(CancellationError())) })
    }
    private func start(_ continuation: CheckedContinuation<(Data, String), Error>) {
        lock.lock()
        if completed { lock.unlock(); continuation.resume(throwing: CancellationError()); return }
        self.continuation = continuation; lock.unlock()
        do { try current() } catch { finish(.failure(error)); return }
        let config = configuration.copy() as! URLSessionConfiguration
        config.urlCache = nil; config.urlCredentialStorage = nil; config.httpCookieStorage = nil; config.httpShouldSetCookies = false
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        let queue = OperationQueue(); queue.maxConcurrentOperationCount = 1
        let session = URLSession(configuration: config, delegate: self, delegateQueue: queue)
        let task = session.dataTask(with: request)
        let registration = Session.shared.observeAuthorizationChanges { [weak self] _ in self?.finish(.failure(APIError.badURL)) }
        lock.lock()
        if completed { lock.unlock(); Session.shared.removeAuthorizationObserver(registration.id); session.invalidateAndCancel(); return }
        self.session = session; self.task = task; observer = registration.id; lock.unlock()
        do { try current(); task.resume() } catch { finish(.failure(error)) }
    }
    private func finish(_ result: Result<(Data, String), Error>) {
        lock.lock()
        guard !completed else { lock.unlock(); return }
        completed = true
        let continuation = self.continuation; self.continuation = nil
        let task = self.task; self.task = nil
        let session = self.session; self.session = nil
        let observer = self.observer; self.observer = nil
        data = Data(); lock.unlock()
        if let observer { Session.shared.removeAuthorizationObserver(observer) }
        task?.cancel(); session?.invalidateAndCancel(); continuation?.resume(with: result)
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil); finish(.failure(APIError.badURL))
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        do {
            try current()
            guard let response = response as? HTTPURLResponse, response.url == request.url else { throw APIError.badURL }
            guard response.statusCode == 200 else { throw APIError.http(response.statusCode) }
            guard response.expectedContentLength < 0 || response.expectedContentLength <= SharedArtworkBudget.assetLimit else { throw APIError.badURL }
            let type = (response.value(forHTTPHeaderField: "Content-Type") ?? "").split(separator: ";", maxSplits: 1).first?.trimmingCharacters(in: .whitespaces).lowercased() ?? ""
            guard ["image/png", "image/jpeg", "image/webp"].contains(type) else { throw APIError.badURL }
            lock.lock(); mime = type; let done = completed; lock.unlock()
            completionHandler(done ? .cancel : .allow)
        } catch { completionHandler(.cancel); finish(.failure(error)) }
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive bytes: Data) {
        do { try current() } catch { finish(.failure(error)); return }
        lock.lock(); guard !completed else { lock.unlock(); return }
        guard bytes.count <= SharedArtworkBudget.assetLimit - data.count else { lock.unlock(); finish(.failure(APIError.badURL)); return }
        data.append(bytes); lock.unlock()
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error { finish(.failure(error)); return }
        do { try current() } catch { finish(.failure(error)); return }
        lock.lock(); let result = (data, mime); let valid = !data.isEmpty && !mime.isEmpty; lock.unlock()
        if valid { finish(.success(result)) } else { finish(.failure(APIError.badURL)) }
    }
}

#if canImport(UIKit)
@MainActor
final class SharedArtworkImageState: ObservableObject {
    @Published private var storedImage: UIImage?
    private var subject: SharedArtworkSubject?
    private var revision = UUID()
    private var observer: UUID?
    private var task: Task<UIImage, Error>?
    var image: UIImage? { guard let subject, (try? subject.requireCurrent()) != nil else { return nil }; return storedImage }
    func stop() {
        revision = UUID(); task?.cancel(); task = nil
        if let observer { Session.shared.removeAuthorizationObserver(observer) }
        observer = nil; storedImage = nil; subject = nil
    }
    func load(_ subject: SharedArtworkSubject?, backdrop: Bool) async {
        stop()
        guard let subject, let descriptor = try? subject.descriptor(backdrop: backdrop) else { return }
        self.subject = subject
        let expected = revision
        let registration = Session.shared.observeAuthorizationChanges { [weak self] _ in
            Task { @MainActor [weak self] in if self?.revision == expected { self?.stop() } }
        }
        observer = registration.id
        guard registration.generation == subject.generation, (try? subject.requireCurrent()) != nil else { stop(); return }
        let work = Task.detached(priority: .utility) {
            let bytes = try await subject.read(descriptor)
            try Task.checkCancellation(); try subject.requireCurrent()
            let image = try sharedArtworkThumbnail(bytes, maximum: backdrop ? 780 : 300)
            try Task.checkCancellation(); try subject.requireCurrent(); return image
        }
        task = work
        do {
            let image = try await withTaskCancellationHandler(operation: { try await work.value }, onCancel: { work.cancel() })
            guard revision == expected, !Task.isCancelled, (try? subject.requireCurrent()) != nil else { return }
            storedImage = image
        } catch { if revision == expected { storedImage = nil } }
        if revision == expected { task = nil }
    }
    deinit { task?.cancel(); if let observer { Session.shared.removeAuthorizationObserver(observer) } }
}

struct SharedArtworkImage: View {
    let subject: SharedArtworkSubject?
    var backdrop = false
    @StateObject private var state = SharedArtworkImageState()
    private var identity: String {
        let descriptor = try? subject?.descriptor(backdrop: backdrop)
        return (subject?.key ?? "missing") + "|" + (descriptor?.url ?? "missing")
    }
    var body: some View {
        Group {
            if let image = state.image { Image(uiImage: image).resizable().aspectRatio(contentMode: backdrop ? .fit : .fill) }
            else { Image(systemName: backdrop ? "photo" : "film").foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity) }
        }
        .clipped().accessibilityHidden(true)
        .task(id: identity) { await state.load(subject, backdrop: backdrop) }
        .onDisappear { state.stop() }
    }
}
#endif
