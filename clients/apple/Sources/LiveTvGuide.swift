import Foundation

// The programme guide, as the client sees it.
//
// Everything below the models is a pure function over a decoded guide, in the
// same shape as `PlayerInputRouting`: a transcription that the test bundle
// checks against `tests/playback/live-tv-guide-cases.json`, never a reader of
// that file. The web renderer answers the same cases from the same fixture, so
// "what is on now" cannot mean two different things on two clients.

struct LiveTvProgramme: Codable, Equatable, Sendable, Identifiable {
    let start: Int
    let end: Int
    let title: String
    let episodeTitle: String?
    let episode: String?
    let synopsis: String?
    let imageUrl: String?
    let originalAirDate: String?
    let filters: [String]?

    var id: String { "\(start)-\(title)" }
    var seconds: Int { max(0, end - start) }

    init(start: Int, end: Int, title: String, episodeTitle: String? = nil, episode: String? = nil,
         synopsis: String? = nil, imageUrl: String? = nil, originalAirDate: String? = nil,
         filters: [String]? = nil) {
        self.start = start
        self.end = end
        self.title = title
        self.episodeTitle = episodeTitle
        self.episode = episode
        self.synopsis = synopsis
        self.imageUrl = imageUrl
        self.originalAirDate = originalAirDate
        self.filters = filters
    }
}

struct LiveTvGuideChannel: Codable, Equatable, Sendable {
    let id: String
    let guideNumber: String
    let affiliate: String?
    let imageUrl: String?
    let programmes: [LiveTvProgramme]
}

struct LiveTvGuideWindow: Codable, Equatable, Sendable {
    let start: Int
    let end: Int
}

struct LiveTvGuide: Codable, Equatable, Sendable {
    let source: String
    let freshness: String
    let ageSeconds: Int
    let fetchedAt: Int?
    /// When the owner's own refresh loop next intends to run, unix seconds.
    /// Clients poll on this rather than on a cadence of their own, so a guide
    /// that arrives a minute after a deploy is drawn a minute after a deploy.
    /// Absent from an owner whose loop has not completed a tick yet — and from
    /// one older than this contract — in which case the client falls back to
    /// the contract's floor. Served on an `unavailable` answer too: an owner
    /// with nothing to give still knows when it will have something.
    var nextRefreshAt: Int?
    let window: LiveTvGuideWindow
    let refreshError: String?
    let matchedChannels: Int?
    let lineupChannels: Int?
    let channels: [LiveTvGuideChannel]

    /// Off, empty, stale or erroring are all *rendered* states. A channel row
    /// with no programme line is the degraded page, not a failed one, and the
    /// lineup and session start never consult any of this.
    var hasProgrammes: Bool { channels.contains { !$0.programmes.isEmpty } }
}

/// What is on a channel now, what is next, and how far through we are.
struct LiveTvAiring: Equatable, Sendable {
    let now: LiveTvProgramme?
    let next: LiveTvProgramme?
    /// `nil` exactly when nothing is on — a real hole in the guide is a state
    /// to draw, not one to paper over with the programme that just ended.
    let progress: Double?

    static let none = LiveTvAiring(now: nil, next: nil, progress: nil)
}

/// One positioned cell in the half-hour grid.
struct LiveTvGridCell: Equatable, Sendable {
    let programme: LiveTvProgramme
    let left: Double
    let width: Double
    let airing: Bool
    let clipped: Bool
}

struct LiveTvGridRow: Equatable, Sendable {
    let channel: LiveTvChannel
    let cells: [LiveTvGridCell]
}

struct LiveTvGridLayout: Equatable, Sendable {
    let rows: [LiveTvGridRow]
    let totalWidth: Double
    /// `nil` when now falls outside the drawn window, so the red rule is simply
    /// not drawn rather than clamped to an edge it does not mean.
    let nowX: Double?
}

