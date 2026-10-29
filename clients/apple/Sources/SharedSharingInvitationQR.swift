import SwiftUI
import CoreImage.CIFilterBuiltins
import ImageIO
import UniformTypeIdentifiers
#if os(iOS)
import CoreTransferable
import AVFoundation
#endif

/// QR transport contains the exact invitation, with no URL or network lookup.
enum SharedSharingInvitationQR {
    static let maximumQRBytes = 2_331 // Version 40, byte mode, M correction.
    static let maximumImageBytes = 10 * 1_024 * 1_024

    static func invitation(_ text: String) throws -> String {
        let value = text
        let suffix = value.utf8.dropFirst(16)
        guard value.hasPrefix("cinema-share-v1:"), value.utf8.count <= 10_939, !suffix.isEmpty,
              suffix.allSatisfy({ byte in byte == 45 || byte == 95 || (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte) }) else {
            throw SharedSharingManagementError.invalid("That QR code does not contain a Cinema sharing invitation.")
        }
        return value
    }

    static func image(for invitation: String) -> UIImage? {
        guard (try? self.invitation(invitation)) == invitation,
              invitation.utf8.count <= maximumQRBytes else { return nil }
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(invitation.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage,
              let code = CIContext().createCGImage(output, from: output.extent) else { return nil }
        // Preserve integer modules and a four-module white quiet zone in saved images.
        let scale = 6
        let side = CGFloat((code.width + 8) * scale)
        let format = UIGraphicsImageRendererFormat(); format.scale = 1; format.opaque = true
        return UIGraphicsImageRenderer(size: CGSize(width: side, height: side), format: format).image { context in
            UIColor.white.setFill(); context.fill(CGRect(x: 0, y: 0, width: side, height: side))
            context.cgContext.interpolationQuality = .none
            UIImage(cgImage: code).draw(in: CGRect(x: 4 * scale, y: 4 * scale, width: code.width * scale, height: code.height * scale))
        }
    }

    static func decode(_ data: Data) throws -> String {
        guard !data.isEmpty, data.count <= maximumImageBytes,
              let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: 4_096,
                kCGImageSourceShouldCacheImmediately: true
              ] as CFDictionary) else {
            throw SharedSharingManagementError.invalid("Choose a QR image smaller than 10 MiB.")
        }
        guard let detector = CIDetector(ofType: CIDetectorTypeQRCode,
                                        context: CIContext(options: [.useSoftwareRenderer: true]),
                                        options: [CIDetectorAccuracy: CIDetectorAccuracyHigh]) else {
            throw SharedSharingManagementError.invalid("QR image decoding is unavailable on this device. Use invitation text instead.")
        }
        let payloads = Set(detector.features(in: CIImage(cgImage: image)).compactMap { ($0 as? CIQRCodeFeature)?.messageString })
        guard payloads.count == 1, let payload = payloads.first else {
            throw SharedSharingManagementError.invalid("Choose an image containing one sharing invitation QR code.")
        }
        return try invitation(payload)
    }

    static func read(_ url: URL) throws -> String {
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        let file = try FileHandle(forReadingFrom: url); defer { try? file.close() }
        let data = try file.read(upToCount: maximumImageBytes + 1) ?? Data()
        return try decode(data)
    }
}

#if os(iOS)
@MainActor
enum SharedSharingQRCameraPermission {
    enum Outcome: Equatable { case ready, denied, unavailable }

    /// Availability includes camera permission; request first-use access before consulting it.
    /// Both permission outcomes must be discarded if the initiating draft/request changed.
    static func prepare(status: AVAuthorizationStatus = AVCaptureDevice.authorizationStatus(for: .video),
                        request: () async -> Bool = { await AVCaptureDevice.requestAccess(for: .video) },
                        isCurrent: () -> Bool, available: () -> Bool) async -> Outcome? {
        guard isCurrent() else { return nil }
        let allowed: Bool
        switch status {
        case .notDetermined: allowed = await request()
        case .authorized: allowed = true
        default: allowed = false
        }
        guard isCurrent() else { return nil }
        guard allowed else { return .denied }
        return available() ? .ready : .unavailable
    }
}

struct SharedSharingQRDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.png] }
    let png: Data
    init(png: Data) { self.png = png }
    init(configuration: ReadConfiguration) throws { png = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: png) }
}

struct SharedSharingQRShare: Transferable {
    let png: Data
    static var transferRepresentation: some TransferRepresentation {
        DataRepresentation(exportedContentType: .png) { $0.png }
            .suggestedFileName("Cinema-sharing-invitation.png")
    }
}
#endif
