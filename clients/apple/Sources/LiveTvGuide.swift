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
    /// What a series rule matches on when the source supplies it. A tier that
    /// carries none leaves this nil, which is why "Record series" can also end
    /// up as a title rule locked to one channel.
    let seriesId: String?
    /// The airing's own identifier, carried for diagnosis alone: identity for
    /// scheduling is `(channel, start)` and never this.
    let programmeId: String?
    /// XMLTV's `<new/>` marker, and only that. A source that cannot say leaves
    /// it nil rather than guessing.
    let isNew: Bool?
    let filters: [String]?

    var id: String { "\(start)-\(title)" }
    var seconds: Int { max(0, end - start) }

    init(start: Int, end: Int, title: String, episodeTitle: String? = nil, episode: String? = nil,
         synopsis: String? = nil, imageUrl: String? = nil, originalAirDate: String? = nil,
         seriesId: String? = nil, programmeId: String? = nil, isNew: Bool? = nil,
         filters: [String]? = nil) {
        self.start = start
        self.end = end
        self.title = title
        self.episodeTitle = episodeTitle
        self.episode = episode
        self.synopsis = synopsis
        self.imageUrl = imageUrl
        self.originalAirDate = originalAirDate
        self.seriesId = seriesId
        self.programmeId = programmeId
        self.isNew = isNew
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
    /// The station's logo, from the same guide the cells came from
    /// (`stationLogoURL`); nil draws the callsign.
    var logo: String? = nil
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

enum LiveTvGuidePageLanding: Equatable, Sendable {
    case first
    case last
}

enum LiveTvGuideFocusMove: Equatable, Sendable {
    case focus(LiveTvGuideFocusPosition)
    /// Up from a programme cell in the first row. The grid relinquishes
    /// focus and whatever sits directly above the cells takes it — the stage's
    /// actions on the guide page, the panel's Close button over fullscreen.
    /// Named for where it used to go; the view decides where it goes now.
    case toolbar
    /// Up from the channel header in the first row: the paging chips sit in
    /// the header column directly above it, so that is where Up goes.
    case pagingChips
    /// Right past the last programme in the window. Nothing is drawn further
    /// right, so the press means "show me later" rather than nothing at all.
    case pageLater
    /// Left from the channel header. The header is the window's left edge, so
    /// the press means "show me earlier".
    case pageEarlier
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
    /// Hand focus to whatever the view has directly above the cells.
    case focusToolbar
    /// Focus the middle paging chip (Now), inside the grid's own header.
    case focusPagingChips
    /// Page the window and, once the new layout arrives, land on the first
    /// (later) or last (earlier) cell of the same channel row.
    case pageLater(channelId: String)
    case pageEarlier(channelId: String)
}

struct LiveTvFocusRestoreCoordinator: Equatable, Sendable {
    private enum Owner: Equatable, Sendable { case outside, requested, grid }

    /// Whether focus ARRIVING on a member invalidates a pending restore.
    ///
    /// The On now list says yes: it has no remote adapter, so an arrival on
    /// a row is a press the viewer made, and a restore that then wrote the
    /// row it computed before its yield would override that press.
    ///
    /// The guide grid says no: its adapter takes every press, so an arrival
    /// the coordinator did not order is the engine relocating focus after
    /// the focused view went away — the Guide pill removed under the finger
    /// that pressed it, a cell removed by a window change. Those arrivals
    /// are accidents; the requested restore is the viewer's intent and
    /// still has to run. A press inside the grid invalidates through
    /// `invalidateForNavigation` instead.
    private let arrivalInvalidates: Bool
    private var owner: Owner = .outside
    private var request = 0
    private var revision: UInt = 0

    init(arrivalInvalidates: Bool = true) {
        self.arrivalInvalidates = arrivalInvalidates
    }

    /// Begin either a new explicit entry request or a reconciliation while the
    /// grid still owns focus. A passive content refresh cannot resurrect an
    /// abandoned request.
    mutating func beginRestore(request newRequest: Int,
                               ownerRequested: Bool) -> LiveTvFocusRestoreTicket? {
        // A grid that owns focus may reconcile even if no explicit request was
        // ever made — the viewer can enter it through the engine, Down from
        // the toolbar, and then page the window from inside it.
        guard ownerRequested, newRequest > 0 || owner == .grid else { return nil }
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

    /// Focus leaving the region always invalidates a pending restore; focus
    /// arriving does so only when `arrivalInvalidates` (see there).
    mutating func focusChanged(active: Bool) {
        if active {
            owner = .grid
            if arrivalInvalidates { advanceRevision() }
            return
        }
        owner = .outside
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
    private var restoration = LiveTvFocusRestoreCoordinator(arrivalInvalidates: false)

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
        case .pagingChips:
            // The chips are inside the grid but are not the grid owning a
            // cell; the view's focus observer records that on arrival.
            return [.focusPagingChips]
        case .pageLater:
            // The grid keeps focus (on the channel header, which survives the
            // window change) and lands once the new layout is in.
            restoration.focusChanged(active: true)
            return [.pageLater(channelId: current.channelId)]
        case .pageEarlier:
            restoration.focusChanged(active: true)
            return [.pageEarlier(channelId: current.channelId)]
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
                if current.channelHeader {
                    // The header is the window's left edge; right enters the
                    // row, left asks for the earlier window.
                    if direction == .left { return .pageEarlier }
                    if let first = sorted.first {
                        return .focus(position(row.channel.id, first.programme, header: false))
                    }
                    // A row with no guide data has one placeholder cell.
                    return .focus(LiveTvGuideFocusPosition(
                        channelId: row.channel.id,
                        programmeStart: nil,
                        channelHeader: false,
                        anchorTime: current.anchorTime
                    ))
                }
                // The "No programme information" placeholder: left is the
                // header, right is the window's right edge.
                if direction == .left {
                    return .focus(LiveTvGuideFocusPosition(
                        channelId: row.channel.id,
                        programmeStart: nil,
                        channelHeader: true,
                        anchorTime: current.anchorTime
                    ))
                }
                return .pageLater
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
            guard sorted.indices.contains(next) else { return .pageLater }
            return .focus(position(row.channel.id, sorted[next].programme, header: false))

        case .up, .down:
            let nextRow = rowIndex + (direction == .down ? 1 : -1)
            if nextRow < 0 { return current.channelHeader ? .pagingChips : .toolbar }
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

    /// Where a paged window puts focus: the first cell of the row after
    /// paging later, the last after paging earlier. A row that lost its
    /// channel lands on the first row instead; a row with no data lands on its
    /// placeholder.
    static func landing(
        layout: LiveTvGridLayout,
        channelId: String,
        edge: LiveTvGuidePageLanding
    ) -> LiveTvGuideFocusPosition? {
        guard let row = layout.rows.first(where: { $0.channel.id == channelId })
            ?? layout.rows.first
        else { return nil }
        let sorted = row.cells.sorted { $0.programme.start < $1.programme.start }
        let cell = edge == .first ? sorted.first : sorted.last
        guard let cell else {
            return LiveTvGuideFocusPosition(
                channelId: row.channel.id, programmeStart: nil,
                channelHeader: false, anchorTime: nil)
        }
        return position(row.channel.id, cell.programme, header: false)
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

    /// The station's logo address, or nil for the callsign fallback.
    ///
    /// A transcription of the web's `stationLogoUrl`, checked against
    /// `tests/playback/live-tv-guide-cases.json` `station_logo`; the fixture's
    /// `rule` is the specification. The station is matched by lineup id, and
    /// the guide's own HDHomeRun artwork is the only source. The rule is
    /// textual on purpose — this is a third-party address inside a client that
    /// holds a bearer token, and "whatever Foundation's parser tolerates" is
    /// not the same set on two OS versions, let alone on three clients. The
    /// accepted string is returned with its scheme spelled `https://` and
    /// otherwise verbatim.
    static let stationLogoMaxLength = 512

    static func stationLogoURL(_ guide: LiveTvGuide?, channelId: String) -> String? {
        guard let value = channel(guide, channelId)?.imageUrl, !value.isEmpty else { return nil }
        let scalars = value.unicodeScalars
        guard scalars.count <= stationLogoMaxLength,
              scalars.allSatisfy({ (0x21...0x7E).contains($0.value) }) else { return nil }
        // Printable ASCII from here on, so one byte is one character.
        let bytes = Array(value.utf8)
        guard bytes.count >= 8,
              String(decoding: bytes[0..<8], as: UTF8.self).lowercased() == "https://" else { return nil }
        let rest = bytes[8...]
        let end = rest.firstIndex { $0 == UInt8(ascii: "/") || $0 == UInt8(ascii: "?") || $0 == UInt8(ascii: "#") }
            ?? rest.endIndex
        guard stationLogoAuthorityIsValid(Array(rest[rest.startIndex..<end])) else { return nil }
        return "https://" + String(decoding: rest, as: UTF8.self)
    }

    /// `host[:port]`: letters, digits, `.` and `-`, or a bracketed IPv6
    /// literal; the port one to five digits no greater than 65535.
    private static func stationLogoAuthorityIsValid(_ authority: [UInt8]) -> Bool {
        func isDigit(_ byte: UInt8) -> Bool { (UInt8(ascii: "0")...UInt8(ascii: "9")).contains(byte) }
        func isHex(_ byte: UInt8) -> Bool {
            isDigit(byte) || (UInt8(ascii: "a")...UInt8(ascii: "f")).contains(byte)
                || (UInt8(ascii: "A")...UInt8(ascii: "F")).contains(byte)
        }
        func isHostByte(_ byte: UInt8) -> Bool {
            isDigit(byte) || (UInt8(ascii: "a")...UInt8(ascii: "z")).contains(byte)
                || (UInt8(ascii: "A")...UInt8(ascii: "Z")).contains(byte)
                || byte == UInt8(ascii: ".") || byte == UInt8(ascii: "-")
        }
        let hostEnd: Int
        if authority.first == UInt8(ascii: "[") {
            guard let close = authority.firstIndex(of: UInt8(ascii: "]")), close > 1,
                  authority[1..<close].allSatisfy({ isHex($0) || $0 == UInt8(ascii: ":") || $0 == UInt8(ascii: ".") })
            else { return false }
            hostEnd = close + 1
        } else {
            hostEnd = authority.firstIndex(of: UInt8(ascii: ":")) ?? authority.count
            guard hostEnd > 0, authority[0..<hostEnd].allSatisfy(isHostByte) else { return false }
        }
        if hostEnd == authority.count { return true }
        guard authority[hostEnd] == UInt8(ascii: ":") else { return false }
        let port = authority[(hostEnd + 1)...]
        guard (1...5).contains(port.count), port.allSatisfy(isDigit),
              let number = Int(String(decoding: port, as: UTF8.self)) else { return false }
        return number <= 65535
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
            return LiveTvGridRow(channel: channel, cells: cells,
                                 logo: stationLogoURL(guide, channelId: channel.id))
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
