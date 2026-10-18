import SwiftUI

enum TvHomeLayout: String, CaseIterable, Identifiable {
    case classic
    case theater

    var id: String { rawValue }
    var label: String { self == .classic ? "Classic" : "Theater" }
}

struct TvTheaterFeature {
    let item: Item
    let isContinueWatching: Bool

    var eyebrow: String { isContinueWatching ? "Continue watching" : "New in your library" }
}

enum TvHomeLayoutPolicy {
    /// Match Theater on the web: preserve hub ordering and feature only video.
    /// Next Up, books, photos, and show/season containers stay on their shelves.
    static func feature(
        layout: TvHomeLayout, continueWatching: [Item], recentlyAdded: [Item]
    ) -> TvTheaterFeature? {
        guard layout == .theater else { return nil }
        let video: (Item) -> Bool = { $0.isMovieOrEpisode || $0.kind == "video" }
        if let item = continueWatching.first(where: video) {
            return TvTheaterFeature(item: item, isContinueWatching: true)
        }
        return recentlyAdded.first(where: video).map {
            TvTheaterFeature(item: $0, isContinueWatching: false)
        }
    }

    static func shelfItems(_ items: [Item], feature: TvTheaterFeature?) -> [Item] {
        guard let feature, feature.isContinueWatching else { return items }
        return items.filter { $0.id != feature.item.id }
    }

    @MainActor
    static func playbackContext(_ detail: ItemDetail) -> PlayContext? {
        let item = detail.item
        guard item.isMovieOrEpisode || item.kind == "video" else { return nil }
        let position = item.watch?.positionMs ?? 0
        guard let file = detail.files?.first(where: { $0.available != false }) else { return nil }
        let duration = file.durationMs ?? item.runtimeMs ?? 0
        return PlayContext(
            itemId: item.id, fileId: file.id,
            startMs: AppModel.resumableStartMs(positionMs: position, durationMs: duration),
            durationMs: duration, title: item.title, subtitle: item.showTitle,
            year: item.year, airDate: item.airDate, overview: item.overview
        )
    }
}

@MainActor
final class HomeFeaturePresentation: ObservableObject {
    @Published var play: PlayContext?
    @Published var requestedItemID: Int?
    @Published var error: String?
}

