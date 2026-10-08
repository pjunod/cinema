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

#if os(tvOS)
@MainActor
struct TvTheaterHero: View {
    @EnvironmentObject private var model: AppModel
    let feature: TvTheaterFeature
    @State private var requestedItemID: Int?
    @State private var play: PlayContext?
    @State private var playbackError: String?

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
                .font(.system(size: 52, weight: .bold))
                .foregroundStyle(.white)
                .lineLimit(2)
            if item.showTitle != nil {
                Text(item.title)
                    .font(.title3)
                    .foregroundStyle(.white.opacity(0.85))
                    .lineLimit(1)
            }
            if !metadata.isEmpty {
                Text(metadata)
                    .font(.system(.callout, design: .monospaced))
                    .foregroundStyle(.white.opacity(0.8))
                    .lineLimit(1)
            }
            if let overview = item.overview, !overview.isEmpty {
                Text(overview)
                    .font(.callout)
                    .foregroundStyle(.white.opacity(0.9))
                    .lineLimit(3)
            }
            HStack(spacing: 24) {
                PrimaryButton(
                    title: resumeMs > 0 ? "▶  Resume · \(formatTime(resumeMs))" : "▶  Play",
                    busy: requestedItemID != nil
                ) {
                    requestedItemID = item.id
                }
                .accessibilityIdentifier("theater-play")
                NavigationLink(value: Route.item(item.id)) {
                    Text("Details")
                }
                .buttonStyle(TVReadableButtonStyle(prominent: false))
                .accessibilityIdentifier("theater-details")
            }
            .padding(.top, 12)
        }
        .frame(maxWidth: 900, alignment: .leading)
        .padding(.horizontal, screenHPad)
        .padding(.vertical, 36)
        .frame(maxWidth: .infinity, minHeight: 540, alignment: .leading)
        .background {
            GeometryReader { geometry in
                ZStack {
                    Color.black
                    Palette.accent.opacity(0.18)
                    if let backdrop = item.backdrop {
                        AuthImage(path: backdrop, targetSize: geometry.size)
                            .frame(width: geometry.size.width, height: geometry.size.height)
                            .clipped()
                    }
                    LinearGradient(
                        colors: [.black.opacity(0.88), .black.opacity(0.55), .clear],
                        startPoint: .leading, endPoint: .trailing
                    )
                    LinearGradient(
                        colors: [.clear, .black.opacity(0.65)],
                        startPoint: .center, endPoint: .bottom
                    )
                }
            }
            .accessibilityHidden(true)
        }
        .tvNavigationFocusSection()
        .task(id: requestedItemID) {
            guard let id = requestedItemID else { return }
            defer { requestedItemID = nil }
            do {
                let detail = try await model.itemDetail(id)
                guard !Task.isCancelled else { return }
                if let context = TvHomeLayoutPolicy.playbackContext(detail) {
                    play = context
                } else {
                    playbackError = "This title has no playable video file."
                }
            } catch {
                guard !Task.isCancelled else { return }
                playbackError = error.localizedDescription
            }
        }
        .alert("Unable to play", isPresented: Binding(
            get: { playbackError != nil }, set: { if !$0 { playbackError = nil } }
        )) {
            Button("OK", role: .cancel) { playbackError = nil }
        } message: {
            Text(playbackError ?? "")
        }
        .fullScreenCover(item: $play, onDismiss: {
            Task { await model.loadHome() }
        }) { context in
            PlayerView(
                itemId: context.itemId, fileId: context.fileId, startMs: context.startMs,
                durationMs: context.durationMs, title: context.title,
                subtitle: context.subtitle, year: context.year, airDate: context.airDate,
                overview: context.overview, onPlayNext: { play = $0 }
            )
            .id(context.id)
            .environmentObject(model)
        }
    }

    private var metadata: String {
        DetailView.tvPlayableMetadata(item, file: nil, durationMs: item.runtimeMs)
    }
}
#endif
