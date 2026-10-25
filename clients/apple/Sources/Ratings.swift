import SwiftUI

/// One review-site score: `imdb` out of 10, the others out of 100.
struct Rating: Codable, Equatable {
    var source: String
    var value: Double
    var scale: Int
    var votes: Int?
}

/// `GET items/{id}/annotations`: scores and the reader's list flags.
struct ItemAnnotations: Codable, Equatable {
    var ratings: [Rating]?
    var watchlist: Bool?
    var favorite: Bool?
}

/// `GET lists/{list}`: the reader's watchlist or favorites, newest first.
struct ItemListResponse: Codable {
    var items: [Item]?
}

enum RatingFormat {
    static func name(_ source: String, short: Bool = false) -> String {
        switch source {
        case "imdb": return "IMDb"
        case "rotten_tomatoes": return short ? "RT" : "Rotten Tomatoes"
        case "metacritic": return "Metacritic"
        case "tmdb": return "TMDB"
        default: return source
        }
    }

    /// IMDb out of ten with one decimal, Metacritic as a whole number, the
    /// rest as a percentage.
    static func text(_ rating: Rating) -> String {
        if rating.scale == 10 { return String(format: "%.1f", rating.value) }
        let n = Int(rating.value.rounded())
        return rating.source == "metacritic" ? "\(n)" : "\(n)%"
    }

    /// A Rotten Tomatoes score of 60% or more is a positive critics' verdict.
    static func rottenTomatoesPositive(_ value: Double) -> Bool { value >= 60 }

    static func accessibility(_ rating: Rating) -> String {
        "\(name(rating.source)) \(text(rating))" + (rating.votes.map { ", from \($0) votes" } ?? "")
    }
}

/// The one place a score source's mark is drawn. Original glyphs keyed by
/// source; change a source's artwork here and every rating in the app
/// follows, without touching any layout.
struct RatingMark: View {
    let source: String
    let value: Double
    var size: CGFloat = 18

    var body: some View {
        switch source {
        case "imdb":
            Image(systemName: "star.fill")
                .resizable().scaledToFit()
                .foregroundStyle(Color(red: 0.984, green: 0.749, blue: 0.141))
                .frame(width: size, height: size)
        case "rotten_tomatoes":
            let positive = RatingFormat.rottenTomatoesPositive(value)
            ZStack {
                Circle().fill(positive ? Color(red: 0.937, green: 0.267, blue: 0.267) : Color(red: 0.396, green: 0.639, blue: 0.051))
                Image(systemName: positive ? "checkmark" : "xmark")
                    .font(.system(size: size * 0.5, weight: .heavy))
                    .foregroundStyle(.white)
            }
            .frame(width: size, height: size)
        case "metacritic":
            let band: Color = value >= 61 ? Color(red: 0.133, green: 0.773, blue: 0.369)
                : value >= 40 ? Color(red: 0.984, green: 0.749, blue: 0.141)
                : Color(red: 0.937, green: 0.267, blue: 0.267)
            Text("\(Int(value.rounded()))")
                .font(.system(size: size * 0.45, weight: .heavy))
                .foregroundStyle(Color(red: 0.027, green: 0.063, blue: 0.122))
                .frame(width: size, height: size)
                .background(RoundedRectangle(cornerRadius: size / 4).fill(band))
        case "tmdb":
            Image(systemName: "person.2.fill")
                .resizable().scaledToFit()
                .foregroundStyle(Color(red: 0.431, green: 0.659, blue: 0.996))
                .frame(width: size, height: size)
        default:
            EmptyView()
        }
    }
}

/// One score: its mark, the score, and (where there is room) the source name.
struct RatingChip: View {
    let rating: Rating
    var showsName = true

    var body: some View {
        HStack(spacing: 6) {
            RatingMark(source: rating.source, value: rating.value)
            Text(RatingFormat.text(rating)).font(.subheadline.weight(.bold))
            if showsName {
                Text(RatingFormat.name(rating.source, short: true))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(RatingFormat.accessibility(rating))
    }
}
