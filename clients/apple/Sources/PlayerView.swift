import SwiftUI

#if os(iOS)
import UIKit
#endif

enum PlayerControl: Hashable {
    case reveal
    case close
    case retry
    case progress
    case marker
    case skipBack
    case playPause
    case skipForward
    case pictureInPicture
    case audio
    case subtitles
    case quality
    case autoplay
    case stats
}

#if os(iOS)
private enum PlayerOptionMenu: Hashable {
    case audio
    case subtitles
    case quality
    case more
}

enum PlayerOptionMenuPalette {
    /// UIKit semantic colors are intentional here. `foregroundStyle(.primary)`
    /// would select the primary level of the player's inherited white style,
    /// leaving white labels on a light popover.
    static let foreground = UIColor.label
    static let secondaryForeground = UIColor.secondaryLabel
}

private struct PlayerOptionMenuButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
            .padding(.vertical, 9)
            .background(
                configuration.isPressed ? Color.primary.opacity(0.12) : Color.clear,
                in: RoundedRectangle(cornerRadius: 7)
            )
            .contentShape(Rectangle())
    }
}
#endif

enum PlayerSeekDirection: CaseIterable {
    case left
    case right
    case up
    case down

    var seconds: Double {
        switch self {
        case .left: return -10
        case .right: return 10
        case .up: return 30
        case .down: return -30
        }
    }
}

enum PlayerRemoteMoveOutcome: Equatable {
    case seek(seconds: Double)
    case focus(PlayerControl)
    case ignore
}

enum TVPlayerRemoteRouting {
    static func moveOutcome(
        focusedControl: PlayerControl,
        progressEngaged: Bool,
        direction: PlayerSeekDirection,
        progressRightNeighbor: PlayerControl = .autoplay,
        markerAvailable: Bool = false
    ) -> PlayerRemoteMoveOutcome {
        switch focusedControl {
        case .reveal:
            return .seek(seconds: direction.seconds)
        case .progress:
            switch direction {
            case .left:
                return progressEngaged
                    ? .seek(seconds: PlayerSeekDirection.left.seconds)
                    : .focus(.skipForward)
            case .right:
                return progressEngaged
                    ? .seek(seconds: PlayerSeekDirection.right.seconds)
                    : .focus(progressRightNeighbor)
            case .down:
                return .focus(.playPause)
            case .up:
                return markerAvailable ? .focus(.marker) : .ignore
            }
        default:
            return .ignore
        }
    }
}

struct PlayerMetadataBadge: Equatable, Identifiable {
    enum Kind: String, Equatable {
        case resolution
        case dynamicRange
        case audio
    }

    /// Mirrors the web player's `.res`, `.hdr`, `.dv`, and `.audio` badge
    /// classes. Keeping this semantic (instead of storing a `Color`) makes the
    /// badge contract testable and lets resolution continue following the
    /// viewer's selected accent palette.
    enum Tone: String, Equatable {
        case resolution
        case hdr
        case dolbyVision
        case audio
    }

    let kind: Kind
    let tone: Tone
    /// The source grade (or the whole terse label for non-HDR badges).
    let mark: String?
    let accessibilityLabel: String
    /// The grade actually rendering when it differs from `mark`. Keeping this
    /// separate lets the badge dim the unavailable source capability while
    /// leaving the functioning result (`→ HDR10`) fully lit, like the web UI.
    var renderedMark: String? = nil
    /// Only the icon/source half is subdued when `renderedMark` is present.
    var dimmed: Bool = false

    var displayMark: String? {
        guard let renderedMark else { return mark }
        return [mark, "→ \(renderedMark)"].compactMap { $0 }.joined(separator: " ")
    }

    /// Detail-page metadata still uses the native SF Symbol row. Playback uses
    /// `PlayerMetadataBadgeIcon` instead, so this compatibility mapping does
    /// not leak the old glyphs back into the player.
    var symbol: String {
        switch kind {
        case .resolution: return "tv.fill"
        case .dynamicRange: return "sparkles"
        case .audio: return "waveform"
        }
    }

    var id: String { kind.rawValue }
}

enum PlayerMetadataBadgeMetrics {
    static let rowSpacing: CGFloat = 5
    static let contentSpacing: CGFloat = 4
    static let horizontalPadding: CGFloat = 6
    static let verticalPadding: CGFloat = 2
    static let strokeWidth: CGFloat = 1
    /// Readable, but plainly the "off" treatment next to a lit chip.
    static let dimmedOpacity: Double = 0.45

    #if os(tvOS)
    static let fontSize: CGFloat = 16
    static let iconSize: CGFloat = 19
    static let tracking: CGFloat = 0.55
    #else
    static let fontSize: CGFloat = 10
    static let iconSize: CGFloat = 13
    static let tracking: CGFloat = 0.35
    #endif
}

private extension PlayerMetadataBadge.Tone {
    var color: Color {
        switch self {
        case .resolution: return Palette.accent
        case .hdr: return Color(red: 0x12 / 255, green: 0xB3 / 255, blue: 0xA6 / 255)
        case .dolbyVision: return Color(red: 0xC9 / 255, green: 0x9A / 255, blue: 0x2B / 255)
        case .audio: return Color(red: 0x7F / 255, green: 0x8F / 255, blue: 0xE0 / 255)
        }
    }

    var fillOpacity: Double {
        switch self {
        case .resolution: return 0.13
        case .hdr: return 0.14
        case .dolbyVision: return 0.15
        case .audio: return 0.14
        }
    }

    var borderOpacity: Double {
        switch self {
        case .resolution: return 0.42
        case .hdr: return 0.48
        case .dolbyVision: return 0.52
        case .audio: return 0.46
        }
    }
}

/// The exact self-hosted Material glyphs used by the web player. SwiftUI's SF
/// Symbols are excellent platform icons, but they made the same media facts
/// look unrelated across plurx clients.
private struct PlayerMetadataBadgeIcon: Shape {
    let kind: PlayerMetadataBadge.Kind

    func path(in rect: CGRect) -> Path {
        var path = Path()
        switch kind {
        case .resolution:
            path.move(to: CGPoint(x: 21, y: 3))
            path.addLine(to: CGPoint(x: 3, y: 3))
            path.addCurve(
                to: CGPoint(x: 1, y: 5),
                control1: CGPoint(x: 1.9, y: 3),
                control2: CGPoint(x: 1, y: 3.9)
            )
            path.addLine(to: CGPoint(x: 1, y: 17))
            path.addCurve(
                to: CGPoint(x: 3, y: 19),
                control1: CGPoint(x: 1, y: 18.1),
                control2: CGPoint(x: 1.9, y: 19)
            )
            path.addLine(to: CGPoint(x: 8, y: 19))
            path.addLine(to: CGPoint(x: 8, y: 21))
            path.addLine(to: CGPoint(x: 16, y: 21))
            path.addLine(to: CGPoint(x: 16, y: 19))
            path.addLine(to: CGPoint(x: 21, y: 19))
            path.addCurve(
                to: CGPoint(x: 23, y: 17),
                control1: CGPoint(x: 22.1, y: 19),
                control2: CGPoint(x: 23, y: 18.1)
            )
            path.addLine(to: CGPoint(x: 23, y: 5))
            path.addCurve(
                to: CGPoint(x: 21, y: 3),
                control1: CGPoint(x: 23, y: 3.9),
                control2: CGPoint(x: 22.1, y: 3)
            )
            path.closeSubpath()
            path.move(to: CGPoint(x: 21, y: 17))
            path.addLine(to: CGPoint(x: 3, y: 17))
            path.addLine(to: CGPoint(x: 3, y: 5))
            path.addLine(to: CGPoint(x: 21, y: 5))
            path.closeSubpath()
        case .dynamicRange:
            addPolygon([
                (19, 9), (20.25, 6.25), (23, 5), (20.25, 3.75),
                (19, 1), (17.75, 3.75), (15, 5), (17.75, 6.25)
            ], to: &path)
            addPolygon([
                (11.5, 9.5), (9, 4), (6.5, 9.5), (1, 12),
                (6.5, 14.5), (9, 20), (11.5, 14.5), (17, 12)
            ], to: &path)
            addPolygon([
                (19, 15), (17.75, 17.75), (15, 19), (17.75, 20.25),
                (19, 23), (20.25, 20.25), (23, 19), (20.25, 17.75)
            ], to: &path)
        case .audio:
            path.addRect(CGRect(x: 3, y: 10, width: 2, height: 4))
            path.addRect(CGRect(x: 7, y: 6, width: 2, height: 12))
            path.addRect(CGRect(x: 11, y: 2, width: 2, height: 20))
            path.addRect(CGRect(x: 15, y: 6, width: 2, height: 12))
            path.addRect(CGRect(x: 19, y: 10, width: 2, height: 4))
        }

        return path.applying(CGAffineTransform(
            a: rect.width / 24,
            b: 0,
            c: 0,
            d: rect.height / 24,
            tx: rect.minX,
            ty: rect.minY
        ))
    }

    private func addPolygon(
        _ points: [(CGFloat, CGFloat)],
        to path: inout Path
    ) {
        guard let first = points.first else { return }
        path.move(to: CGPoint(x: first.0, y: first.1))
        for point in points.dropFirst() {
            path.addLine(to: CGPoint(x: point.0, y: point.1))
        }
        path.closeSubpath()
    }
}

private struct PlayerMetadataBadgeView: View {
    let badge: PlayerMetadataBadge

    var body: some View {
        HStack(spacing: PlayerMetadataBadgeMetrics.contentSpacing) {
            HStack(spacing: PlayerMetadataBadgeMetrics.contentSpacing) {
                PlayerMetadataBadgeIcon(kind: badge.kind)
                    .fill(badge.tone.color, style: FillStyle(eoFill: true))
                    .frame(
                        width: PlayerMetadataBadgeMetrics.iconSize,
                        height: PlayerMetadataBadgeMetrics.iconSize
                    )
                if let mark = badge.mark {
                    Text(mark)
                }
            }
            .opacity(badge.dimmed ? PlayerMetadataBadgeMetrics.dimmedOpacity : 1)
            if let renderedMark = badge.renderedMark {
                Text("→ \(renderedMark)")
            }
        }
        .font(.system(
            size: PlayerMetadataBadgeMetrics.fontSize,
            weight: .bold,
            design: .rounded
        ))
        .tracking(PlayerMetadataBadgeMetrics.tracking)
        .foregroundStyle(badge.tone.color)
        .padding(.horizontal, PlayerMetadataBadgeMetrics.horizontalPadding)
        .padding(.vertical, PlayerMetadataBadgeMetrics.verticalPadding)
        .background {
            if !badge.dimmed {
                Capsule().fill(badge.tone.color.opacity(badge.tone.fillOpacity))
            }
        }
        .overlay {
            Capsule().stroke(
                badge.tone.color.opacity(badge.tone.borderOpacity),
                lineWidth: PlayerMetadataBadgeMetrics.strokeWidth
            )
        }
        .fixedSize(horizontal: true, vertical: false)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(badge.accessibilityLabel)
    }
}

struct PlayerOverlayVisibility: Equatable {
    let controls: Bool
    let playbackInfo: Bool
}

#if os(tvOS)
/// The legibility contract for playback diagnostics viewed across a room.
/// These values stay separate from compact transport chrome because the info
/// surface is deliberately a dashboard, not another row of player controls.
enum TVPlaybackInfoPresentation {
    static let panelMaxWidth: CGFloat = 1_480
    static let titleFontSize: CGFloat = 32
    static let valueFontSize: CGFloat = 19
    // No longer read by any view — the ledger sizes its section boxes from
    // their content. Retained because AppleClientTests asserts on it.
    static let cardMinimumHeight: CGFloat = 238
    static let debugPanelMaxWidth: CGFloat = 1_620
    static let debugPanelMaxHeight: CGFloat = 850
    static let debugEdgeInset: CGFloat = 48

    static func healthLabel(stalls: Int?) -> String {
        guard let stalls else { return "Measuring" }
        if stalls == 0 { return "No stalls" }
        return stalls == 1 ? "1 stall" : "\(stalls) stalls"
    }
}
#endif

enum PlayerNaturalEndAction: Equatable {
    case dismiss
    case findNext

    @MainActor
    func perform(
        dismiss: () -> Void,
        findNext: () -> Void
    ) {
        switch self {
        case .dismiss: dismiss()
        case .findNext: findNext()
        }
    }
}

/// Makes every exit from the full-screen player follow the same order:
/// publish the restoring UI state, release playback resources exactly once,
/// then leave the cover on the next main-loop turn. That turn is important on
/// iPadOS — it gives the hosting controller a chance to restore status-bar,
/// Home-indicator, and persistent-overlay preferences before it disappears.
@MainActor
final class PlayerLifecycleCoordinator: ObservableObject {
    @Published private(set) var isTearingDown = false

    private var didTeardown = false
    private var didFinish = false
    private var pendingCompletions: [@MainActor () -> Void] = []
    private var completionDrainScheduled = false

    func teardown(_ cleanup: () -> Void) {
        guard !didTeardown else { return }
        didTeardown = true
        cleanup()
    }

    func finish(
        teardown cleanup: () -> Void,
        completion: @escaping @MainActor () -> Void
    ) {
        enqueueCompletion(completion)
        guard !didFinish else { return }
        didFinish = true
        isTearingDown = true
        teardown(cleanup)
    }

    /// Cleanup and the tearing-down state are write-once; completion ownership
    /// is not. Queue every caller, including one that arrives after `didFinish`,
    /// and deliver in order on the next main-loop turn.
    private func enqueueCompletion(_ completion: @escaping @MainActor () -> Void) {
        pendingCompletions.append(completion)
        guard !completionDrainScheduled else { return }
        completionDrainScheduled = true
        Task { @MainActor in
            await withCheckedContinuation {
                (continuation: CheckedContinuation<Void, Never>) in
                DispatchQueue.main.async { continuation.resume() }
            }
            let completions = pendingCompletions
            pendingCompletions.removeAll()
            completionDrainScheduled = false
            for completion in completions { completion() }
        }
    }
}

#if os(iOS)
struct PlayerSystemOverlayPreferences {
    let statusBarHidden: Bool
    let persistentOverlays: Visibility

    static let restoredAfterPlayback = Self(
        statusBarHidden: false,
        persistentOverlays: .automatic
    )

    static func resolve(
        controlsVisible: Bool,
        persistentContentVisible: Bool
    ) -> Self {
        let systemChromeVisible = controlsVisible || persistentContentVisible
        return Self(
            statusBarHidden: !systemChromeVisible,
            persistentOverlays: systemChromeVisible ? .automatic : .hidden
        )
    }
}

struct PlayerSystemOverlayModifier: ViewModifier {
    let preferences: PlayerSystemOverlayPreferences

    func body(content: Content) -> some View {
        content
            // The custom AVPlayerLayer surface has no AVPlayerViewController to
            // retire iPadOS chrome for it. Keep system chrome in the same state
            // as the controls the viewer actually asked to show.
            .statusBarHidden(preferences.statusBarHidden)
            .persistentSystemOverlays(preferences.persistentOverlays)
    }
}

/// The landscape/iPad transport keeps its fixed control groups at the edges
/// and gives every remaining point to the timeline. Keeping this as a real
/// layout boundary prevents a `fixedSize` measurement from becoming the
/// selected row's final width inside `ViewThatFits`.
struct PlayerTouchWideRow<Transport: View, Timeline: View, Options: View>: View {
    let transport: Transport
    let timeline: Timeline
    let options: Options

    init(
        @ViewBuilder transport: () -> Transport,
        @ViewBuilder timeline: () -> Timeline,
        @ViewBuilder options: () -> Options
    ) {
        self.transport = transport()
        self.timeline = timeline()
        self.options = options()
    }

    var body: some View {
        HStack(spacing: 8) {
            transport
            timeline
                .layoutPriority(1)
            options
        }
        .frame(maxWidth: .infinity)
    }
}
#endif