struct LiveTvChannelFilter: Equatable, Sendable {
    var query: String = ""
    var favoritesOnly: Bool = false
    var hideProtected: Bool = false
}

/// A guide focus location independent of SwiftUI. Keeping movement here makes
/// a rapid series of remote presses one ordered state transition at a time;
/// view restoration is not allowed to invent another destination later.
struct LiveTvGuideFocusPosition: Equatable, Sendable {
    let channelId: String
    let programmeStart: Int?
    let channelHeader: Bool
    let anchorTime: Int?
}

enum LiveTvGuideFocusMove: Equatable, Sendable {
    case focus(LiveTvGuideFocusPosition)
    case toolbar
    case unchanged
}

/// One focus owner arbitrates remote moves and delayed restoration. SwiftUI's
/// `.task(id:)` cancellation is cooperative, so a yielded task also carries a
/// ticket that becomes invalid as soon as newer input or another focus region
/// wins. The view applies the returned effects in order; a top-boundary move
/// therefore clears the grid before asking the toolbar to take focus.
struct LiveTvFocusRestoreTicket: Equatable, Sendable {
    fileprivate let request: Int
    fileprivate let revision: UInt
}

enum LiveTvGuideFocusEffect: Equatable, Sendable {
    case focus(LiveTvGuideFocusPosition)
    case clearGrid
    case focusToolbar
}

struct LiveTvFocusRestoreCoordinator: Equatable, Sendable {
    private enum Owner: Equatable, Sendable { case outside, requested, grid }

    private var owner: Owner = .outside
    private var request = 0
    private var revision: UInt = 0

    /// Begin either a new explicit entry request or a reconciliation while the
    /// grid still owns focus. A passive content refresh cannot resurrect an
    /// abandoned request.
    mutating func beginRestore(request newRequest: Int,
                               ownerRequested: Bool) -> LiveTvFocusRestoreTicket? {
        guard ownerRequested, newRequest > 0 else { return nil }
        if newRequest > request {
            request = newRequest
            owner = .requested
            advanceRevision()
        }
        guard owner != .outside, newRequest == request else { return nil }
        return LiveTvFocusRestoreTicket(request: request, revision: revision)
    }

    func permits(_ ticket: LiveTvFocusRestoreTicket, ownerRequested: Bool) -> Bool {
        ownerRequested && owner != .outside
            && ticket.request == request && ticket.revision == revision
    }

    mutating func focusChanged(active: Bool) {
        owner = active ? .grid : .outside
        advanceRevision()
    }

    mutating func leave() {
        owner = .outside
        advanceRevision()
    }

    mutating func invalidateForNavigation() {
        advanceRevision()
    }

    private mutating func advanceRevision() {
        revision &+= 1
    }
}

struct LiveTvGuideFocusCoordinator: Equatable, Sendable {
    private var restoration = LiveTvFocusRestoreCoordinator()

    mutating func beginRestore(request: Int,
                               ownerRequested: Bool) -> LiveTvFocusRestoreTicket? {
        restoration.beginRestore(request: request, ownerRequested: ownerRequested)
    }

    func permits(_ ticket: LiveTvFocusRestoreTicket, ownerRequested: Bool) -> Bool {
        restoration.permits(ticket, ownerRequested: ownerRequested)
    }

    mutating func focusChanged(active: Bool) {
        restoration.focusChanged(active: active)
    }

    mutating func leave() {
        restoration.leave()
    }

    mutating func move(layout: LiveTvGridLayout,
                       current: LiveTvGuideFocusPosition,
                       direction: LiveTvContractInput,
                       fallbackAnchor: Int) -> [LiveTvGuideFocusEffect] {
        restoration.invalidateForNavigation()
        switch LiveTvGuideFocusNavigator.move(
            layout: layout,
            current: current,
            direction: direction,
            fallbackAnchor: fallbackAnchor
        ) {
        case .focus(let next):
            restoration.focusChanged(active: true)
            return [.focus(next)]
        case .toolbar:
            restoration.leave()
            return [.clearGrid, .focusToolbar]
        case .unchanged:
            return []
        }
    }

}