@MainActor
struct TvTheaterHero: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    let feature: TvTheaterFeature
    @ObservedObject var presentation: HomeFeaturePresentation = HomeFeaturePresentation()

    private var standardTheater: Bool {
        #if os(tvOS)
        return model.layout == .standard
        #else
        return false
        #endif
    }
    private var item: Item { feature.item }
    private var resumeMs: Int {
        AppModel.resumableStartMs(
            positionMs: item.watch?.positionMs ?? 0, durationMs: item.runtimeMs ?? 0
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(feature.eyebrow.uppercased())
                .font(.system(size: 18, weight: .bold, design: .monospaced))
                .tracking(3)
                .foregroundStyle(Palette.accent)
            Text(item.showTitle ?? item.title)
                .font(standardTheater ? .system(size: 52, weight: .bold) : .largeTitle.bold())
                .foregroundStyle(standardTheater ? .white : Palette.heroInk)
                .lineLimit(2)
            if item.showTitle != nil {
                Text(item.title)
                    .font(.title3)
                    .foregroundStyle(standardTheater ? .white.opacity(0.85) : Palette.heroInk)
                    .lineLimit(1)
            }
            if !metadata.isEmpty {
                Text(metadata)
                    .font(.system(.callout, design: .monospaced))
                    .foregroundStyle(standardTheater ? .white.opacity(0.8) : Palette.heroInk)
                    .lineLimit(1)
            }
            if let overview = item.overview, !overview.isEmpty {
                Text(overview)
                    .font(.callout)
                    .foregroundStyle(standardTheater ? .white.opacity(0.9) : Palette.heroInk)
                    .lineLimit(3)
            }
            Group {
                if !standardTheater && dynamicTypeSize.isAccessibilitySize {
                    VStack(alignment: .leading, spacing: 16) { actionButtons }
                } else { HStack(spacing: 24) { actionButtons } }
            }
            .padding(.top, 12)
        }
        .frame(maxWidth: 900, alignment: .leading)
        .padding(.horizontal, screenHPad)
        .padding(.vertical, 36)
        .frame(maxWidth: .infinity, minHeight: standardTheater ? 540 : 360, alignment: .leading)
        .background {
            GeometryReader { geometry in
                ZStack {
                    standardTheater ? Color.black : Palette.heroScrim
                    Palette.accent.opacity(0.18)
                    if let backdrop = item.backdrop {
                        AuthImage(path: backdrop, targetSize: geometry.size)
                            .frame(width: geometry.size.width, height: geometry.size.height)
                            .clipped()
                    }
                    LinearGradient(
                        colors: standardTheater ? [.black.opacity(0.88), .black.opacity(0.55), .clear]
                            : [Palette.heroScrim.opacity(0.97), Palette.heroScrim.opacity(0.93), Palette.heroScrim.opacity(0.7)],
                        startPoint: .leading, endPoint: .trailing
                    )
                    LinearGradient(
                        colors: [.clear, standardTheater ? .black.opacity(0.65) : Palette.heroScrim.opacity(0.95)],
                        startPoint: .center, endPoint: .bottom
                    )
                }
            }
            .accessibilityHidden(true)
        }
        .tvNavigationFocusSection()
    }

    @ViewBuilder private var actionButtons: some View {
        #if os(tvOS)
        if standardTheater {
            PrimaryButton(
                title: resumeMs > 0 ? "▶  Resume · \(formatTime(resumeMs))" : "▶  Play",
                busy: presentation.requestedItemID != nil
            ) { presentation.requestedItemID = item.id }
            .accessibilityIdentifier("theater-play")
            .remoteControl("home-feature:play", label: resumeMs > 0 ? "Resume" : "Play") { presentation.requestedItemID = item.id }
            NavigationLink(value: Route.item(item.id)) { Text("Details") }
                .buttonStyle(TVReadableButtonStyle(prominent: false))
                .accessibilityIdentifier("theater-details")
                .remoteControl("home-feature:details", label: "Details") { navigation.navigate(to: .item(item.id)) }
        } else { meridianActionButtons }
        #else
        meridianActionButtons
        #endif
    }

    @ViewBuilder private var meridianActionButtons: some View {
                Button { presentation.requestedItemID = item.id } label: {
                    HStack {
                        if presentation.requestedItemID != nil { ProgressView() }
                        Label(resumeMs > 0 ? "Resume · \(formatTime(resumeMs))" : "Play", systemImage: "play.fill")
                    }
                    .font(.headline).padding(.horizontal, 24).frame(minHeight: 48)
                    .foregroundStyle(Palette.buttonInk)
                    .background(Palette.accent, in: Capsule())
                }
                .disabled(presentation.requestedItemID != nil)
                #if os(tvOS)
                .buttonStyle(TVReadableButtonStyle(prominent: true))
                #else
                .buttonStyle(.plain)
                #endif
                .accessibilityIdentifier("theater-play")
                .remoteControl("home-feature:play", label: resumeMs > 0 ? "Resume" : "Play") { presentation.requestedItemID = item.id }
                Button("Details") { navigation.navigate(to: .item(item.id)) }
                .remoteControl("home-feature:details", label: "Details") { navigation.navigate(to: .item(item.id)) }
                #if os(tvOS)
                .buttonStyle(TVReadableButtonStyle(prominent: false))
                #endif
                .accessibilityIdentifier("theater-details")
    }

    private var metadata: String {
        #if os(tvOS)
        return DetailView.tvPlayableMetadata(item, file: nil, durationMs: item.runtimeMs)
        #else
        return [item.year.map(String.init), item.runtimeMs.map(DetailView.compactRuntimeLabel)].compactMap { $0 }.joined(separator: " · ")
        #endif
    }
}