/// The production marker content shared by the player and its layout tests.
///
/// A chapterless file still offers Skip Credits from a duration-based estimate,
/// and the server marks that marker `chapter: false` so the UI can hedge
/// (ARCHITECTURE.md §6). The hedge is the whole point: an estimate that looks
/// exactly like a chapter-derived button reads as a bug rather than as the
/// guess it is. A marker that is chapter-derived — or that omits the field, as
/// an older server does — keeps the exact presentation.
///
/// The hedge leads rather than trails. This label is one line at every text
/// size, so an accessibility size truncates its tail; a trailing "(est.)" would
/// be the first thing to disappear at exactly the size that needs it most.
struct PlayerMarkerButtonLabel: View {
    let title: String
    var estimated: Bool = false

    /// The hollow glyph is the second signal, for a viewer who reads the icon
    /// before the text: an outline against the exact marker's solid fill.
    static let exactSymbol = "forward.end.fill"
    static let estimatedSymbol = "forward.end"
    static let estimatedMark = "≈"

    /// Provenance is authoritative when present. `chapter` remains the
    /// compatibility fallback for an older server.
    static func isEstimated(_ marker: Marker) -> Bool {
        marker.isEstimated
    }

    static func symbol(estimated: Bool) -> String {
        estimated ? estimatedSymbol : exactSymbol
    }

    static func displayTitle(_ title: String, estimated: Bool) -> String {
        estimated ? "\(estimatedMark) \(title)" : title
    }

    /// VoiceOver reads neither the glyph nor the mark, so it gets the hedge in
    /// words instead.
    static func accessibilityLabel(_ title: String, estimated: Bool) -> String {
        estimated ? "\(title), estimated" : title
    }

    var body: some View {
        Label(
            Self.displayTitle(title, estimated: estimated),
            systemImage: Self.symbol(estimated: estimated)
        )
        .font(.system(.caption, design: .monospaced))
        .lineLimit(1)
        .accessibilityLabel(Self.accessibilityLabel(title, estimated: estimated))
    }
}

/// Keeps transient playback actions compact and pinned to the trailing edge of
/// the transport chrome at every viewport width.
struct PlayerTrailingControlRow<Control: View>: View {
    let control: Control

    init(@ViewBuilder control: () -> Control) {
        self.control = control()
    }

    var body: some View {
        HStack {
            Spacer(minLength: 0)
            control
                #if os(tvOS)
                .fixedSize(horizontal: true, vertical: false)
                #endif
        }
        .frame(maxWidth: .infinity)
    }
}

/// Source vs delivered vs rendered, for the one badge that has to answer both
/// "what is this file?" and "what am I getting?" — MEDIA-BADGES-PLAN.md §2.
///
/// A reporter and nothing else. No value computed here reaches a decision, a
/// capability query, or a session request; the badge changed, the pipeline did
/// not (that plan's §9).
enum DynamicRange {
    static let dolbyVision = "dolby_vision"
    static let hdr10 = "hdr10"
    static let hlg = "hlg"
    static let sdr = "sdr"

    /// The coarse grade of a source in the server's own vocabulary, so a source
    /// and `delivered_dynamic_range` compare by string equality. Both fields
    /// are read because a file can carry only the rich `hdr_format` label.
    static func source(hdr: String?, hdrFormat: String?) -> String? {
        let label = hdrFormat?.trimmingCharacters(in: .whitespaces) ?? ""
        let coarse = hdr?.trimmingCharacters(in: .whitespaces).lowercased() ?? ""
        if coarse == dolbyVision || label.localizedCaseInsensitiveContains("dolby") {
            return dolbyVision
        }
        if coarse == sdr { return nil }
        if !coarse.isEmpty { return coarse }
        if label.isEmpty { return nil }
        // "HDR10+", "HDR10", "SMPTE ST 2084" — anything left that names a grade
        // without saying HLG is a PQ grade.
        return label.localizedCaseInsensitiveContains("hlg") ? hlg : hdr10
    }

    /// The terse chip text used by the web player. Dolby Vision retains its
    /// probed profile (`DV P8`); other HDR formats use the already-short rich
    /// source string when one is available.
    static func sourceMark(_ grade: String, hdrFormat: String?) -> String {
        let rich = hdrFormat?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if grade == dolbyVision {
            if let profile = dolbyVisionProfile(in: rich) {
                return "DV P\(profile)"
            }
            return "DV"
        }
        return rich.isEmpty ? shortLabel(grade) : rich.uppercased()
    }

    static func sourceLabel(_ grade: String, hdrFormat: String?) -> String {
        let rich = hdrFormat?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return rich.isEmpty ? longLabel(grade) : rich
    }

    private static func dolbyVisionProfile(in label: String) -> String? {
        guard let range = label.range(
            of: #"profile\s*[0-9]+"#,
            options: [.regularExpression, .caseInsensitive]
        ) else { return nil }
        let digits = label[range].filter { $0.isNumber }
        return digits.isEmpty ? nil : String(digits)
    }

    static func shortLabel(_ grade: String) -> String {
        switch grade {
        case Self.dolbyVision: return "DV"
        case Self.hdr10: return "HDR10"
        case Self.hlg: return "HLG"
        case Self.sdr: return "SDR"
        default: return grade.uppercased()
        }
    }

    static func longLabel(_ grade: String) -> String {
        switch grade {
        case Self.dolbyVision: return "Dolby Vision"
        case Self.hdr10: return "HDR10"
        case Self.hlg: return "HLG"
        case Self.sdr: return "SDR"
        default: return grade.uppercased()
        }
    }

    /// What is on the panel. Delivered bits are necessary but not sufficient:
    /// an HDR10 direct play on an SDR display is delivered HDR and rendered
    /// SDR. `AVPlayer.eligibleForHDRPlayback` (via `Caps.displayIsHDR`) is the
    /// documented signal and the whole of it — there is no public per-variant
    /// introspection for HLS, and headroom polling is a stated non-goal.
    static func rendered(delivered: String, displayHDR: Bool) -> String {
        (delivered != sdr && !displayHDR) ? sdr : delivered
    }

    /// The server already says *why* ("Dolby Vision metadata removed for this
    /// device; compatible HDR base kept") — better than anything invented here.
    /// Only borrow a reason that is actually about the picture.
    static func reason(from reasons: [String]?) -> String? {
        reasons?.first {
            $0.localizedCaseInsensitiveContains("dolby vision")
                || $0.localizedCaseInsensitiveContains("hdr")
                || $0.localizedCaseInsensitiveContains("tone")
        }
    }
}

/// Full-screen Apple player with an explicit on-demand transport. AVPlayer
/// sees a growing plurx HLS playlist as an EVENT stream while the server is
/// producing it, so relying on the system overlay alone labels a movie LIVE
/// and can replace Pause with Stop. The controls here use the known film
/// runtime and work for direct, remux, and transcode delivery alike.
struct PlayerView: View {
    static let controlAutoHideDelayNanoseconds: UInt64 = 4_000_000_000

    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss

    let itemId: Int
    let fileId: Int
    let startMs: Int
    let durationMs: Int
    let title: String
    var progressOffsetMs: Int = 0
    var itemDurationMs: Int? = nil
    var subtitle: String? = nil
    var year: Int? = nil
    var airDate: String? = nil
    var overview: String? = nil
    var offlineItem: OfflineItem? = nil
    /// The detail screen's pre-play track choice, spent on this playback only.
    var selection: PrePlaySelection = .none
    /// Debug acceptance can choose a deterministic first rung without racing a
    /// remote-control quality change against the initial session open.
    var initialHeight: Int? = nil
    var diagnosticProbesEnabled = false
    var onPlayNext: ((PlayContext) -> Void)?
    /// Hands the owning detail screen the last on-screen position immediately.
    /// The server progress write is intentionally best-effort and asynchronous;
    /// without this handoff the still-present detail view keeps rendering the
    /// resume point it loaded before playback began.
    var onPlaybackStopped: ((Int) -> Void)?

    @StateObject private var controller = PlayerController()
    @StateObject private var pictureInPicture = PictureInPictureController()
    @StateObject private var lifecycle = PlayerLifecycleCoordinator()
    @State private var showStats = false
    @State private var statsMode = PlaybackStatsMode.standard
    @State private var findingNext = false
    @State private var nextEpisodeTask: Task<Void, Never>?
    @State private var isScrubbing = false
    @State private var scrubMs = 0.0
    @State private var controlsVisible = true
    @State private var autoHideGeneration = 0
    #if os(iOS)
    @State private var activeOptionMenu: PlayerOptionMenu?
    #endif
    #if os(tvOS)
    @FocusState private var focusedControl: PlayerControl?
    @State private var tvProgressEngaged = false
    #endif

    var body: some View {
        ZStack(alignment: .topLeading) {
            Color.black.ignoresSafeArea()

            PlayerSurface(
                player: controller.player,
                pictureInPicture: pictureInPicture,
                pgsOverlay: controller.pgsOverlayWindow,
                allowsPictureInPicture: PlayerSurface.shouldAllowPictureInPicture(
                    isTearingDown: lifecycle.isTearingDown,
                    pgsOverlayIsActive: controller.pgsOverlayIsActive
                )
            )
                .ignoresSafeArea()

            #if os(tvOS)
            if lifecycle.isTearingDown {
                // Keep the presented cover in the focus system until the next
                // main-loop turn dismisses it. Removing every focusable child
                // first produces a transient "no focusable views" state.
                Color.clear
                    .contentShape(Rectangle())
                    .ignoresSafeArea()
                    .focusable()
                    .focusEffectDisabled()
                    .accessibilityHidden(true)
            }
            #endif

            if !lifecycle.isTearingDown {
                #if os(tvOS)
                if !controlsVisible {
                    Color.clear
                        .contentShape(Rectangle())
                        .ignoresSafeArea()
                        .focusable()
                        .focusEffectDisabled()
                        .focused($focusedControl, equals: .reveal)
                        .onTapGesture { revealControlsFromRemote() }
                        .onMoveCommand { direction in seekFromRemote(direction) }
                        .onPlayPauseCommand {
                            controller.togglePlayPause()
                            revealControlsFromRemote()
                        }
                        .accessibilityLabel("Show playback controls")
                }
                #endif

                #if os(iOS)
                Color.clear
                    .contentShape(Rectangle())
                    .ignoresSafeArea()
                    .onTapGesture { toggleControls() }
                    .accessibilityLabel("Show or hide playback controls")
                #endif

                if controller.failed {
                    failureView
                } else {
                    if overlayVisibility.controls {
                        VStack(spacing: 0) {
                            HStack(alignment: .top) {
                                #if os(iOS)
                                closeButton
                                #endif
                                Spacer()
                            }
                            Spacer()
                            playbackControls
                                #if os(tvOS)
                                // Playback info is a modal surface on the TV.
                                // Keep the remote inside its visible Done action
                                // instead of letting focus escape to dimmed
                                // transport controls behind the panel.
                                .disabled(showStats && statsMode != .mini)
                                #endif
                        }
                        .padding(20)
                        .transition(.opacity)
                    }

                    if overlayVisibility.playbackInfo {
                        #if os(tvOS)
                        PlaybackStatsView(
                            controller: controller,
                            mode: $statsMode,
                            onDismiss: dismissPlaybackInfo
                        )
                        .transition(.opacity.combined(with: .scale(scale: 0.98)))
                        #else
                        PlaybackStatsView(
                            controller: controller,
                            mode: $statsMode,
                            onDismiss: dismissPlaybackInfo
                        )
                        .frame(maxWidth: .infinity, alignment: .trailing)
                        .padding(20)
                        .transition(.opacity.combined(with: .move(edge: .trailing)))
                        #endif
                    }
                }

                if controller.isChangingStream {
                    streamChangeProgress
                        .tint(.white)
                        .padding(18)
                        .background(.ultraThinMaterial, in: Circle())
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }

                if findingNext {
                    VStack(spacing: 10) {
                        ProgressView().tint(.white)
                        Text("Up next…")
                            .font(.system(.callout, design: .monospaced))
                            .foregroundColor(.white)
                    }
                    .padding(18)
                    .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }

                if let error = playbackBannerMessage,
                   !controller.failed {
                    Text(error)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(.white)
                        .padding(10)
                        .background(Palette.accent.opacity(0.9), in: RoundedRectangle(cornerRadius: 8))
                        .frame(maxWidth: .infinity, alignment: .top)
                        .padding(.top, 20)
                        .padding(.horizontal, 80)
                }
            }
        }
        .task {
            #if os(iOS)
            if let offlineItem {
                controller.startOffline(model: model, item: offlineItem)
            } else {
                controller.start(
                    model: model,
                    itemId: itemId,
                    fileId: fileId,
                    startMs: startMs,
                    durationMs: durationMs,
                    progressOffsetMs: progressOffsetMs,
                    itemDurationMs: itemDurationMs,
                    title: title,
                    selection: selection,
                    initialHeight: initialHeight,
                    diagnosticProbesEnabled: diagnosticProbesEnabled
                )
            }
            #else
            controller.start(
                model: model,
                itemId: itemId,
                fileId: fileId,
                startMs: startMs,
                durationMs: durationMs,
                progressOffsetMs: progressOffsetMs,
                itemDurationMs: itemDurationMs,
                title: title,
                selection: selection,
                initialHeight: initialHeight,
                diagnosticProbesEnabled: diagnosticProbesEnabled
            )
            #endif
            #if os(tvOS)
            try? await Task.sleep(nanoseconds: 100_000_000)
            focusedControl = .playPause
            #endif
        }
        .onDisappear {
            lifecycle.teardown { teardownPlayback() }
        }
        .task(id: autoHideGeneration) {
            guard Self.shouldAutoHideControls(
                visible: controlsVisible,
                scrubbing: isScrubbing,
                changingStream: controller.isChangingStream,
                optionMenuOpen: optionMenuOpen,
                tearingDown: lifecycle.isTearingDown
            ) else { return }
            try? await Task.sleep(nanoseconds: Self.controlAutoHideDelayNanoseconds)
            guard !Task.isCancelled,
                  Self.shouldAutoHideControls(
                      visible: controlsVisible,
                      scrubbing: isScrubbing,
                      changingStream: controller.isChangingStream,
                      optionMenuOpen: optionMenuOpen,
                      tearingDown: lifecycle.isTearingDown
                  ) else { return }
            hideControls()
        }
        .onChange(of: controller.isPlaying) { _, _ in revealControls() }
        .onChange(of: controller.isChangingStream) { _, _ in revealControls() }
        .onChange(of: showStats) { _, _ in restartAutoHideTimer() }
        .onChange(of: isScrubbing) { _, _ in restartAutoHideTimer() }
        .onChange(of: optionMenuOpen) { _, _ in restartAutoHideTimer() }
        .onChange(of: controller.finished) { _, finished in
            let action = itemDurationMs != nil && offlineItem == nil
                ? Self.audiobookNaturalEndAction(
                    finished: finished,
                    alreadyFinding: findingNext
                )
                : Self.naturalEndAction(
                    finished: finished,
                    autoplay: model.autoplay,
                    offline: offlineItem != nil
                )
            guard let action else { return }
            // Leave the SwiftUI update transaction before publishing teardown
            // state and replacing the AVPlayer item.
            Task { @MainActor in
                await Task.yield()
                guard !lifecycle.isTearingDown else { return }
                handleNaturalEnd(action)
            }
        }
        #if os(tvOS)
        .onChange(of: controller.failed) { _, failed in
            guard failed else { return }
            focusedControl = controller.canRetryPlaybackFailure ? .retry : .close
        }
        .onChange(of: focusedControl) { _, newControl in
            if newControl != .progress { tvProgressEngaged = false }
            if controlsVisible { restartAutoHideTimer() }
        }
        .onExitCommand {
            if tvProgressEngaged {
                tvProgressEngaged = false
                revealControls()
            } else if showStats {
                dismissPlaybackInfo()
            } else if controlsVisible {
                hideControls()
            } else {
                finishPlayback()
            }
        }
        #endif
        #if os(iOS)
        .modifier(PlayerSystemOverlayModifier(
            preferences: playerSystemOverlayPreferences
        ))
        #endif
    }