enum LiveTvGuideFocusNavigator {
    static func move(
        layout: LiveTvGridLayout,
        current: LiveTvGuideFocusPosition,
        direction: LiveTvContractInput,
        fallbackAnchor: Int
    ) -> LiveTvGuideFocusMove {
        guard let rowIndex = layout.rows.firstIndex(where: { $0.channel.id == current.channelId })
        else { return .unchanged }
        let row = layout.rows[rowIndex]
        let sorted = row.cells.sorted { $0.programme.start < $1.programme.start }

        switch direction {
        case .left, .right:
            guard let start = current.programmeStart else {
                if current.channelHeader, direction == .right, let first = sorted.first {
                    return .focus(position(row.channel.id, first.programme, header: false))
                }
                if !current.channelHeader, direction == .left {
                    return .focus(LiveTvGuideFocusPosition(
                        channelId: row.channel.id,
                        programmeStart: nil,
                        channelHeader: true,
                        anchorTime: current.anchorTime
                    ))
                }
                return .unchanged
            }
            guard let index = sorted.firstIndex(where: { $0.programme.start == start }) else {
                return .unchanged
            }
            let next = index + (direction == .right ? 1 : -1)
            if next < 0 {
                return .focus(LiveTvGuideFocusPosition(
                    channelId: row.channel.id,
                    programmeStart: nil,
                    channelHeader: true,
                    anchorTime: current.anchorTime
                ))
            }
            guard sorted.indices.contains(next) else { return .unchanged }
            return .focus(position(row.channel.id, sorted[next].programme, header: false))

        case .up, .down:
            let nextRow = rowIndex + (direction == .down ? 1 : -1)
            if nextRow < 0 { return .toolbar }
            guard layout.rows.indices.contains(nextRow) else { return .unchanged }
            let targetRow = layout.rows[nextRow]
            if current.channelHeader {
                return .focus(LiveTvGuideFocusPosition(
                    channelId: targetRow.channel.id,
                    programmeStart: nil,
                    channelHeader: true,
                    anchorTime: current.anchorTime
                ))
            }
            let anchor = current.anchorTime ?? current.programmeStart ?? fallbackAnchor
            let candidate = targetRow.cells.first {
                $0.programme.start <= anchor && anchor < $0.programme.end
            } ?? targetRow.cells.min {
                abs(($0.programme.start + $0.programme.end) / 2 - anchor)
                    < abs(($1.programme.start + $1.programme.end) / 2 - anchor)
            }
            return .focus(LiveTvGuideFocusPosition(
                channelId: targetRow.channel.id,
                programmeStart: candidate?.programme.start,
                channelHeader: false,
                anchorTime: anchor
            ))

        default:
            return .unchanged
        }
    }

    private static func position(
        _ channelId: String,
        _ programme: LiveTvProgramme,
        header: Bool
    ) -> LiveTvGuideFocusPosition {
        LiveTvGuideFocusPosition(
            channelId: channelId,
            programmeStart: programme.start,
            channelHeader: header,
            anchorTime: programme.start + max(1, programme.end - programme.start) / 2
        )
    }
}

enum LiveTvGuideReducer {
    static func channel(_ guide: LiveTvGuide?, _ channelId: String) -> LiveTvGuideChannel? {
        guide?.channels.first { $0.id == channelId }
    }

    /// The start instant belongs to the programme that starts; the end instant
    /// does not. Without that rule a viewer at exactly 8:30 sees two programmes
    /// on air, and "what is on now" stops being a question with one answer.
    static func airing(_ guide: LiveTvGuide?, channelId: String, now: Int) -> LiveTvAiring {
        let rows = channel(guide, channelId)?.programmes ?? []
        var current: LiveTvProgramme?
        var next: LiveTvProgramme?
        for row in rows {
            if row.start <= now && now < row.end {
                current = row
                continue
            }
            if row.start > now, next == nil || row.start < next!.start {
                next = row
            }
        }
        guard let current, current.seconds > 0 else {
            return LiveTvAiring(now: current, next: next, progress: nil)
        }
        let progress = Double(now - current.start) / Double(current.seconds)
        return LiveTvAiring(now: current, next: next, progress: progress)
    }

