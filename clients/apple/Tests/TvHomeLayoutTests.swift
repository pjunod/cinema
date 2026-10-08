import XCTest
import SwiftUI
@testable import plurx

final class TvHomeLayoutTests: XCTestCase {
    #if os(tvOS)
    @MainActor
    func testTheaterHeroKeepsLongCopyWithinTelevisionViewport() throws {
        let item = Item(
            id: 7, kind: "episode", title: "The last train through the city",
            year: 2026,
            overview: "A blackout traps a fixer and a stranger on the same stalled train; by dawn only one story survives. Together they follow a signal through the sleeping city.",
            seasonNumber: 2, episodeNumber: 7, showTitle: "Signal Lost",
            runtimeMs: 3_600_000, resolution: 2160,
            watch: Watch(positionMs: 900_000, watched: false)
        )
        let renderer = ImageRenderer(content:
            TvTheaterHero(feature: TvTheaterFeature(item: item, isContinueWatching: true))
                .frame(width: 1920)
        )
        let image = try XCTUnwrap(renderer.uiImage)
        XCTAssertEqual(image.size.width, 1920)
        XCTAssertLessThanOrEqual(image.size.height, 600, "Leave room for the first shelf below the hero")
        let attachment = XCTAttachment(image: image)
        attachment.name = "Apple TV Theater — missing artwork"
        attachment.lifetime = .keepAlways
        add(attachment)
    }
    #endif

    func testClassicRemainsDefaultAndTheaterPersistsIndependently() throws {
        let suite = "TvHomeLayoutTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let settings = SettingsStore(defaults: defaults)
        XCTAssertEqual(settings.tvHomeLayout, .classic)
        settings.tvHomeLayout = .theater
        XCTAssertEqual(SettingsStore(defaults: defaults).tvHomeLayout, .theater)
        settings.liveTvLayout = .guidePreview
        settings.libraryGrouping = .category
        settings.theme = .noirr
        XCTAssertEqual(SettingsStore(defaults: defaults).tvHomeLayout, .theater)
        settings.tvHomeLayout = .classic
        XCTAssertEqual(SettingsStore(defaults: defaults).tvHomeLayout, .classic)
        defaults.set("future-layout", forKey: "plurx.tvHomeLayout")
        XCTAssertEqual(SettingsStore(defaults: defaults).tvHomeLayout, .classic)
    }

    func testTheaterFeaturesVideoWithoutDroppingOtherContinueWatchingItems() throws {
        let book = Item(id: 1, kind: "audiobook", title: "Book")
        let episode = Item(id: 2, kind: "episode", title: "Episode")
        let movie = Item(id: 3, kind: "movie", title: "Movie")
        let items = [book, episode, movie]
        let feature = try XCTUnwrap(TvHomeLayoutPolicy.feature(
            layout: .theater, continueWatching: items, recentlyAdded: [movie]
        ))
        XCTAssertEqual(feature.item.id, episode.id)
        XCTAssertTrue(feature.isContinueWatching)
        XCTAssertEqual(TvHomeLayoutPolicy.shelfItems(items, feature: feature).map(\.id), [1, 3])
        let classic = TvHomeLayoutPolicy.feature(
            layout: .classic, continueWatching: items, recentlyAdded: [movie]
        )
        XCTAssertNil(classic)
        XCTAssertEqual(TvHomeLayoutPolicy.shelfItems(items, feature: classic).map(\.id), [1, 2, 3])
    }

    func testTheaterFallsBackToRecentVideoAndHandlesLibrariesWithoutVideo() throws {
        let containers = ["show", "season", "photo", "folder", "book", "audiobook"].enumerated().map {
            Item(id: $0.offset, kind: $0.element, title: $0.element)
        }
        let video = Item(id: 9, kind: "video", title: "Home movie")
        let feature = try XCTUnwrap(TvHomeLayoutPolicy.feature(
            layout: .theater, continueWatching: containers, recentlyAdded: containers + [video]
        ))
        XCTAssertEqual(feature.item.id, 9)
        XCTAssertFalse(feature.isContinueWatching)
        XCTAssertEqual(feature.eyebrow, "New in your library")
        XCTAssertEqual(TvHomeLayoutPolicy.shelfItems(containers, feature: feature), containers)
        XCTAssertNil(TvHomeLayoutPolicy.feature(
            layout: .theater, continueWatching: containers, recentlyAdded: containers
        ))
        XCTAssertNil(TvHomeLayoutPolicy.feature(
            layout: .theater, continueWatching: [], recentlyAdded: []
        ))
    }

    @MainActor
    func testTheaterPlaybackSkipsUnavailableFilesAndRejectsUnavailableTitles() throws {
        let item = Item(id: 7, kind: "movie", title: "Movie")
        let unavailable = MediaFile(id: 41, available: false)
        let available = MediaFile(id: 42, available: true)
        let detail = ItemDetail(item: item, files: [unavailable, available])
        XCTAssertEqual(TvHomeLayoutPolicy.playbackContext(detail)?.fileId, 42)
        XCTAssertNil(TvHomeLayoutPolicy.playbackContext(
            ItemDetail(item: item, files: [unavailable])
        ))
        // Older servers can omit availability; unknown is not unavailable.
        XCTAssertEqual(TvHomeLayoutPolicy.playbackContext(
            ItemDetail(item: item, files: [MediaFile(id: 43)])
        )?.fileId, 43)
    }

    @MainActor
    func testTheaterPlaybackUsesFreshDetailAndSharedResumeThresholds() throws {
        var item = Item(id: 7, kind: "episode", title: "Episode", showTitle: "Series")
        item.watch = Watch(positionMs: 120_000, watched: false)
        var file = MediaFile(id: 42)
        file.durationMs = 600_000
        var detail = ItemDetail(item: item, files: [file])
        let context = try XCTUnwrap(TvHomeLayoutPolicy.playbackContext(detail))
        XCTAssertEqual(context.itemId, 7)
        XCTAssertEqual(context.fileId, 42)
        XCTAssertEqual(context.startMs, 120_000)
        XCTAssertEqual(context.durationMs, 600_000)
        XCTAssertEqual(context.subtitle, "Series")
        for position in [0, 3_000, 590_000] {
            item.watch?.positionMs = position
            detail = ItemDetail(item: item, files: [file])
            XCTAssertEqual(TvHomeLayoutPolicy.playbackContext(detail)?.startMs, 0)
        }
        XCTAssertNil(TvHomeLayoutPolicy.playbackContext(ItemDetail(item: item)))
        XCTAssertNil(TvHomeLayoutPolicy.playbackContext(ItemDetail(
            item: Item(id: 8, kind: "show", title: "Series"), files: [file]
        )))
    }
}