    private func handleNaturalEnd(_ action: PlayerNaturalEndAction) {
        action.perform(
            dismiss: { finishPlayback() },
            findNext: {
                findingNext = true
                nextEpisodeTask?.cancel()
                nextEpisodeTask = Task {
                    let next: PlayContext?
                    if itemDurationMs != nil {
                        next = await model.nextAudiobookPart(itemId: itemId, after: fileId)
                    } else {
                        next = await model.nextEpisode(after: itemId)
                    }
                    guard !Task.isCancelled, !lifecycle.isTearingDown else { return }
                    findingNext = false
                    guard let next, let onPlayNext else {
                        finishPlayback()
                        return
                    }
                    finishPlayback(continuingPlayback: true) { onPlayNext(next) }
                }
            }
        )
    }

    private var playbackBannerMessage: String? {
        controller.playbackNotice
            ?? controller.playbackError
            ?? pictureInPicture.errorMessage
    }

    #if os(iOS)
    private var playerSystemOverlayPreferences: PlayerSystemOverlayPreferences {
        if lifecycle.isTearingDown {
            return .restoredAfterPlayback
        }
        return PlayerSystemOverlayPreferences.resolve(
            controlsVisible: controlsVisible,
            // These surfaces outlive the transport's four-second timeout. Keep
            // system chrome stable while the viewer reads them, then retire it
            // only after the last visible player surface has gone away.
            persistentContentVisible: showStats
                || controller.failed
                || controller.isChangingStream
                || findingNext
                || playbackBannerMessage != nil
        )
    }
    #endif

    private var overlayVisibility: PlayerOverlayVisibility {
        Self.overlayVisibility(
            controlsVisible: controlsVisible,
            playbackInfoVisible: showStats
        )
    }

    static func overlayVisibility(
        controlsVisible: Bool,
        playbackInfoVisible: Bool
    ) -> PlayerOverlayVisibility {
        PlayerOverlayVisibility(
            controls: controlsVisible,
            playbackInfo: playbackInfoVisible
        )
    }

    static func shouldAutoHideControls(
        visible: Bool,
        scrubbing: Bool,
        changingStream: Bool,
        optionMenuOpen: Bool,
        tearingDown: Bool = false
    ) -> Bool {
        visible && !scrubbing && !changingStream && !optionMenuOpen && !tearingDown
    }

    static func naturalEndAction(
        finished: Bool,
        autoplay: Bool,
        offline: Bool
    ) -> PlayerNaturalEndAction? {
        guard finished else { return nil }
        return autoplay && !offline ? .findNext : .dismiss
    }

    static func audiobookNaturalEndAction(
        finished: Bool,
        alreadyFinding: Bool
    ) -> PlayerNaturalEndAction? {
        guard finished, !alreadyFinding else { return nil }
        // Physical parts are one logical work, so crossing the seam does not
        // depend on the separate "autoplay next episode" preference.
        return .findNext
    }

    /// The one teardown path for natural completion, manual Close/Menu, and
    /// fatal-screen exits. UI state goes first so iPadOS restores system chrome
    /// while the cover still exists; AVKit and transport resources go next.
    private func finishPlayback(
        continuingPlayback: Bool = false,
        then completion: (@MainActor () -> Void)? = nil
    ) {
        lifecycle.finish(teardown: {
            teardownPlayback(deactivateAudioSession: !continuingPlayback)
        }) {
            if let completion {
                completion()
            } else {
                dismiss()
            }
        }
    }

    private func teardownPlayback(deactivateAudioSession: Bool = true) {
        let stoppedAt = controller.realPositionMs()
        nextEpisodeTask?.cancel()
        nextEpisodeTask = nil
        autoHideGeneration &+= 1
        showStats = false
        findingNext = false
        isScrubbing = false
        #if os(iOS)
        activeOptionMenu = nil
        #else
        tvProgressEngaged = false
        #endif
        pictureInPicture.detach()
        controller.stop(deactivateAudioSession: deactivateAudioSession)
        onPlaybackStopped?(stoppedAt)
    }

    /// A presented touch menu is hosted outside the control hierarchy. Removing
    /// the controls therefore removes its presentation anchor and dismisses the
    /// menu too. On tvOS, focus on a menu button is the equivalent interaction:
    /// keep the chrome up while the viewer is opening or navigating that menu.
    /// An engaged progress bar holds it open for the same reason.
    private var optionMenuOpen: Bool {
        #if os(iOS)
        activeOptionMenu != nil
        #else
        if tvProgressEngaged { return true }
        switch focusedControl {
        case .audio, .subtitles, .quality: return true
        default: return false
        }
        #endif
    }

    private func toggleControls() {
        #if os(iOS)
        withAnimation(.easeInOut(duration: 0.2)) {
            controlsVisible.toggle()
        }
        restartAutoHideTimer()
        #endif
    }

    private func revealControls() {
        withAnimation(.easeInOut(duration: 0.2)) { controlsVisible = true }
        restartAutoHideTimer()
    }

    private func restartAutoHideTimer() {
        guard !lifecycle.isTearingDown else { return }
        autoHideGeneration &+= 1
    }

    private func hideControls() {
        guard controlsVisible else { return }
        #if os(iOS)
        activeOptionMenu = nil
        #else
        tvProgressEngaged = false
        #endif
        withAnimation(.easeOut(duration: 0.2)) {
            controlsVisible = false
        }
        #if os(tvOS)
        focusedControl = nil
        Task { @MainActor in
            await Task.yield()
            if !controlsVisible { focusedControl = .reveal }
        }
        #endif
    }

    private func dismissPlaybackInfo() {
        withAnimation { showStats = false }
        #if os(tvOS)
        revealControlsFromRemote()
        #else
        revealControls()
        #endif
    }

    #if os(tvOS)
    private func revealControlsFromRemote() {
        revealControls()
        Task { @MainActor in
            await Task.yield()
            if controlsVisible { focusedControl = .playPause }
        }
    }

    private func seekFromRemote(_ direction: MoveCommandDirection) {
        guard let seekDirection = Self.playerSeekDirection(direction) else { return }
        let outcome = TVPlayerRemoteRouting.moveOutcome(
            focusedControl: .reveal,
            progressEngaged: false,
            direction: seekDirection
        )
        applyRemoteMoveOutcome(outcome, revealingFromHiddenControls: true)
    }

    private static func playerSeekDirection(
        _ direction: MoveCommandDirection
    ) -> PlayerSeekDirection? {
        switch direction {
        case .left: .left
        case .right: .right
        case .up: .up
        case .down: .down
        @unknown default: nil
        }
    }

    private func applyRemoteMoveOutcome(
        _ outcome: PlayerRemoteMoveOutcome,
        revealingFromHiddenControls: Bool = false
    ) {
        switch outcome {
        case let .seek(seconds):
            controller.skip(seconds: seconds)
            if revealingFromHiddenControls {
                revealControlsFromRemote()
            } else {
                revealControls()
            }
        case let .focus(control):
            focusedControl = control
            revealControls()
        case .ignore:
            revealControls()
        }
    }
    #endif

    @ViewBuilder
    private var streamChangeProgress: some View {
        #if os(iOS)
        ProgressView().controlSize(.large)
        #else
        ProgressView()
        #endif
    }