    /// Positioned cells for the half-hour grid. Clips to the window, never
    /// overlaps, and reports where the red now line goes. Geometry only: what a
    /// cell looks like is the view's business.
    static func gridLayout(
        guide: LiveTvGuide?,
        channels: [LiveTvChannel],
        window: LiveTvGuideWindow,
        now: Int,
        slotSeconds: Int,
        pxPerSlot: Double
    ) -> LiveTvGridLayout {
        let slot = slotSeconds > 0 ? slotSeconds : 1800
        let px = pxPerSlot > 0 ? pxPerSlot : 240
        func scale(_ seconds: Int) -> Double {
            Double(seconds - window.start) / Double(slot) * px
        }
        let rows = channels.map { channel -> LiveTvGridRow in
            let programmes = self.channel(guide, channel.id)?.programmes ?? []
            var cells: [LiveTvGridCell] = []
            for row in programmes {
                let start = max(row.start, window.start)
                let end = min(row.end, window.end)
                guard end > start else { continue }
                cells.append(LiveTvGridCell(
                    programme: row,
                    left: scale(start),
                    width: scale(end) - scale(start),
                    airing: row.start <= now && now < row.end,
                    clipped: row.start < window.start || row.end > window.end
                ))
            }
            return LiveTvGridRow(channel: channel, cells: cells)
        }
        return LiveTvGridLayout(
            rows: rows,
            totalWidth: scale(window.end),
            nowX: now >= window.start && now <= window.end ? scale(now) : nil
        )
    }

    /// Half-hour column headings across the window.
    static func gridSlots(window: LiveTvGuideWindow, slotSeconds: Int) -> [Int] {
        let slot = slotSeconds > 0 ? slotSeconds : 1800
        var out: [Int] = []
        var at = window.start
        while at < window.end {
            out.append(at)
            at += slot
        }
        return out
    }

    /// Where the guide's data stops on a channel — the hatched marker. `nil`
    /// when it runs past the window, which is the ordinary case.
    static func guideEnds(_ guide: LiveTvGuide?, channelId: String, window: LiveTvGuideWindow) -> Int? {
        let rows = channel(guide, channelId)?.programmes ?? []
        guard let last = rows.last else { return window.start }
        return last.end >= window.end ? nil : last.end
    }

    /// Search matches the number, the callsign, and what is on now — typing
    /// what you can see on screen should find the channel showing it.
    static func filter(
        channels: [LiveTvChannel],
        guide: LiveTvGuide?,
        options: LiveTvChannelFilter,
        now: Int
    ) -> [LiveTvChannel] {
        let query = options.query.trimmingCharacters(in: .whitespacesAndNewlines)
        return channels.filter { channel in
            if options.favoritesOnly && !channel.favorite { return false }
            if options.hideProtected && !channel.watchable { return false }
            if query.isEmpty { return true }
            let title = airing(guide, channelId: channel.id, now: now).now?.title ?? ""
            let haystack = "\(channel.guideNumber) \(channel.guideName) \(title)"
            return haystack.localizedCaseInsensitiveContains(query)
        }
    }

    /// Which channel is ±1 from `current` in the visible order, wrapping. An
    /// unknown current lands on the first, so channel-up from a channel that
    /// was just filtered away still goes somewhere.
    static func adjacent(visible: [String], current: String?, delta: Int) -> String? {
        guard !visible.isEmpty else { return nil }
        guard let current, let at = visible.firstIndex(of: current) else { return visible[0] }
        let count = visible.count
        let next = ((at + delta) % count + count) % count
        return visible[next]
    }
}
