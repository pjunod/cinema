import Foundation

enum RemoteTextBounds {
    static func label(_ text: String, maximumBytes: Int = 256, fallback: String = "Untitled") -> String {
        var result = ""
        let cleaned = String(String.UnicodeScalarView(text.unicodeScalars.filter { $0.properties.generalCategory != .control }))
            .split(whereSeparator: \.isWhitespace).joined(separator: " ")
        for scalar in cleaned.unicodeScalars {
            let next = String(scalar)
            if result.utf8.count + next.utf8.count > maximumBytes { break }
            result += next
        }
        return result.isEmpty ? fallback : result
    }
}
extension CinemaRemoteTrackKind { static let allRemoteCases: [Self] = [.audio, .subtitles, .quality] }