    private var failureView: some View {
        VStack(spacing: 14) {
            Text(controller.playbackFailureTitle)
                .font(.system(.body, design: .monospaced))
                .foregroundColor(.white)
            if let error = controller.playbackError {
                Text(error)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundColor(Palette.muted)
                    .multilineTextAlignment(.center)
            }
            HStack(spacing: 12) {
                if controller.canRetryPlaybackFailure {
                    Button("Try Again") { controller.retryAfterPlaybackFailure() }
                        .buttonStyle(.borderedProminent)
                        .tint(Palette.accent)
                        #if os(tvOS)
                        .focused($focusedControl, equals: .retry)
                        #endif
                }
                Button("Close") { finishPlayback() }
                    .buttonStyle(.bordered)
                    #if os(tvOS)
                    .focused($focusedControl, equals: .close)
                    #endif
            }
        }
        .padding(30)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black)
    }

    #if os(iOS)
    private var closeButton: some View {
        Button { finishPlayback() } label: {
            Image(systemName: "xmark.circle.fill")
                .font(.largeTitle)
                .foregroundStyle(.white.opacity(0.9))
        }
        .buttonStyle(.plain)
    }
    #endif

    private var playbackControls: some View {
        VStack(alignment: .leading, spacing: 8) {
            playbackInfoHeader

            if let marker = controller.activeMarker {
                PlayerTrailingControlRow {
                    markerButton(marker)
                }
            }

            #if os(tvOS)
            HStack(spacing: 12) {
                transportControlGroup
                if controller.knownDurationMs > 0 {
                    playbackTimeLabel(controller.currentMs)
                    tvProgressBar
                        .layoutPriority(1)
                    playbackTimeLabel(controller.knownDurationMs)
                }
                playbackOptionGroup
            }
            #else
            touchPlaybackRows
            #endif
        }
        #if os(tvOS)
        .font(.body)
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .foregroundStyle(.white)
        .frame(maxWidth: .infinity)
        #else
        .font(.body)
        .buttonStyle(IOSPlayerControlButtonStyle())
        .foregroundStyle(.white)
        .frame(maxWidth: .infinity)
        #endif
    }

    static func nowPlayingSummary(_ overview: String?) -> String {
        let summary = overview?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return summary.isEmpty ? "No description available." : summary
    }

    static func playbackDateLabel(airDate: String?, year: Int?) -> String? {
        let raw = airDate?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !raw.isEmpty {
            let parser = DateFormatter()
            parser.calendar = Calendar(identifier: .gregorian)
            parser.locale = Locale(identifier: "en_US_POSIX")
            parser.timeZone = TimeZone(secondsFromGMT: 0)
            parser.dateFormat = "yyyy-MM-dd"
            if let date = parser.date(from: String(raw.prefix(10))) {
                let display = DateFormatter()
                display.calendar = parser.calendar
                display.locale = Locale(identifier: "en_US_POSIX")
                display.timeZone = parser.timeZone
                display.setLocalizedDateFormatFromTemplate("MMM d, yyyy")
                return display.string(from: date)
            }
            return raw
        }
        return year.map(String.init)
    }

    private func playbackTimeLabel(_ milliseconds: Int) -> some View {
        Text(formatTime(milliseconds))
            .fixedSize()
            #if os(tvOS)
            .padding(.horizontal, TVPlayerChromeMetrics.timeHorizontalInset)
            .padding(.vertical, TVPlayerChromeMetrics.timeVerticalInset)
            .background(.black.opacity(0.58), in: Capsule())
            #endif
    }

    private var playbackInfoHeader: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(playbackHeading)
                #if os(tvOS)
                .font(.system(size: 30, weight: .bold))
                #else
                .font(.system(size: 26, weight: .bold))
                #endif
                .foregroundColor(.white)
                .lineLimit(1)

            playbackFacts

            if !playbackContext.isEmpty {
                Text(playbackContext.joined(separator: "   ·   "))
                #if os(tvOS)
                .font(.system(size: 21, weight: .medium, design: .rounded))
                #else
                .font(.system(size: 15, weight: .medium, design: .rounded))
                #endif
                .foregroundColor(.white.opacity(0.7))
                .lineLimit(1)
            }

            Text(Self.nowPlayingSummary(overview))
                #if os(tvOS)
                .font(.system(size: TVPlayerChromeMetrics.infoBodyFontSize, weight: .regular))
                .lineLimit(3)
                #else
                .font(.system(size: 15, weight: .regular))
                .lineLimit(3)
                #endif
                .foregroundStyle(.white.opacity(0.88))
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: 1_180, alignment: .leading)
        .accessibilityElement(children: .combine)
    }

    private var playbackHeading: String {
        [subtitle, title]
            .compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: "   ·   ")
    }

    private var playbackContext: [String] {
        [Self.playbackDateLabel(airDate: airDate, year: year), runtimeLabel]
            .compactMap { $0 }
            .filter { !$0.isEmpty }
    }

    private var playbackFacts: some View {
        let audio = controller.audioTracks.first { $0.index == controller.selectedAudio }
            ?? controller.audioTracks.first { $0.default }
            ?? controller.audioTracks.first
        // Eligibility is asked here, at render time, rather than cached at
        // launch: an Apple TV whose Dolby Vision output is turned off in
        // Settings changes this answer while the app is running.
        let badges = Self.playbackBadges(
            source: controller.decision?.source,
            audio: audio,
            delivered: controller.deliveredRange,
            displayHDR: Caps.displayIsHDR
        )
        return HStack(spacing: PlayerMetadataBadgeMetrics.rowSpacing) {
            ForEach(badges) { badge in
                PlayerMetadataBadgeView(badge: badge)
            }
        }
    }

    private var runtimeLabel: String? {
        let milliseconds = controller.knownDurationMs > 0
            ? controller.knownDurationMs
            : durationMs
        guard milliseconds > 0 else { return nil }
        let totalMinutes = milliseconds / 60_000
        let hours = totalMinutes / 60
        let minutes = totalMinutes % 60
        return hours > 0 ? "\(hours)h \(minutes)m" : "\(minutes)m"
    }

    /// `delivered`/`displayHDR` default to "no session yet, assume nothing is
    /// being lost", which is exactly the source-only state the detail screens
    /// and an older server both want.
    static func playbackBadges(
        source: SourceSummary?,
        audio: AudioTrack?,
        delivered: String? = nil,
        displayHDR: Bool = true
    ) -> [PlayerMetadataBadge] {
        var badges: [PlayerMetadataBadge] = []
        if let label = playbackResolutionLabel(width: source?.width, height: source?.height) {
            badges.append(PlayerMetadataBadge(
                kind: .resolution,
                tone: .resolution,
                mark: label.uppercased(),
                accessibilityLabel: label
            ))
        }
        if let range = dynamicRangeBadge(
            hdr: source?.hdr,
            hdrFormat: source?.hdrFormat,
            delivered: delivered,
            displayHDR: displayHDR
        ) {
            badges.append(range)
        }
        if let audio, let sound = soundLabel(audio) {
            badges.append(PlayerMetadataBadge(
                kind: .audio,
                tone: .audio,
                mark: sound.mark,
                accessibilityLabel: sound.accessibilityLabel
            ))
        }
        return badges
    }

    /// The three states of MEDIA-BADGES-PLAN §2.3, as a pure function of
    /// (source grade, delivered grade, display capability):
    ///
    /// - **lit** — rendered grade equals the source grade: today's chip.
    /// - **different grade** — they differ: the source half dims and the arrow
    ///   suffix names what is actually on screen (`DV → HDR10`) at full
    ///   brightness, matching the web player's capability/function split.
    /// - **source-only** — `delivered` is nil (no session, or a server that
    ///   does not report it): today's chip, unchanged.
    ///
    /// The badge text always starts from what the file carries, because that
    /// claim stays true either way; what changes is whether it is being kept.
    static func dynamicRangeBadge(
        hdr: String?,
        hdrFormat: String?,
        delivered: String?,
        displayHDR: Bool
    ) -> PlayerMetadataBadge? {
        guard let source = DynamicRange.source(hdr: hdr, hdrFormat: hdrFormat) else {
            return nil
        }
        let sourceMark = DynamicRange.sourceMark(source, hdrFormat: hdrFormat)
        let sourceLabel = DynamicRange.sourceLabel(source, hdrFormat: hdrFormat)
        let tone: PlayerMetadataBadge.Tone = source == DynamicRange.dolbyVision
            ? .dolbyVision
            : .hdr
        let lit = PlayerMetadataBadge(
            kind: .dynamicRange,
            tone: tone,
            mark: sourceMark,
            accessibilityLabel: sourceLabel
        )
        guard let delivered, !delivered.isEmpty else { return lit }
        let rendered = DynamicRange.rendered(delivered: delivered, displayHDR: displayHDR)
        guard rendered != source else { return lit }
        return PlayerMetadataBadge(
            kind: .dynamicRange,
            tone: tone,
            mark: sourceMark,
            accessibilityLabel:
                "\(DynamicRange.longLabel(source)), playing as \(DynamicRange.longLabel(rendered))",
            renderedMark: DynamicRange.shortLabel(rendered),
            dimmed: true
        )
    }

    /// Exact port of the web player's `resLabel`: it uses both raster edges so
    /// portrait metadata and scope-cropped masters stay in the intended tier.
    static func playbackResolutionLabel(width: Int?, height: Int?) -> String? {
        let validWidth = (width ?? 0) > 0 ? width ?? 0 : 0
        let validHeight = (height ?? 0) > 0 ? height ?? 0 : 0
        let shortEdge: Int
        let longEdge: Int
        if validWidth > 0, validHeight > 0 {
            shortEdge = min(validWidth, validHeight)
            longEdge = max(validWidth, validHeight)
        } else {
            shortEdge = validHeight > 0 ? validHeight : validWidth
            longEdge = shortEdge
        }
        guard shortEdge > 0 else { return nil }
        if longEdge >= 3_200 || shortEdge >= 1_700 { return "2160p" }
        if longEdge >= 2_300 || shortEdge >= 1_300 { return "1440p" }
        if longEdge >= 1_600 || shortEdge >= 900 { return "1080p" }
        if longEdge >= 1_100 || shortEdge >= 650 { return "720p" }
        if longEdge >= 700 || shortEdge >= 400 { return "480p" }
        return "\(shortEdge)p"
    }

    /// The same three-layer truth in one sentence, for the playback-info
    /// panel's "Dynamic range" row — the web player's equivalent row worded the
    /// same way. Nil when there is nothing to report: an SDR source with no
    /// session on it.
    static func dynamicRangeSummary(
        source: SourceSummary?,
        delivered: String?,
        displayHDR: Bool,
        reasons: [String]?
    ) -> String? {
        let grade = DynamicRange.source(hdr: source?.hdr, hdrFormat: source?.hdrFormat)
            ?? DynamicRange.sdr
        guard let delivered, !delivered.isEmpty else {
            guard grade != DynamicRange.sdr else { return nil }
            // No session and no report: the rich source label is all there is.
            return source?.hdrFormat ?? DynamicRange.longLabel(grade)
        }
        let rendered = DynamicRange.rendered(delivered: delivered, displayHDR: displayHDR)
        let long = DynamicRange.longLabel(rendered)
        guard rendered != grade else { return "\(long) (rendering)" }
        let displayLoss = delivered != DynamicRange.sdr && !displayHDR
        let note: String
        if displayLoss {
            note = "this display is not HDR"
        } else if let served = DynamicRange.reason(from: reasons) {
            note = served
        } else if rendered == DynamicRange.sdr {
            note = "tone-mapped from \(DynamicRange.longLabel(grade))"
        } else {
            note = "delivered as \(long)"
        }
        return "\(long) — \(note)"
    }

    static func playbackFacts(
        source: SourceSummary?,
        audio: AudioTrack?,
        delivered: String? = nil,
        displayHDR: Bool = true
    ) -> [String] {
        playbackBadges(
            source: source,
            audio: audio,
            delivered: delivered,
            displayHDR: displayHDR
        ).map(\.accessibilityLabel)
    }

    static func soundLabel(_ track: AudioTrack) -> (mark: String, accessibilityLabel: String)? {
        let title = track.title ?? ""
        let format: (mark: String, label: String)
        if title.localizedCaseInsensitiveContains("atmos") {
            format = ("ATMOS", "Dolby Atmos")
        } else {
            switch track.codec.lowercased() {
            case "eac3", "e-ac-3": format = ("DD+", "Dolby Digital Plus")
            case "ac3", "ac-3": format = ("DD", "Dolby Digital")
            case "truehd": format = ("TRUEHD", "Dolby TrueHD")
            case "dts", "dca": format = ("DTS", "DTS")
            case "aac": format = ("AAC", "AAC")
            case "flac": format = ("FLAC", "FLAC")
            case "opus": format = ("OPUS", "Opus")
            default:
                let codec = track.codec.uppercased()
                format = (codec, codec)
            }
        }
        let channels: (mark: String, label: String)?
        switch track.channels {
        case 1: channels = ("MONO", "Mono")
        case 2: channels = ("2.0", "2.0")
        case 6: channels = ("5.1", "5.1")
        case 7: channels = ("6.1", "6.1")
        case 8: channels = ("7.1", "7.1")
        case let count?: channels = ("\(count)CH", "\(count)ch")
        case nil: channels = nil
        }
        guard !format.mark.isEmpty else { return nil }
        return (
            [format.mark, channels?.mark].compactMap { $0 }.joined(separator: " "),
            [format.label, channels?.label].compactMap { $0 }.joined(separator: " ")
        )
    }

    private func markerButton(_ marker: Marker) -> some View {
        Button {
            controller.skipActiveMarker()
            revealControls()
        } label: {
            PlayerMarkerButtonLabel(
                title: marker.label,
                estimated: PlayerMarkerButtonLabel.isEstimated(marker)
            )
        }
        #if os(tvOS)
        .buttonStyle(TVReadableButtonStyle(prominent: true))
        .focused($focusedControl, equals: .marker)
        #else
        .buttonStyle(.borderedProminent)
        .tint(Palette.accent)
        #endif
        .onAppear { controller.reportMarkerOffer(marker) }
    }

    private var expandedControlRow: some View {
        HStack(spacing: 12) {
            transportControlGroup
            Spacer(minLength: 8)
            playbackOptionGroup
        }
    }

    #if os(iOS)
    /// Uses the Apple TV's single-row hierarchy whenever the touch viewport is
    /// wide enough (iPhone landscape and iPad), with the same hierarchy split
    /// over two rows only when a portrait phone cannot fit it safely.
    private var touchPlaybackRows: some View {
        ViewThatFits(in: .horizontal) {
            PlayerTouchWideRow {
                transportControlGroup
            } timeline: {
                if controller.knownDurationMs > 0 {
                    playbackTimeLabel(Int(isScrubbing ? scrubMs : Double(controller.currentMs)))
                    touchProgressSlider
                        .frame(minWidth: 100)
                    playbackTimeLabel(controller.knownDurationMs)
                }
            } options: {
                playbackOptionGroup
            }

            VStack(alignment: .leading, spacing: 8) {
                if controller.knownDurationMs > 0 {
                    HStack(spacing: 10) {
                        playbackTimeLabel(Int(isScrubbing ? scrubMs : Double(controller.currentMs)))
                        touchProgressSlider
                        playbackTimeLabel(controller.knownDurationMs)
                    }
                }
                ViewThatFits(in: .horizontal) {
                    expandedControlRow
                        .fixedSize(horizontal: true, vertical: false)
                    compactControlRow
                }
                .frame(maxWidth: .infinity)
            }
        }
        .font(.system(.caption, design: .monospaced))
        .foregroundColor(.white)
    }

    private var touchProgressSlider: some View {
        Slider(
            value: Binding(
                get: { isScrubbing ? scrubMs : Double(controller.currentMs) },
                set: { scrubMs = $0 }
            ),
            in: 0...Double(controller.knownDurationMs),
            onEditingChanged: { editing in
                if editing {
                    scrubMs = Double(controller.currentMs)
                    isScrubbing = true
                } else {
                    // Seek first: it publishes the target into
                    // `controller.currentMs` synchronously, so the binding's
                    // fallback never flashes the pre-scrub position.
                    controller.seek(toMs: Int(scrubMs))
                    isScrubbing = false
                }
            }
        )
        .tint(Palette.accent)
    }
    #endif

    private var transportControlGroup: some View {
        HStack(spacing: 8) {
            skipBackButton
            playPauseButton
            skipForwardButton
        }
    }

    private var playbackOptionGroup: some View {
        HStack(spacing: 8) {
            if pictureInPicture.isSupported { pictureInPictureButton }
            if controller.audioTracks.count > 1 { audioMenu }
            if !controller.subtitles.isEmpty { subtitleMenu }
            if !controller.qualityRungs.isEmpty { qualityMenu }
            autoplayButton
            statsButton
        }
    }

    #if os(iOS)
    private var compactControlRow: some View {
        HStack(spacing: 8) {
            skipBackButton
            playPauseButton
            skipForwardButton
            Spacer(minLength: 4)
            if pictureInPicture.isSupported { pictureInPictureButton }
            moreMenu
        }
        .frame(maxWidth: .infinity)
    }
    #endif

    private var skipBackButton: some View {
        Button {
            controller.skip(seconds: -10)
            revealControls()
        } label: {
            Image(systemName: "gobackward.10")
        }
        .accessibilityLabel("Back 10 seconds")
        #if os(tvOS)
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .skipBack)
        #endif
    }

    private var playPauseButton: some View {
        Button {
            controller.togglePlayPause()
            revealControls()
        } label: {
            Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill")
                .frame(minWidth: 20)
        }
        .accessibilityLabel(controller.isPlaying ? "Pause" : "Play")
        #if os(tvOS)
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .playPause)
        #endif
    }

    private var skipForwardButton: some View {
        Button {
            controller.skip(seconds: 10)
            revealControls()
        } label: {
            Image(systemName: "goforward.10")
        }
        .accessibilityLabel("Forward 10 seconds")
        #if os(tvOS)
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .skipForward)
        #endif
    }

    private var pictureInPictureButton: some View {
        let controlState = pictureInPicture.controlState
        return Button {
            guard controller.allowsPictureInPictureCommand() else { return }
            pictureInPicture.toggle()
            revealControls()
        } label: {
            Image(systemName: pictureInPicture.isActive ? "pip.exit" : "pip.enter")
                .foregroundStyle(pictureInPicture.isActive ? Palette.accent : .white)
        }
        .disabled(!controlState.isButtonEnabled)
        .accessibilityLabel(pictureInPicture.isActive
                            ? "Stop Picture in Picture"
                            : "Start Picture in Picture")
        #if os(tvOS)
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .pictureInPicture)
        #endif
    }

    private var autoplayButton: some View {
        Button {
            model.setAutoplay(!model.autoplay)
            revealControls()
        } label: {
            Image(systemName: "play.square.stack.fill")
                .foregroundStyle(model.autoplay ? Palette.accent : .white)
        }
        .accessibilityLabel(model.autoplay ? "Autoplay next on" : "Autoplay next off")
        #if os(tvOS)
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .autoplay)
        #endif
    }

    private var statsButton: some View {
        Button {
            withAnimation { showStats.toggle() }
            revealControls()
        } label: {
            Image(systemName: "info.circle.fill")
                .foregroundStyle(showStats ? Palette.accent : .white)
        }
        .accessibilityLabel("Playback info")
        #if os(tvOS)
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .stats)
        #endif
    }

    #if os(iOS)
    private var moreMenu: some View {
        Button {
            presentOptionMenu(.more)
        } label: {
            Image(systemName: "ellipsis.circle.fill")
        }
        .accessibilityLabel("More playback options")
        .popover(isPresented: optionMenuBinding(.more), arrowEdge: .bottom) {
            optionMenuPanel("Playback options") {
                if controller.audioTracks.count > 1 {
                    optionMenuSection("Audio") { audioChoices }
                }
                if !controller.subtitles.isEmpty {
                    optionMenuSection("Subtitles") { subtitleChoices }
                }
                if !controller.qualityRungs.isEmpty {
                    optionMenuSection("Quality") { qualityChoices }
                }
                Divider()
                Button {
                    model.setAutoplay(!model.autoplay)
                    dismissOptionMenu()
                    revealControls()
                } label: {
                    Label(model.autoplay ? "Turn off autoplay" : "Turn on autoplay",
                          systemImage: "play.square.stack.fill")
                }
                Button {
                    withAnimation { showStats.toggle() }
                    dismissOptionMenu()
                    revealControls()
                } label: {
                    Label(showStats ? "Hide playback info" : "Playback info",
                          systemImage: "info.circle.fill")
                }
            }
        }
    }
    #endif

    #if os(tvOS)
    /// SwiftUI's Slider is unavailable on tvOS. This focusable bar uses the
    /// Siri Remote's left/right commands as ordinary focus navigation until
    /// Select engages scrubbing; engaged presses move through the same
    /// absolute film timeline in 10-second steps. Up reaches a visible skip
    /// marker above the transport row and is inert when no marker is present;
    /// Down returns to play/pause. This is not a Button:
    /// tvOS adds a large white pressed/focus surround to Buttons even when the
    /// ordinary focus effect is disabled.
    private var tvProgressBar: some View {
        GeometryReader { geometry in
            let fraction = controller.knownDurationMs > 0
                ? min(max(Double(controller.currentMs) / Double(controller.knownDurationMs), 0), 1)
                : 0
            ZStack(alignment: .leading) {
                Capsule().fill(.white.opacity(0.25))
                Capsule()
                    .fill(Palette.accent)
                    .frame(width: geometry.size.width * fraction)
            }
            .overlay {
                if focusedControl == .progress {
                    ZStack {
                        Capsule().stroke(
                            .black.opacity(0.96),
                            lineWidth: TVPlayerProgressFocusRing.outerStrokeWidth
                        )
                        Capsule().stroke(
                            Palette.accent.opacity(0.34),
                            lineWidth: TVPlayerProgressFocusRing.fadeStrokeWidth
                        )
                        Capsule().stroke(
                            Palette.accent,
                            lineWidth: TVPlayerProgressFocusRing.accentStrokeWidth
                        )
                    }
                }
            }
            .scaleEffect(y: tvProgressEngaged ? 1.5 : 1)
            .animation(.easeOut(duration: 0.12), value: tvProgressEngaged)
        }
        .frame(height: 8)
        .contentShape(Rectangle())
        .focusable()
        .focusEffectDisabled()
        .focused($focusedControl, equals: .progress)
        .onTapGesture {
            tvProgressEngaged.toggle()
            revealControls()
        }
        .onMoveCommand { direction in
            guard let seekDirection = Self.playerSeekDirection(direction) else { return }
            applyRemoteMoveOutcome(TVPlayerRemoteRouting.moveOutcome(
                focusedControl: .progress,
                progressEngaged: tvProgressEngaged,
                direction: seekDirection,
                progressRightNeighbor: progressRightControl,
                markerAvailable: controller.activeMarker != nil
            ))
        }
        .accessibilityLabel("Playback position")
        .accessibilityValue(tvProgressEngaged ? "Scrubbing" : "Not scrubbing")
        .accessibilityHint(tvProgressEngaged
            ? "Left or right seeks 10 seconds. Press Select or Menu to finish."
            : "Press Select to scrub. Left or right moves between controls.")
    }

    private var progressRightControl: PlayerControl {
        if pictureInPicture.isSupported,
           pictureInPicture.isActive || pictureInPicture.isPossible {
            return .pictureInPicture
        }
        if controller.audioTracks.count > 1 { return .audio }
        if !controller.subtitles.isEmpty { return .subtitles }
        if !controller.qualityRungs.isEmpty { return .quality }
        return .autoplay
    }
    #endif

    private var audioMenu: some View {
        #if os(iOS)
        Button {
            presentOptionMenu(.audio)
        } label: {
            Image(systemName: "speaker.wave.2.fill")
        }
        .accessibilityLabel("Audio track")
        .popover(isPresented: optionMenuBinding(.audio), arrowEdge: .bottom) {
            optionMenuPanel("Audio") { audioChoices }
        }
        #else
        Menu { audioChoices } label: {
            Image(systemName: "speaker.wave.2.fill")
        }
        .accessibilityLabel("Audio track")
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .audio)
        #endif
        }

    private var subtitleMenu: some View {
        #if os(iOS)
        Button {
            presentOptionMenu(.subtitles)
        } label: {
            Image(systemName: "captions.bubble.fill")
                .foregroundStyle(controller.selectedSubtitle == nil ? .white : Palette.accent)
        }
        .accessibilityLabel("Subtitles")
        .popover(isPresented: optionMenuBinding(.subtitles), arrowEdge: .bottom) {
            optionMenuPanel("Subtitles") { subtitleChoices }
        }
        #else
        Menu { subtitleChoices } label: {
            Image(systemName: "captions.bubble.fill")
                .foregroundStyle(controller.selectedSubtitle == nil ? .white : Palette.accent)
        }
        .accessibilityLabel("Subtitles")
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .subtitles)
        #endif
        }

    private var qualityMenu: some View {
        #if os(iOS)
        Button {
            presentOptionMenu(.quality)
        } label: {
            Image(systemName: "slider.horizontal.3")
        }
        .accessibilityLabel("Playback quality")
        .popover(isPresented: optionMenuBinding(.quality), arrowEdge: .bottom) {
            optionMenuPanel("Playback quality") { qualityChoices }
        }
        #else
        Menu { qualityChoices } label: {
            Image(systemName: "slider.horizontal.3")
        }
        .accessibilityLabel("Playback quality")
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .quality)
        #endif
        }

    #if os(iOS)
    private func optionMenuBinding(_ menu: PlayerOptionMenu) -> Binding<Bool> {
        Binding(
            get: { activeOptionMenu == menu },
            set: { presented in
                activeOptionMenu = presented ? menu : nil
            }
        )
    }

    private func presentOptionMenu(_ menu: PlayerOptionMenu) {
        activeOptionMenu = menu
        revealControls()
    }

    private func dismissOptionMenu() {
        activeOptionMenu = nil
    }

    private func optionMenuPanel<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.headline)
                    .padding(.horizontal, 10)
                    .padding(.bottom, 4)
                content()
            }
            .buttonStyle(PlayerOptionMenuButtonStyle())
            .padding(8)
        }
        .frame(minWidth: 260, idealWidth: 320, maxWidth: 380, maxHeight: 430)
        // The player chrome is always white, but a popover can use light material.
        // Use an explicit color style so the popover cannot inherit that white.
        .foregroundStyle(Color(uiColor: PlayerOptionMenuPalette.foreground))
        .presentationCompactAdaptation(.popover)
    }

    private func optionMenuSection<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        Group {
            Text(title.uppercased())
                .font(.caption.weight(.semibold))
                .foregroundStyle(Color(uiColor: PlayerOptionMenuPalette.secondaryForeground))
                .padding(.horizontal, 10)
                .padding(.top, 4)
            content()
            Divider()
        }
    }
    #endif

    @ViewBuilder
    private var audioChoices: some View {
        ForEach(controller.audioTracks) { track in
            Button {
                controller.selectAudio(track.index)
                #if os(iOS)
                dismissOptionMenu()
                #endif
                revealControls()
            } label: {
                Label(
                    audioLabel(track),
                    systemImage: controller.selectedAudio == track.index
                        ? "checkmark"
                        : "speaker.wave.2"
                )
            }
        }
    }

    @ViewBuilder
    private var subtitleChoices: some View {
        Button {
            controller.selectSubtitle(nil)
            #if os(iOS)
            dismissOptionMenu()
            #endif
            revealControls()
        } label: {
            Label(
                "Off",
                systemImage: controller.selectedSubtitle == nil
                    ? "checkmark"
                    : "captions.bubble"
            )
        }
        ForEach(controller.subtitles) { track in
            Button {
                controller.selectSubtitle(track.index)
                #if os(iOS)
                dismissOptionMenu()
                #endif
                revealControls()
            } label: {
                Label(
                    Self.subtitleLabel(track),
                    systemImage: controller.selectedSubtitle == track.index
                        ? "checkmark"
                        : "captions.bubble"
                )
            }
        }
    }

    @ViewBuilder
    private var qualityChoices: some View {
        Button {
            controller.selectQuality(nil)
            #if os(iOS)
            dismissOptionMenu()
            #endif
            revealControls()
        } label: {
            Label(
                "Auto",
                systemImage: controller.selectedHeight == nil
                    ? "checkmark"
                    : "wand.and.stars"
            )
        }
        ForEach(controller.qualityRungs) { rung in
            Button {
                controller.selectQuality(rung.height)
                #if os(iOS)
                dismissOptionMenu()
                #endif
                revealControls()
            } label: {
                Label(
                    "\(rung.height)p  \(rung.totalKbps / 1000) Mb/s",
                    systemImage: controller.selectedHeight == rung.height
                        ? "checkmark"
                        : "rectangle.inset.filled"
                )
            }
        }
    }

    private func audioLabel(_ track: AudioTrack) -> String {
        [track.title, Self.languageName(track.language), track.codec.uppercased(),
         track.channels.map { "\($0) ch" }]
            .compactMap { $0 }
            .joined(separator: "  ")
    }

    static func subtitleLabel(_ track: SubtitleTrack) -> String {
        var parts = [track.title, languageName(track.language), track.codec.uppercased()]
            .compactMap { $0 }
        if track.forced { parts.append("Forced") }
        if track.isPGSOverlay {
            parts.append("Overlay")
        } else if !track.isNativeHLS {
            parts.append("Burn-in")
        }
        return parts.joined(separator: "  ")
    }

    private static func languageName(_ code: String?) -> String? {
        guard let code else { return nil }
        let names = [
            "eng": "English", "en": "English", "jpn": "Japanese", "ja": "Japanese",
            "spa": "Spanish", "es": "Spanish", "fre": "French", "fr": "French",
            "ger": "German", "de": "German", "ita": "Italian", "por": "Portuguese",
            "kor": "Korean", "chi": "Chinese", "rus": "Russian",
        ]
        return names[code.lowercased()] ?? code.uppercased()
    }
}

enum PlaybackStatsMode: String, CaseIterable, Identifiable {
    case mini
    case standard
    case debug

    var id: Self { self }

    var label: String {
        switch self {
        case .mini: return "Mini"
        case .standard: return "Standard"
        case .debug: return "Debug"
        }
    }
}

private enum PlaybackStatTone {
    case neutral
    case muted
    case good
    case warning
    case critical

    var color: Color {
        switch self {
        case .neutral: return .white.opacity(0.92)
        case .muted: return .white.opacity(0.55)
        case .good: return Color(red: 0.42, green: 0.86, blue: 0.61)
        case .warning: return Color(red: 1.0, green: 0.74, blue: 0.29)
        case .critical: return Color(red: 1.0, green: 0.37, blue: 0.39)
        }
    }
}

/// Where a ledger section sits in the panel. The column is assigned
/// structurally rather than measured, so the rendered order is always the
/// declared order no matter how long the values happen to be this second.
private enum PlaybackLedgerColumn: Equatable {
    case left
    case right
}

/// Whether a row belongs in the two-column grid or in the notes strip that
/// spans the panel underneath it. Sentence-shaped values leave the grid so
/// they can never stretch a column.
private enum PlaybackLedgerPlacement: Equatable {
    case grid
    case notes
}

/// One label/value pair. Rows are plain data rather than views so the
/// alignment and density decisions below can inspect them before anything is
/// laid out.
private struct PlaybackLedgerRow: Identifiable {
    var section: String = ""
    let label: String
    let value: String
    var tone: PlaybackStatTone = .neutral
    var placement: PlaybackLedgerPlacement = .grid

    var id: String { "\(section)·\(label)" }

    /// Values that read as a sentence rather than a datum. Membership here is
    /// the only thing that routes a row out of the grid, so a row's column is
    /// fixed by what it is, not by what it happens to be carrying this second.
    static let noteLabels: Set<String> = [
        "Session",
        "Reason",
        "Video",
        "Audio",
        "Dynamic range",
        "Transport",
        "Waiting reason",
        "Last request",
    ]

    /// Placement is decided by label alone. There is deliberately no length
    /// test: a value that grows a clause — `Server ahead` picking up
    /// `· held · bytes release ≤120 MB` the moment the session suspends —
    /// would otherwise hop between the grid and the notes strip on every
    /// two-second status poll and drag its section's alignment verdict with
    /// it. Builders whose value is a short datum plus an optional long clause
    /// split the two into a fixed grid row and a fixed note instead; see
    /// `ledgerSplitRows`. Builders whose value is prose outright ask for
    /// `ledgerNote` by construction.
    static func resolvedPlacement(label: String) -> PlaybackLedgerPlacement {
        noteLabels.contains(label) ? .notes : .grid
    }
}

/// A section aligns as a unit: a column of numbers reads as a column only if
/// every value in it shares the same edge and the same digit width.
private struct PlaybackLedgerSection: Identifiable {
    let id: String
    let title: String
    let column: PlaybackLedgerColumn
    let placeholder: String?
    /// Set on sections whose heading carries meaning the notes strip cannot.
    /// SERVER on cached VOD holds exactly one sentence-shaped row: without
    /// this the whole box disappears and "already transcoded" surfaces under
    /// Notes, which reads as "no server section" rather than "cached".
    let keepsNotesInBox: Bool
    let rows: [PlaybackLedgerRow]

    init(
        _ title: String,
        column: PlaybackLedgerColumn,
        placeholder: String? = nil,
        keepsNotesInBox: Bool = false,
        rows: [PlaybackLedgerRow]
    ) {
        self.id = title
        self.title = title
        self.column = column
        self.placeholder = placeholder
        self.keepsNotesInBox = keepsNotesInBox
        self.rows = rows.map { row in
            var copy = row
            copy.section = title
            return copy
        }
    }

    var gridRows: [PlaybackLedgerRow] {
        rows.filter { $0.placement == .grid }
    }

    var noteRows: [PlaybackLedgerRow] {
        rows.filter { $0.placement == .notes }
    }

    /// True when this section draws its own note rows inside its box instead
    /// of sending them down to the shared strip. Deliberately independent of
    /// whether the section also has grid rows: one unrelated short row — a
    /// selected subtitle track, say — arriving in SERVER must not evict the
    /// cached-VOD sentence this flag exists to keep in the box.
    var ownsNotes: Bool {
        keepsNotesInBox && placeholder == nil && !noteRows.isEmpty
    }

    /// The note rows that reach the strip under the columns.
    var stripNoteRows: [PlaybackLedgerRow] {
        ownsNotes ? [] : noteRows
    }

    /// A section box is drawn only when it has grid rows to show, a
    /// placeholder to explain their absence, or notes it keeps for itself.
    /// A section that contributes only notes otherwise reaches the notes
    /// strip.
    var rendersBox: Bool {
        !gridRows.isEmpty || placeholder != nil || ownsNotes
    }

    var prefersNumericAlignment: Bool {
        let visible = gridRows
        guard !visible.isEmpty else { return false }
        let numeric = visible.filter { PlaybackLedgerSection.isNumeric($0.value) }.count
        return Double(numeric) >= Double(visible.count) * 0.7
    }

    /// Sixteen short server values waste two thirds of a television column as
    /// one list, so a long section of short values folds into two sub-columns.
    var prefersDenseColumns: Bool {
        let visible = gridRows
        return visible.count >= 10 && visible.allSatisfy { $0.value.count <= 12 }
    }

    /// The em dash counts as numeric so a section does not change alignment
    /// the moment a value it is still waiting for arrives.
    static func isNumeric(_ value: String) -> Bool {
        if value == "—" { return true }
        guard let first = value.first else { return false }
        return first.isNumber
    }
}

/// The same three playback-info levels used by the web and Android players.
/// Each client renders them natively, but Mini, Standard, and Debug keep the
/// same job and information hierarchy on every screen size.
///
/// Standard and Debug share one ledger: a header, two structurally assigned
/// columns of label/value rows, and a notes strip for the sentence-shaped
/// values. Only the field set and a handful of platform metrics change
/// between them, so switching mode grows the same block downward from the
/// same top-trailing corner instead of relaying the screen.
private struct PlaybackStatsView: View {
    @ObservedObject var controller: PlayerController
    @Binding var mode: PlaybackStatsMode
    let onDismiss: () -> Void

    #if os(tvOS)
    @FocusState private var dismissFocused: Bool
    #endif

    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif

    /// This panel floats over the video, so its contrast must not follow the
    /// app's light/dark palette. A fixed dark surface keeps the white copy
    /// readable in every appearance and over both bright and dark frames.
    /// It also carries the whole contrast job — there is no scrim behind it.
    private let panelSurface = Palette.playerChrome.opacity(0.96)
    private let labelColor = Color.white.opacity(0.82)

    var body: some View {
        Group {
            if mode == .mini {
                miniBody
            } else {
                ledgerBody
            }
        }
    }

    private var modeSelector: some View {
        HStack(spacing: 6) {
            ForEach(PlaybackStatsMode.allCases) { candidate in
                Button {
                    withAnimation(.easeInOut(duration: 0.16)) { mode = candidate }
                } label: {
                    Text(candidate.label)
                        .font(.system(size: modeFontSize, weight: .semibold, design: .rounded))
                        .padding(.horizontal, modeHorizontalPadding)
                        .padding(.vertical, modeVerticalPadding)
                        .foregroundStyle(mode == candidate ? .white : .white.opacity(0.62))
                        .background(
                            mode == candidate ? Palette.accent : Color.white.opacity(0.07),
                            in: Capsule()
                        )
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(candidate.label) playback info")
                .accessibilityAddTraits(mode == candidate ? .isSelected : [])
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Playback info size")
    }

    private var modeFontSize: CGFloat {
        #if os(tvOS)
        15
        #else
        11
        #endif
    }

    private var modeHorizontalPadding: CGFloat {
        #if os(tvOS)
        13
        #else
        9
        #endif
    }

    private var modeVerticalPadding: CGFloat {
        #if os(tvOS)
        7
        #else
        5
        #endif
    }

    private var closeButton: some View {
        Button(action: onDismiss) {
            Image(systemName: "xmark")
                .font(.system(size: modeFontSize, weight: .bold))
                .padding(modeVerticalPadding)
                .foregroundStyle(.white.opacity(0.72))
                .background(.white.opacity(0.07), in: Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Close playback info")
    }

    // MARK: - Mini

    /// One horizontal line inside a bounded panel: method, position, the
    /// three facts, and a health pill, all separated by hairlines.
    private var miniBody: some View {
        miniShrinkWrap(
            HStack(spacing: miniSpacing) {
                Text(controller.methodLabel)
                    .font(.system(size: miniTitleSize, weight: .bold, design: .rounded))
                    .foregroundStyle(.white)
                    .lineLimit(1)

                miniDivider

                Text("\(formatTime(controller.currentMs)) / \(formatTime(controller.knownDurationMs))")
                    .font(.system(size: miniDetailSize, weight: .medium, design: .monospaced))
                    .foregroundStyle(.white.opacity(0.62))
                    .lineLimit(1)
                    .minimumScaleFactor(0.72)

                ForEach(miniFacts) { fact in
                    miniDivider
                    miniFact(fact)
                }

                miniDivider

                miniHealth
                miniControls
            }
            .padding(.horizontal, miniHorizontalPadding)
            .padding(.vertical, miniVerticalPadding)
        )
        .background(panelSurface, in: RoundedRectangle(cornerRadius: miniCornerRadius))
        .overlay {
            RoundedRectangle(cornerRadius: miniCornerRadius)
                .stroke(.white.opacity(0.12), lineWidth: 1)
        }
        .shadow(color: .black.opacity(0.45), radius: 24, y: 12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        .padding(miniOuterPadding)
    }

    /// The controls are the one part of the line that must never compress,
    /// so they carry the layout priority and the facts absorb the squeeze.
    private var miniControls: some View {
        HStack(spacing: miniSpacing) {
            modeSelector
            closeButton
        }
        .layoutPriority(1)
    }

    /// The line is bounded, never free-running. `fixedSize` used to stand
    /// here on the television, which proposed every child its ideal width and
    /// so made the `lineLimit` and `minimumScaleFactor` on the fact values
    /// inert: a long method label beside a long summary simply kept growing,
    /// and because the panel is trailing-aligned it bled off the leading
    /// edge. A width bound gives those modifiers something to work against
    /// again — the values scale and truncate inside it instead.
    private func miniShrinkWrap<V: View>(_ view: V) -> some View {
        view.frame(maxWidth: miniMaxWidth)
    }

    private var miniDivider: some View {
        Rectangle()
            .fill(Color.white.opacity(0.16))
            .frame(width: 1, height: miniDividerHeight)
            .accessibilityHidden(true)
    }

    private var miniFacts: [PlaybackLedgerRow] {
        [
            PlaybackLedgerRow(label: "Playing", value: miniPlayingSummary, tone: playbackTone),
            PlaybackLedgerRow(label: "Buffer", value: miniBufferSummary, tone: bufferTone),
            PlaybackLedgerRow(label: "Network", value: miniNetworkSummary, tone: networkTone),
        ]
    }

    private func miniFact(_ fact: PlaybackLedgerRow) -> some View {
        HStack(spacing: 5) {
            Text(fact.label.uppercased())
                .font(.system(size: miniLabelSize, weight: .bold, design: .rounded))
                .tracking(0.8)
                .foregroundStyle(.white.opacity(0.42))
                .lineLimit(1)
            Text(fact.value)
                .font(.system(size: miniDetailSize, weight: .semibold, design: .rounded))
                .foregroundStyle(fact.tone.color)
                .lineLimit(1)
                .minimumScaleFactor(0.72)
        }
        .accessibilityElement(children: .combine)
    }

    private var miniHealth: some View {
        let stalls = controller.stalls ?? 0
        return HStack(spacing: 5) {
            Circle()
                .fill(stalls > 0 ? Color.orange : Color.green)
                .frame(width: miniHealthDotSize, height: miniHealthDotSize)
            Text(stalls > 0 ? "\(stalls) stall\(stalls == 1 ? "" : "s")" : "Healthy")
                .font(.system(size: miniDetailSize, weight: .semibold, design: .rounded))
                .foregroundStyle(playbackTone.color)
                .lineLimit(1)
        }
        .padding(.horizontal, miniPillHorizontalPadding)
        .padding(.vertical, miniPillVerticalPadding)
        .background(.white.opacity(0.07), in: Capsule())
        .accessibilityElement(children: .combine)
    }

    private var miniPlayingSummary: String {
        let size = controller.presentationSize
        let resolution = size.width > 0 && size.height > 0
            ? "\(Int(size.width))×\(Int(size.height))"
            : "Waiting"
        let range = PlayerView.dynamicRangeSummary(
            source: controller.decision?.source,
            delivered: controller.deliveredRange,
            displayHDR: Caps.displayIsHDR,
            reasons: []
        )
        return [resolution, range?.components(separatedBy: " — ").first]
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    private var miniBufferSummary: String {
        if let runway = controller.bufferedRunwaySeconds() {
            return String(format: "%.1f s ahead", runway)
        }
        return "Measuring"
    }

    private var miniNetworkSummary: String {
        if let delivered = controller.sessionStatus?.deliveredBps, delivered > 0 {
            return bitRate(delivered)
        }
        if let observed = controller.observedBitrate, observed > 0 {
            return bitRate(Int(observed))
        }
        return "Measuring"
    }

    private var miniSpacing: CGFloat {
        #if os(tvOS)
        12
        #else
        7
        #endif
    }

    private var miniTitleSize: CGFloat {
        #if os(tvOS)
        21
        #else
        14
        #endif
    }

    private var miniDetailSize: CGFloat {
        #if os(tvOS)
        14
        #else
        10
        #endif
    }

    private var miniLabelSize: CGFloat {
        #if os(tvOS)
        11
        #else
        8
        #endif
    }

    private var miniHealthDotSize: CGFloat {
        #if os(tvOS)
        8
        #else
        6
        #endif
    }

    private var miniDividerHeight: CGFloat {
        #if os(tvOS)
        22
        #else
        14
        #endif
    }

    private var miniPillHorizontalPadding: CGFloat {
        #if os(tvOS)
        11
        #else
        8
        #endif
    }

    private var miniPillVerticalPadding: CGFloat {
        #if os(tvOS)
        5
        #else
        3
        #endif
    }

    private var miniHorizontalPadding: CGFloat {
        #if os(tvOS)
        18
        #else
        11
        #endif
    }

    private var miniVerticalPadding: CGFloat {
        #if os(tvOS)
        12
        #else
        9
        #endif
    }

    /// The upper bound on the shrink-wrapped line. Without it the values'
    /// `minimumScaleFactor` never engages, because nothing ever proposes the
    /// line a width narrower than it asked for.
    private var miniMaxWidth: CGFloat {
        #if os(tvOS)
        1_240
        #else
        560
        #endif
    }

    private var miniCornerRadius: CGFloat {
        #if os(tvOS)
        16
        #else
        10
        #endif
    }

    private var miniOuterPadding: CGFloat {
        #if os(tvOS)
        36
        #else
        12
        #endif
    }

    // MARK: - Ledger shell

    /// Anchored to the top-trailing corner in every mode and clipped short of
    /// the transport controls, so growing from Standard to Debug extends the
    /// same block downward instead of moving it.
    private var ledgerBody: some View {
        GeometryReader { geometry in
            let insets = geometry.safeAreaInsets
            let usableWidth = max(
                0,
                geometry.size.width - insets.leading - insets.trailing - (ledgerEdgeInset * 2)
            )
            let usableHeight = max(
                0,
                geometry.size.height - insets.top - insets.bottom
                    - (ledgerEdgeInset * 2) - ledgerTransportReserve
            )
            let panelWidth = max(0, min(ledgerMaxWidth, usableWidth * ledgerWidthFraction))
            let panelHeight = max(0, min(ledgerMaxHeight, usableHeight))

            ZStack {
                ledgerBackdrop

                ledgerInitialFocus(
                    ledgerPanel
                        .frame(width: panelWidth)
                        .frame(maxHeight: panelHeight, alignment: .top)
                        .padding(.top, insets.top + ledgerEdgeInset)
                        .padding(.trailing, insets.trailing + ledgerEdgeInset)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
                )
            }
        }
    }

    /// The visual scrim is gone — the panel carries the whole contrast job —
    /// but on the phone the scrim was also doing layout work: Debug was a
    /// modal surface, and the old `Color.black.opacity(0.52)` absorbed every
    /// tap that missed the panel. Without something hit-testable those taps
    /// now reach the transport controls behind it. The old scrim carried no
    /// tap gesture, so neither does this: taps are swallowed, not acted on.
    /// Standard never had a scrim on the phone and does not get one here.
    /// The television keeps focus out of the controls by disabling them at
    /// the call site instead, so it needs nothing.
    @ViewBuilder
    private var ledgerBackdrop: some View {
        #if os(iOS)
        if mode == .debug {
            Color.clear
                .contentShape(Rectangle())
                .ignoresSafeArea()
                .accessibilityHidden(true)
        }
        #else
        EmptyView()
        #endif
    }

    /// The remote lands on Done in every mode. Debug used to wire no initial
    /// focus at all, which left the panel unreachable.
    private func ledgerInitialFocus<V: View>(_ view: V) -> some View {
        #if os(tvOS)
        return view.onAppear {
            Task { @MainActor in
                await Task.yield()
                dismissFocused = true
            }
        }
        #else
        return view
        #endif
    }

    private var ledgerPanel: some View {
        VStack(alignment: .leading, spacing: ledgerSpacing) {
            let sections = mode == .debug ? debugSections : standardSections

            ledgerHeader

            Divider().overlay(.white.opacity(0.12))

            // The ledger is meant to fit without scrolling — the panel then
            // shrink-wraps to its content instead of reserving the whole
            // height. The ScrollView is the safety net for smaller outputs.
            ViewThatFits(in: .vertical) {
                ledgerColumns(sections)
                ScrollView { ledgerColumns(sections) }
            }
        }
        .padding(ledgerPanelPadding)
        .background(
            panelSurface,
            in: RoundedRectangle(cornerRadius: ledgerCornerRadius, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: ledgerCornerRadius, style: .continuous)
                .stroke(.white.opacity(0.13), lineWidth: 1)
        }
        .shadow(color: .black.opacity(0.58), radius: 28, y: 12)
    }

    private var ledgerHeader: some View {
        HStack(alignment: .center, spacing: ledgerSpacing) {
            ledgerTitleBlock
            Spacer(minLength: ledgerSpacing)
            ledgerHeaderHealth
            modeSelector
            ledgerDismissControl
        }
    }

    private var ledgerTitleBlock: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(mode == .debug ? "Playback debug" : "Playback info")
                .font(ledgerTitleFont)
                .foregroundStyle(.white)
            ledgerSubtitle
        }
    }

    @ViewBuilder
    private var ledgerSubtitle: some View {
        #if os(tvOS)
        if mode == .standard {
            HStack(spacing: 8) {
                Text(controller.methodLabel)
                    .font(.system(size: 18, weight: .semibold, design: .rounded))
                    .foregroundStyle(Palette.accent)

                Text("·")
                    .foregroundStyle(.white.opacity(0.35))

                Text("\(formatTime(controller.currentMs)) of \(formatTime(controller.knownDurationMs))")
                    .font(.system(size: 17, weight: .medium, design: .rounded))
                    .foregroundStyle(.white.opacity(0.72))
            }
        }
        #else
        if mode == .debug {
            Text("Live player, network, and server diagnostics")
                .font(.system(size: ledgerDetailSize, weight: .medium, design: .rounded))
                .foregroundStyle(.white.opacity(0.56))
        }
        #endif
    }

    @ViewBuilder
    private var ledgerHeaderHealth: some View {
        #if os(tvOS)
        if mode == .standard {
            playbackHealth
        }
        #else
        EmptyView()
        #endif
    }

    @ViewBuilder
    private var ledgerDismissControl: some View {
        #if os(tvOS)
        Button(action: onDismiss) {
            Label("Done", systemImage: "xmark")
                .font(.system(size: 17, weight: .semibold, design: .rounded))
        }
        .buttonStyle(TVReadableButtonStyle(prominent: false))
        .focused($dismissFocused)
        .accessibilityLabel("Close playback info")
        #else
        closeButton
        #endif
    }

    #if os(tvOS)
    private var playbackHealth: some View {
        let stalls = controller.stalls
        let label = TVPlaybackInfoPresentation.healthLabel(stalls: stalls)
        let color: Color
        if let stalls, stalls > 0 {
            color = .orange
        } else if stalls == 0 {
            color = .green
        } else {
            color = .white.opacity(0.6)
        }
        return HStack(spacing: 9) {
            Circle()
                .fill(color)
                .frame(width: 11, height: 11)
            Text(label)
                .font(.system(size: 16, weight: .semibold, design: .rounded))
                .foregroundStyle(playbackTone.color)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .background(.white.opacity(0.07), in: Capsule())
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Playback health, \(label)")
    }
    #endif

    // MARK: - Ledger layout

    @ViewBuilder
    private func ledgerColumns(_ sections: [PlaybackLedgerSection]) -> some View {
        VStack(alignment: .leading, spacing: ledgerSpacing) {
            if ledgerUsesTwoColumns {
                HStack(alignment: .top, spacing: ledgerColumnGap) {
                    ledgerColumn(sections.filter { $0.column == .left })
                    ledgerColumn(sections.filter { $0.column == .right })
                }
            } else {
                ledgerColumn(
                    sections.filter { $0.column == .left }
                        + sections.filter { $0.column == .right }
                )
            }

            ledgerNotes(sections)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func ledgerColumn(_ sections: [PlaybackLedgerSection]) -> some View {
        VStack(alignment: .leading, spacing: ledgerSpacing) {
            ForEach(sections.filter { $0.rendersBox }) { section in
                ledgerSection(section)
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    private func ledgerSection(_ section: PlaybackLedgerSection) -> some View {
        ledgerFocusable(
            VStack(alignment: .leading, spacing: ledgerRowSpacing) {
                Text(section.title.uppercased())
                    .font(.system(size: ledgerSectionSize, weight: .bold, design: .rounded))
                    .tracking(1)
                    .foregroundStyle(Palette.accent)

                ledgerSectionRows(section)
            }
            .padding(ledgerSectionPadding)
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .background(
                .white.opacity(0.045),
                in: RoundedRectangle(cornerRadius: ledgerSectionCornerRadius, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: ledgerSectionCornerRadius, style: .continuous)
                    .stroke(.white.opacity(0.07), lineWidth: 1)
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(section.title)
        )
    }

    /// The ledger is sized to fit a 1080p canvas without scrolling, but the
    /// ScrollView stays as a safety net for smaller outputs. A section has to
    /// be focusable or the remote can never reach the part that scrolled off.
    private func ledgerFocusable<V: View>(_ view: V) -> some View {
        #if os(tvOS)
        return view.focusable(true)
        #else
        return view
        #endif
    }

    /// Grid rows first, then — only for a section that owns its notes — the
    /// notes it kept. The two are drawn from disjoint halves of the section
    /// (`gridRows` and `noteRows` partition `rows` on placement) and a section
    /// that owns its notes contributes none to the shared strip, so nothing
    /// here can draw a row twice or leave one undrawn.
    private func ledgerSectionRows(_ section: PlaybackLedgerSection) -> some View {
        let rows = section.gridRows
        return VStack(alignment: .leading, spacing: ledgerRowSpacing) {
            if rows.isEmpty {
                if let placeholder = section.placeholder {
                    Text(placeholder)
                        .font(ledgerValueFont)
                        .foregroundStyle(.white.opacity(0.48))
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                // With no grid rows and no placeholder the box exists only
                // because the section owns notes, which the block below draws.
                // A section with neither never draws a box in the first place.
            } else if ledgerAllowsDenseColumns && section.prefersDenseColumns {
                let split = (rows.count + 1) / 2
                HStack(alignment: .top, spacing: ledgerColumnGap) {
                    ledgerRowStack(Array(rows.prefix(split)), section: section, dense: true)
                    ledgerRowStack(Array(rows.dropFirst(split)), section: section, dense: true)
                }
            } else {
                ledgerRowStack(rows, section: section, dense: false)
            }

            if section.ownsNotes {
                ForEach(section.noteRows) { row in
                    ledgerNoteRow(row)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    private func ledgerRowStack(
        _ rows: [PlaybackLedgerRow],
        section: PlaybackLedgerSection,
        dense: Bool
    ) -> some View {
        VStack(alignment: .leading, spacing: ledgerRowSpacing) {
            ForEach(rows) { row in
                ledgerGridRow(row, numeric: section.prefersNumericAlignment, dense: dense)
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    /// Label in a fixed gutter, value filling the rest on the same baseline.
    /// Grid values are capped at a fixed number of lines so nothing here can
    /// stretch a column again — the long ones live in the notes strip.
    private func ledgerGridRow(
        _ row: PlaybackLedgerRow,
        numeric: Bool,
        dense: Bool
    ) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: ledgerRowGap) {
            Text(row.label)
                .font(ledgerLabelFont)
                .foregroundStyle(ledgerLabelColor)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(
                    width: dense ? ledgerDenseGutterWidth : ledgerGutterWidth,
                    alignment: .leading
                )

            ledgerValueText(row, numeric: numeric)
                .lineLimit(ledgerGridValueLineLimit)
                .truncationMode(.tail)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: numeric ? .trailing : .leading)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private func ledgerNotes(_ sections: [PlaybackLedgerSection]) -> some View {
        let notes = sections.flatMap { $0.stripNoteRows }
        if !notes.isEmpty {
            // The strip is the last child of the column stack, so on the
            // television it has to be focusable for the same reason a section
            // does: with nothing focusable below the final section box the
            // remote can never scroll far enough to reveal it.
            ledgerFocusable(
                VStack(alignment: .leading, spacing: ledgerRowSpacing) {
                    Divider().overlay(.white.opacity(0.12))

                    Text("Notes")
                        .font(.system(size: ledgerSectionSize, weight: .bold, design: .rounded))
                        .tracking(1)
                        .foregroundStyle(.white.opacity(0.5))

                    ForEach(notes) { row in
                        ledgerNoteRow(row)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityElement(children: .contain)
                .accessibilityLabel("Notes")
            )
        }
    }

    /// The gutter is the hanging indent: wrapped lines line up under the
    /// first line of the value, never under the label.
    private func ledgerNoteRow(_ row: PlaybackLedgerRow) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: ledgerRowGap) {
            Text(row.label)
                .font(ledgerLabelFont)
                .foregroundStyle(ledgerLabelColor)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(width: ledgerGutterWidth, alignment: .leading)

            ledgerValueText(row, numeric: false)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(ledgerNoteAccessibilityLabel(row))
    }

    /// The decision reason used to be a footer that announced itself as one
    /// sentence. As a note row the combined children would read out the bare
    /// word "Reason" first, so it keeps the footer's spoken form.
    private func ledgerNoteAccessibilityLabel(_ row: PlaybackLedgerRow) -> String {
        row.label == "Reason"
            ? "Playback reason, \(row.value)"
            : "\(row.label), \(row.value)"
    }

    private func ledgerValueText(_ row: PlaybackLedgerRow, numeric: Bool) -> some View {
        let text = Text(row.value)
            .font(numeric ? ledgerValueFont.monospacedDigit() : ledgerValueFont)
            .foregroundStyle(row.tone.color)
        #if os(iOS)
        return text.textSelection(.enabled)
        #else
        return text
        #endif
    }

    // MARK: - Debug field set

    private var debugSections: [PlaybackLedgerSection] {
        [
            PlaybackLedgerSection("Playback", column: .left, rows: debugPlaybackRows),
            PlaybackLedgerSection("Source", column: .left, rows: debugSourceRows),
            PlaybackLedgerSection("Now decoding", column: .left, rows: debugDecodingRows),
            PlaybackLedgerSection("Network", column: .right, rows: debugNetworkRows),
            PlaybackLedgerSection(
                "Server",
                column: .right,
                keepsNotesInBox: true,
                rows: debugServerRows
            ),
        ]
    }

    private var debugPlaybackRows: [PlaybackLedgerRow] {
        // The method label picks up qualifiers as the user works — a burned-in
        // subtitle, a PGS overlay, a cached transcode — so the mode is the
        // datum and the qualifiers are the clause.
        let method = ledgerSeam(controller.methodLabel)
        var rows: [PlaybackLedgerRow] = [ledgerRow("Build", buildLabel)]
        rows.append(contentsOf: ledgerSplitRows("Method", method.datum, clause: method.clause))
        rows.append(contentsOf: [
            ledgerRow(
                "Transport",
                controller.currentSessionId == nil
                    ? "Continuous file · range requests · Apple AVPlayer"
                    : "Segmented HLS · Apple AVPlayer"
            ),
            ledgerRow(
                "Position",
                "\(formatTime(controller.currentMs)) / \(formatTime(controller.knownDurationMs))"
            ),
            ledgerRow("File ID", controller.decision.map { "#\($0.fileId)" } ?? "—"),
        ])
        if let session = controller.currentSessionId {
            rows.append(ledgerRow("Session", session))
        }
        if let reasons = controller.decision?.reasons, !reasons.isEmpty {
            rows.append(ledgerRow("Reason", reasons.joined(separator: "; ")))
        }
        return rows
    }

    private var debugSourceRows: [PlaybackLedgerRow] {
        var rows: [PlaybackLedgerRow] = []
        if let source = controller.decision?.source {
            let video = [
                source.videoCodec?.uppercased(),
                source.videoProfile,
                source.bitDepth.map { "\($0)-bit" },
                source.hdrFormat ?? source.hdr?.uppercased(),
            ].compactMap { $0 }.joined(separator: " · ")
            rows.append(ledgerRow("Video", video.isEmpty ? "—" : video))
            rows.append(ledgerRow(
                "Resolution",
                source.width.flatMap { width in source.height.map { "\(width)×\($0)" } } ?? "—"
            ))
            rows.append(ledgerRow("Bitrate", source.bitrate.map(bitRate) ?? "—"))
            rows.append(ledgerRow("Container", source.container?.uppercased() ?? "—"))
        }
        if let audio = selectedAudioDescription {
            rows.append(ledgerRow("Audio", audio))
        }
        rows.append(ledgerRow("AV offset", "\(controller.decision?.audioOffsetMs ?? 0) ms"))
        return rows
    }

    private var debugDecodingRows: [PlaybackLedgerRow] {
        let snapshot = controller.currentDiagnosticSnapshot
        let size = controller.presentationSize
        var rows: [PlaybackLedgerRow] = [
            ledgerRow(
                "Resolution",
                size.width > 0 && size.height > 0
                    ? "\(Int(size.width))×\(Int(size.height))"
                    : "—"
            )
        ]
        if let range = PlayerView.dynamicRangeSummary(
            source: controller.decision?.source,
            delivered: controller.deliveredRange,
            displayHDR: Caps.displayIsHDR,
            reasons: controller.decision?.reasons
        ) {
            rows.append(ledgerRow("Dynamic range", range))
        }
        rows.append(ledgerRow(
            "Buffer",
            snapshot.runway.map { String(format: "%.1f s", $0) } ?? "—",
            tone: runwayTone(snapshot.runway)
        ))
        rows.append(ledgerRow(
            "Player state",
            snapshot.timeControlStatus ?? "unknown",
            tone: playerStateTone(snapshot.timeControlStatus)
        ))
        if let waiting = snapshot.waitingReason {
            rows.append(ledgerRow("Waiting reason", waiting, tone: .warning))
        }
        rows.append(ledgerRow(
            "Buffer empty",
            yesNo(snapshot.playbackBufferEmpty),
            tone: snapshot.playbackBufferEmpty == true ? .critical : .good
        ))
        rows.append(ledgerRow(
            "Likely to keep up",
            yesNo(snapshot.playbackLikelyToKeepUp),
            tone: snapshot.playbackLikelyToKeepUp == false ? .critical : .good
        ))
        rows.append(ledgerRow("Buffer full", yesNo(snapshot.playbackBufferFull)))
        rows.append(ledgerRow(
            "Stalls",
            snapshot.accessStalls.map(String.init) ?? "—",
            tone: stallTone(snapshot.accessStalls)
        ))
        let subtitle = selectedSubtitleParts
        rows.append(contentsOf: ledgerSplitRows(
            "Subtitles",
            subtitle.name,
            clause: subtitle.delivery
        ))
        return rows
    }

    private var debugNetworkRows: [PlaybackLedgerRow] {
        let snapshot = controller.currentDiagnosticSnapshot
        return [
            ledgerRow(
                "Delivery rate",
                controller.sessionStatus?.deliveredBps.map(bitRate) ?? "—",
                tone: networkTone
            ),
            ledgerRow("Observed rate", snapshot.observedBitrateBps.map { bitRate(Int($0)) } ?? "—"),
            ledgerRow("Stream rate", snapshot.indicatedBitrateBps.map { bitRate(Int($0)) } ?? "—"),
            ledgerRow("Requests", snapshot.mediaRequests.map(String.init) ?? "—"),
            ledgerRow(
                "Downloaded media",
                snapshot.downloadedDuration.map { String(format: "%.1f s", $0) } ?? "—"
            ),
            ledgerRow("Transferred", snapshot.bytesTransferred.map(byteCount) ?? "—"),
            ledgerRow(
                "Transfer time",
                snapshot.transferDuration.map { String(format: "%.2f s", $0) } ?? "—"
            ),
        ]
    }

    private var debugServerRows: [PlaybackLedgerRow] {
        if controller.isVOD {
            return [
                ledgerNote("Stream", "Already transcoded · served from cache", tone: .good)
            ]
        }
        guard let status = controller.sessionStatus else {
            return [ledgerRow("Status", "No server-side session", tone: .muted)]
        }
        let speed = status.recentSpeed ?? status.speed
        var rows: [PlaybackLedgerRow] = [
            ledgerRow("Encoder", status.encoder ?? controller.encoder ?? "—"),
            ledgerRow(
                "Encode speed",
                speed.map { String(format: "%.2f×", $0) } ?? "—",
                tone: speed.map { encodeTone(speed: $0, status: status) } ?? .muted
            ),
            ledgerRow(
                "Server ahead",
                status.aheadSeconds.map { "\(max(0, $0)) s" } ?? "—",
                tone: runwayTone(
                    status.aheadSeconds.map(Double.init),
                    suspended: status.suspended ?? false
                )
            ),
            ledgerRow("Ahead bytes", status.aheadBytes.map(byteCount) ?? "—"),
            ledgerRow("Produced", status.outTimeMs.map { formatTime($0) } ?? "—"),
            ledgerRow("Pacing", status.readrate.map { String(format: "%.2f×", $0) } ?? "—"),
            ledgerRow(
                "Held",
                yesNo(status.suspended),
                tone: status.suspended == true ? .good : .neutral
            ),
        ]
        if let reason = status.holdReason {
            rows.append(ledgerRow("Hold reason", reason, tone: .good))
        }
        rows.append(ledgerRow(
            "Suspend count",
            status.suspendCount.map(String.init) ?? "0",
            tone: (status.suspendCount ?? 0) > 8 ? .warning : .neutral
        ))
        rows.append(ledgerRow("Delivered", status.deliveredBytes.map(byteCount) ?? "—"))
        rows.append(ledgerRow(
            "Delivery idle",
            status.deliveredIdleMs.map { "\($0) ms" } ?? "—",
            tone: idleTone(
                status.deliveredIdleMs,
                suspended: status.suspended ?? false
            )
        ))
        if let request = status.lastRequest {
            rows.append(ledgerRow("Last request", request))
        }
        rows.append(ledgerRow(
            "Request idle",
            status.idleSeconds.map { "\($0) s" } ?? "—",
            tone: requestIdleTone(
                status.idleSeconds,
                suspended: status.suspended ?? false
            )
        ))
        if let shape = status.playlistShape {
            rows.append(ledgerRow("Playlist", shape))
        }
        rows.append(ledgerRow("Published end", status.publishedEndMs.map { "\($0) ms" } ?? "—"))
        rows.append(ledgerRow("Fetched end", status.fetchedEndMs.map { "\($0) ms" } ?? "—"))
        return rows
    }

    // MARK: - Standard field set

    /// Standard keeps each platform's own field set and value text; only the
    /// arrangement is shared.
    private var standardSections: [PlaybackLedgerSection] {
        #if os(tvOS)
        return televisionStandardSections
        #else
        return compactStandardSections
        #endif
    }

    #if os(tvOS)
    private var televisionStandardSections: [PlaybackLedgerSection] {
        [
            PlaybackLedgerSection("Playback", column: .left, rows: televisionPlaybackRows),
            PlaybackLedgerSection(
                "Source",
                column: .left,
                placeholder: "Waiting for source details",
                rows: televisionSourceRows
            ),
            PlaybackLedgerSection("Now decoding", column: .left, rows: televisionDecodingRows),
            PlaybackLedgerSection(
                "Server",
                column: .right,
                placeholder: "Waiting for server details",
                rows: televisionServerRows
            ),
        ]
    }

    /// The header already carries method and position on the television, so
    /// this section exists only to route the decision reason into the notes.
    private var televisionPlaybackRows: [PlaybackLedgerRow] {
        guard let reasons = controller.decision?.reasons, !reasons.isEmpty else { return [] }
        return [ledgerRow("Reason", reasons.joined(separator: " · "))]
    }

    private var televisionSourceRows: [PlaybackLedgerRow] {
        guard let source = controller.decision?.source else { return [] }
        let resolution = source.width.flatMap { width in
            source.height.map { "\(width) × \($0)" }
        } ?? "Unknown"
        let video = [
            source.videoCodec?.uppercased(),
            source.bitDepth.map { "\($0)-bit" },
            source.hdrFormat ?? source.hdr?.uppercased(),
        ]
            .compactMap { $0 }
            .joined(separator: " · ")
        var rows: [PlaybackLedgerRow] = [
            ledgerRow("Resolution", resolution),
            ledgerRow("Video", video.isEmpty ? "Unknown" : video),
            ledgerRow("Container", source.container?.uppercased() ?? "Unknown"),
        ]
        if let bitrate = source.bitrate {
            rows.append(ledgerRow("Bitrate", bitRate(bitrate)))
        }
        return rows
    }

    private var televisionDecodingRows: [PlaybackLedgerRow] {
        var rows: [PlaybackLedgerRow] = []
        let size = controller.presentationSize
        if size.width > 0 && size.height > 0 {
            rows.append(ledgerRow("Output", "\(Int(size.width)) × \(Int(size.height))"))
        }
        if let range = PlayerView.dynamicRangeSummary(
            source: controller.decision?.source,
            delivered: controller.deliveredRange,
            displayHDR: Caps.displayIsHDR,
            reasons: controller.decision?.reasons
        ) {
            rows.append(ledgerRow("Dynamic range", range))
        }
        if let bitrate = controller.observedBitrate, bitrate > 0 {
            rows.append(ledgerRow("Observed bitrate", bitRate(Int(bitrate))))
        } else if let bitrate = controller.indicatedBitrate, bitrate > 0 {
            rows.append(ledgerRow("Stream bitrate", bitRate(Int(bitrate))))
        }
        let subtitle = selectedSubtitleParts
        rows.append(contentsOf: ledgerSplitRows(
            "Subtitles",
            subtitle.name,
            clause: subtitle.delivery
        ))
        return rows
    }

    private var televisionServerRows: [PlaybackLedgerRow] {
        if controller.isVOD && controller.methodLabel.contains("cached") {
            return [ledgerRow("Status", "Served from cache", tone: .good)]
        }
        guard let status = controller.sessionStatus else { return [] }
        var rows: [PlaybackLedgerRow] = [
            ledgerRow(
                "Status",
                (status.suspended ?? false) ? "Holding buffer" : "Active",
                tone: .good
            )
        ]
        if let encoder = status.encoder ?? controller.encoder {
            rows.append(ledgerRow("Encoder", encoder))
        }
        if let speed = status.recentSpeed ?? status.speed {
            rows.append(ledgerRow(
                "Encode speed",
                String(format: "%.2f×", speed),
                tone: encodeTone(speed: speed, status: status)
            ))
        }
        if let ahead = status.aheadSeconds {
            // The hold clause appears and disappears every couple of seconds as
            // the client buffer fills and drains, so it is a note of its own
            // rather than a tail on the seconds — the seconds never move.
            let held = (status.suspended ?? false)
                ? "held\(holdReleaseDescription(status))"
                : nil
            rows.append(contentsOf: ledgerSplitRows(
                "Buffer ahead",
                "\(max(0, ahead)) s",
                clause: held,
                tone: runwayTone(Double(ahead), suspended: status.suspended ?? false)
            ))
        }
        if let delivered = status.deliveredBps {
            rows.append(ledgerRow("Delivery", bitRate(delivered)))
        }
        if let bytes = status.deliveredBytes {
            rows.append(ledgerRow("Transferred", byteCount(bytes)))
        }
        return rows
    }
    #else
    private var compactStandardSections: [PlaybackLedgerSection] {
        [
            PlaybackLedgerSection("Playback", column: .left, rows: compactPlaybackRows),
            PlaybackLedgerSection("Source", column: .left, rows: compactSourceRows),
            PlaybackLedgerSection("Now decoding", column: .left, rows: compactDecodingRows),
            PlaybackLedgerSection(
                "Server",
                column: .right,
                keepsNotesInBox: true,
                rows: compactServerRows
            ),
        ]
    }

    private var compactPlaybackRows: [PlaybackLedgerRow] {
        // Same seam as Debug: the mode is the datum, the qualifiers the user's
        // choices add to it are the clause.
        let method = ledgerSeam(controller.methodLabel)
        var rows: [PlaybackLedgerRow] = ledgerSplitRows(
            "Method",
            method.datum,
            clause: method.clause
        )
        rows.append(
            ledgerRow(
                "Position",
                "\(formatTime(controller.currentMs)) / \(formatTime(controller.knownDurationMs))"
            )
        )
        if let reasons = controller.decision?.reasons, !reasons.isEmpty {
            rows.append(ledgerRow("Reason", reasons.joined(separator: "; ")))
        }
        return rows
    }

    private var compactSourceRows: [PlaybackLedgerRow] {
        guard let source = controller.decision?.source else { return [] }
        let resolution = source.width.flatMap { width in
            source.height.map { "\(width)×\($0)" }
        } ?? "—"
        let video = [
            source.videoCodec?.uppercased(),
            source.bitDepth.map { "\($0)-bit" },
            source.hdrFormat ?? source.hdr?.uppercased(),
        ]
            .compactMap { $0 }
            .joined(separator: " · ")
        // Resolution is the datum; the codec facts hung off it run past what a
        // grid row can show on an iPad, so they are a note rather than a tail
        // the grid would have to truncate.
        var rows: [PlaybackLedgerRow] = ledgerSplitRows(
            "Source",
            resolution,
            clause: video.isEmpty ? "—" : video
        )
        rows.append(ledgerRow("Container", source.container?.uppercased() ?? "—"))
        if let bitrate = source.bitrate {
            rows.append(ledgerRow("Source rate", bitRate(bitrate)))
        }
        return rows
    }

    private var compactDecodingRows: [PlaybackLedgerRow] {
        var rows: [PlaybackLedgerRow] = []
        let size = controller.presentationSize
        if size.width > 0 && size.height > 0 {
            rows.append(ledgerRow("Output", "\(Int(size.width))×\(Int(size.height))"))
        }
        // The badge says it in three characters; this says it in words, with
        // the server's own reason for the difference.
        if let range = PlayerView.dynamicRangeSummary(
            source: controller.decision?.source,
            delivered: controller.deliveredRange,
            displayHDR: Caps.displayIsHDR,
            reasons: controller.decision?.reasons
        ) {
            rows.append(ledgerRow("Dynamic range", range))
        }
        if let bitrate = controller.observedBitrate, bitrate > 0 {
            rows.append(ledgerRow("Observed rate", bitRate(Int(bitrate))))
        } else if let bitrate = controller.indicatedBitrate, bitrate > 0 {
            rows.append(ledgerRow("Stream rate", bitRate(Int(bitrate))))
        }
        if let stalls = controller.stalls {
            rows.append(ledgerRow("Player stalls", String(stalls)))
        }
        return rows
    }

    private var compactServerRows: [PlaybackLedgerRow] {
        var rows: [PlaybackLedgerRow] = []
        if controller.isVOD && controller.methodLabel.contains("cached") {
            rows.append(ledgerNote("Server", "Already transcoded · served from cache"))
        } else if let status = controller.sessionStatus {
            if let encoder = status.encoder ?? controller.encoder {
                rows.append(ledgerRow("Encoder", encoder))
            }
            if let speed = status.recentSpeed ?? status.speed {
                rows.append(ledgerRow("Encode speed", String(format: "%.2f×", speed)))
            }
            if let ahead = status.aheadSeconds {
                // Same seam as the television: the seconds stay in the grid
                // and the hold clause is a note that comes and goes with the
                // hold, so neither half changes column as the server suspends.
                let held = (status.suspended ?? false)
                    ? "held\(holdReleaseDescription(status))"
                    : nil
                rows.append(contentsOf: ledgerSplitRows(
                    "Server ahead",
                    "\(max(0, ahead)) s",
                    clause: held
                ))
            }
            if let delivered = status.deliveredBps {
                rows.append(ledgerRow("Delivery rate", bitRate(delivered)))
            }
            if let bytes = status.deliveredBytes {
                rows.append(ledgerRow("Delivered", byteCount(bytes)))
            }
        }
        if let subtitle = controller.selectedSubtitle,
           let track = controller.subtitles.first(where: { $0.index == subtitle }) {
            let parts = subtitleParts(track, index: subtitle)
            rows.append(contentsOf: ledgerSplitRows(
                "Subtitles",
                parts.name,
                clause: parts.delivery
            ))
        }
        return rows
    }
    #endif

    private func ledgerRow(
        _ label: String,
        _ value: String,
        tone: PlaybackStatTone = .neutral
    ) -> PlaybackLedgerRow {
        PlaybackLedgerRow(
            label: label,
            value: value,
            tone: tone,
            placement: PlaybackLedgerRow.resolvedPlacement(label: label)
        )
    }

    /// A row whose value is prose by construction rather than by label — the
    /// cached-VOD sentence, which is a note wherever it appears even though
    /// its label is not a note label anywhere else.
    private func ledgerNote(
        _ label: String,
        _ value: String,
        tone: PlaybackStatTone = .neutral
    ) -> PlaybackLedgerRow {
        PlaybackLedgerRow(label: label, value: value, tone: tone, placement: .notes)
    }

    /// A short datum with an optional trailing clause, split into two rows
    /// that never trade places: the datum is always a grid row and the clause,
    /// when there is one, is always a note under the same label. The clause
    /// coming and going changes whether the note exists, never where either
    /// half is drawn. Both halves carry the same tone so the pair still reads
    /// as one fact.
    private func ledgerSplitRows(
        _ label: String,
        _ datum: String,
        clause: String?,
        tone: PlaybackStatTone = .neutral
    ) -> [PlaybackLedgerRow] {
        var rows = [
            PlaybackLedgerRow(label: label, value: datum, tone: tone, placement: .grid)
        ]
        if let clause, !clause.isEmpty {
            rows.append(
                PlaybackLedgerRow(label: label, value: clause, tone: tone, placement: .notes)
            )
        }
        return rows
    }

    /// Cuts a separator-joined value at its first seam: the leading fact is
    /// the datum, everything after it is the clause. Used where the value is
    /// assembled elsewhere and only arrives here as one string.
    private func ledgerSeam(_ value: String) -> (datum: String, clause: String?) {
        guard let seam = value.range(of: " · ") else { return (value, nil) }
        let clause = String(value[seam.upperBound...])
        return (String(value[..<seam.lowerBound]), clause.isEmpty ? nil : clause)
    }

    // MARK: - Shared values

    private var selectedAudioDescription: String? {
        let track = controller.audioTracks.first(where: { $0.index == controller.selectedAudio })
            ?? controller.audioTracks.first(where: { $0.default })
        guard let track else { return nil }
        return [
            track.codec.uppercased(),
            track.channels.map(channelDescription),
            track.language?.uppercased(),
            track.title,
        ].compactMap { $0 }.joined(separator: " · ")
    }

    /// "Off" has no clause, so the note simply does not exist while subtitles
    /// are off — the grid row stays put either way.
    private var selectedSubtitleParts: (name: String, delivery: String?) {
        guard let index = controller.selectedSubtitle,
              let track = controller.subtitles.first(where: { $0.index == index })
        else { return ("Off", nil) }
        let parts = subtitleParts(track, index: index)
        return (parts.name, parts.delivery)
    }

    private func channelDescription(_ channels: Int) -> String {
        switch channels {
        case 1: return "Mono"
        case 2: return "Stereo"
        case 6: return "5.1"
        case 8: return "7.1"
        default: return "\(channels)ch"
        }
    }

    private func yesNo(_ value: Bool?) -> String {
        value.map { $0 ? "Yes" : "No" } ?? "—"
    }

    private var playbackTone: PlaybackStatTone {
        stallTone(controller.stalls)
    }

    private var bufferTone: PlaybackStatTone {
        runwayTone(controller.bufferedRunwaySeconds())
    }

    private var networkTone: PlaybackStatTone {
        guard let delivered = controller.sessionStatus?.deliveredBps.map(Double.init)
                ?? controller.observedBitrate,
              let expected = controller.indicatedBitrate,
              expected > 0
        else { return .muted }
        let runway = controller.bufferedRunwaySeconds() ?? 0
        if delivered < expected * 0.55 && runway < 2 { return .critical }
        if delivered < expected * 0.85 && runway < 8 { return .warning }
        return .good
    }

    private func stallTone(_ stalls: Int?) -> PlaybackStatTone {
        guard let stalls else { return .muted }
        if stalls == 0 { return .good }
        if stalls >= 3 { return .critical }
        return .warning
    }

    private func runwayTone(
        _ seconds: Double?,
        suspended: Bool = false
    ) -> PlaybackStatTone {
        if suspended { return .good }
        guard let seconds else { return .muted }
        if seconds < 1.5 { return .critical }
        if seconds < 5 { return .warning }
        return .good
    }

    private func encodeTone(
        speed: Double,
        status: PlaybackSessionStatus
    ) -> PlaybackStatTone {
        if status.suspended == true { return .good }
        let runway = Double(status.aheadSeconds ?? 0)
        if speed < 0.65 && runway < 2 { return .critical }
        if speed < 1 && runway < 10 { return .warning }
        return .good
    }

    private func playerStateTone(_ state: String?) -> PlaybackStatTone {
        guard let state = state?.lowercased() else { return .muted }
        if state.contains("play") { return .good }
        if state.contains("fail") || state.contains("error") { return .critical }
        if state.contains("wait") || state.contains("buffer") { return .warning }
        return .neutral
    }

    private func idleTone(
        _ milliseconds: Int?,
        suspended: Bool = false
    ) -> PlaybackStatTone {
        if suspended { return .good }
        guard let milliseconds else { return .muted }
        if milliseconds > 20_000 { return .critical }
        if milliseconds > 8_000 { return .warning }
        return .neutral
    }

    private func requestIdleTone(
        _ seconds: Int?,
        suspended: Bool = false
    ) -> PlaybackStatTone {
        if suspended { return .good }
        guard let seconds else { return .muted }
        if seconds > 20 { return .critical }
        if seconds > 8 { return .warning }
        return .neutral
    }

    private var buildLabel: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String
        switch (version, build) {
        case let (version?, build?) where version != build: return "\(version) (\(build))"
        case let (version?, _): return version
        case let (_, build?): return build
        default: return "development"
        }
    }

    /// The track's name is the datum; how it is being delivered is the clause.
    /// Kept apart so a ledger row can put the name in the grid and the
    /// delivery in the notes without either moving when a track is swapped for
    /// one with a longer name.
    private func subtitleParts(
        _ track: SubtitleTrack,
        index: Int
    ) -> (name: String, delivery: String) {
        let delivery = track.isPGSOverlay
            ? (controller.pgsOverlayStatus.label ?? "PGS overlay")
            : (track.isNativeHLS ? "native WebVTT" : "burned in")
        return (track.title ?? track.language?.uppercased() ?? "Track \(index + 1)", delivery)
    }

    private func holdReleaseDescription(_ status: PlaybackSessionStatus) -> String {
        guard status.suspended ?? false else { return "" }
        switch status.holdReason {
        case "time":
            return status.resumeBelowSeconds.map { " · time release ≤\($0) s" } ?? ""
        case "bytes":
            return status.resumeBelowBytes.map { " · bytes release ≤\(byteCount($0))" } ?? ""
        case "global":
            return status.resumeBelowBytes.map { " · global release ≤\(byteCount($0))" } ?? ""
        default:
            return ""
        }
    }

    private func bitRate(_ bits: Int) -> String {
        String(format: "%.2f Mb/s", Double(bits) / 1_000_000.0)
    }

    private func byteCount(_ bytes: Int) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }

    // MARK: - Ledger metrics

    /// Two columns need real width. A phone has none to spare, so there the
    /// sections stack in declared order instead of splitting.
    private var ledgerUsesTwoColumns: Bool {
        #if os(iOS)
        return horizontalSizeClass != .compact
        #else
        return true
        #endif
    }

    /// The television reads its grid from across a room and holds every value
    /// to one line. A phone's value column is around 160pt wide — half that
    /// once the sections split into two columns — which cuts values the notes
    /// strip does not claim, so the compact size class gets a second line.
    /// It stays a hard cap: two lines cannot stretch a column either.
    private var ledgerGridValueLineLimit: Int {
        #if os(iOS)
        return horizontalSizeClass == .compact ? 2 : 1
        #else
        return 1
        #endif
    }

    /// Two-up inside a section only works where the sub-columns stay wide
    /// enough to hold a short value without wrapping.
    private var ledgerAllowsDenseColumns: Bool {
        #if os(tvOS)
        return true
        #else
        return false
        #endif
    }

    private var ledgerTitleFont: Font {
        #if os(tvOS)
        return mode == .debug
            ? .system(size: 26, weight: .bold, design: .rounded)
            : .system(
                size: TVPlaybackInfoPresentation.titleFontSize,
                weight: .bold,
                design: .rounded
            )
        #else
        return mode == .debug
            ? .system(size: 17, weight: .bold, design: .rounded)
            : .system(.headline, design: .monospaced)
        #endif
    }

    private var ledgerLabelFont: Font {
        #if os(tvOS)
        return mode == .debug
            ? .system(size: 12, weight: .medium, design: .monospaced)
            : .system(size: 14, weight: .medium, design: .rounded)
        #else
        return mode == .debug
            ? .system(size: 9, weight: .medium, design: .monospaced)
            : .system(.caption, design: .monospaced)
        #endif
    }

    private var ledgerValueFont: Font {
        #if os(tvOS)
        return mode == .debug
            ? .system(size: 13, weight: .medium, design: .monospaced)
            : .system(
                size: TVPlaybackInfoPresentation.valueFontSize,
                weight: .semibold,
                design: .rounded
            )
        #else
        return mode == .debug
            ? .system(size: 10, weight: .medium, design: .monospaced)
            : .system(.caption, design: .monospaced)
        #endif
    }

    private var ledgerLabelColor: Color {
        mode == .debug ? .white.opacity(0.45) : labelColor
    }

    private var ledgerSectionSize: CGFloat {
        #if os(tvOS)
        return mode == .debug ? 14 : 15
        #else
        return mode == .debug ? 10 : 11
        #endif
    }

    private var ledgerDetailSize: CGFloat {
        #if os(tvOS)
        14
        #else
        10
        #endif
    }

    private var ledgerGutterWidth: CGFloat {
        #if os(tvOS)
        return mode == .debug ? 104 : 140
        #else
        return mode == .debug ? 86 : 100
        #endif
    }

    private var ledgerDenseGutterWidth: CGFloat {
        #if os(tvOS)
        88
        #else
        70
        #endif
    }

    private var ledgerSpacing: CGFloat {
        #if os(tvOS)
        12
        #else
        8
        #endif
    }

    private var ledgerColumnGap: CGFloat {
        #if os(tvOS)
        18
        #else
        10
        #endif
    }

    private var ledgerRowSpacing: CGFloat {
        #if os(tvOS)
        7
        #else
        5
        #endif
    }

    private var ledgerRowGap: CGFloat {
        #if os(tvOS)
        8
        #else
        6
        #endif
    }

    private var ledgerPanelPadding: CGFloat {
        #if os(tvOS)
        20
        #else
        12
        #endif
    }

    private var ledgerSectionPadding: CGFloat {
        #if os(tvOS)
        13
        #else
        9
        #endif
    }

    private var ledgerCornerRadius: CGFloat {
        #if os(tvOS)
        20
        #else
        12
        #endif
    }

    private var ledgerSectionCornerRadius: CGFloat {
        #if os(tvOS)
        13
        #else
        9
        #endif
    }

    private var ledgerEdgeInset: CGFloat {
        #if os(tvOS)
        return TVPlaybackInfoPresentation.debugEdgeInset
        #else
        return 10
        #endif
    }

    private var ledgerWidthFraction: CGFloat {
        #if os(tvOS)
        return mode == .debug ? 0.66 : 0.52
        #else
        return 0.94
        #endif
    }

    private var ledgerMaxWidth: CGFloat {
        #if os(tvOS)
        return mode == .debug
            ? TVPlaybackInfoPresentation.debugPanelMaxWidth
            : TVPlaybackInfoPresentation.panelMaxWidth
        #else
        return mode == .debug ? 760 : 560
        #endif
    }

    private var ledgerMaxHeight: CGFloat {
        #if os(tvOS)
        return TVPlaybackInfoPresentation.debugPanelMaxHeight
        #else
        return 680
        #endif
    }

    /// The panel stops short of the transport controls rather than floating
    /// over them. This is the height of the transport row plus the overlay's
    /// own padding, not the whole now-playing header beside it.
    private var ledgerTransportReserve: CGFloat {
        #if os(tvOS)
        200
        #else
        130
        #endif
    }
}
