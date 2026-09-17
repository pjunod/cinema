import SwiftUI

#if os(iOS)
import UIKit
#endif

enum PlayerControl: Hashable {
    case reveal
    case close
    case retry
    /// A blocking surface whose fault offers `sign_in` — a 401 or 403, where
    /// retrying the same bearer is the one thing that cannot work.
    case signIn
    case progress
    case marker
    case skipBack
    case playPause
    case skipForward
    case pictureInPicture
    case audio
    case subtitles
    case quality
    case settings
    case stats
}

extension PlayerControl {
    var isTransportControl: Bool {
        switch self {
        case .skipBack, .playPause, .skipForward, .pictureInPicture,
             .audio, .subtitles, .quality, .settings, .stats:
            true
        default:
            false
        }
    }

    var isChromeControl: Bool {
        isTransportControl || self == .progress || self == .marker
    }
}

#if os(iOS)
private enum PlayerOptionMenu: Hashable {
    case audio
    case subtitles
    case quality
    case settings
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

    /// The source's profile as a number, for comparing against the delivered
    /// one rather than for display.
    ///
    /// Reads the same label `sourceMark` already reads, so the two can never
    /// disagree about which profile the file carries. A row scanned before the
    /// profile columns existed carries the bare string "Dolby Vision" with no
    /// number in it, and then there is no answer and no arrow — reading a
    /// number out of prose that does not have one is how a badge invents a
    /// conversion that never happened.
    static func dolbyVisionProfileNumber(in hdrFormat: String?) -> Int? {
        let rich = hdrFormat?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard let digits = dolbyVisionProfile(in: rich) else { return nil }
        return Int(digits)
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
    @State private var statsMode = PlaybackStatsMode.persisted
    @State private var findingNext = false
    @State private var nextEpisodeTask: Task<Void, Never>?
    @State private var isScrubbing = false
    @State private var scrubMs = 0.0
    @State private var pendingMs: Int?
    @State private var controlsVisible = true
    @State private var autoHideGeneration = 0
    #if os(iOS)
    @State private var activeOptionMenu: PlayerOptionMenu?
    #endif
    #if os(tvOS)
    @FocusState private var focusedControl: PlayerControl?
    @State private var lastFocusedControl: PlayerControl = .playPause
    @State private var repeatedInput: PlayerContractInput?
    @State private var repeatCount = 0
    @State private var lastMoveAt: TimeInterval = 0
    @State private var tvMenuOpen = false
    @State private var menuOpener: PlayerControl = .playPause
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
                // Not while the stream has failed: the surface is full-screen,
                // focusable and carries the remote adapter, so once the chrome
                // had auto-hidden it sat in front of the retry buttons
                // consuming every direction — `failed × up/down` is `ignore`,
                // and only Menu got out.
                if !controlsVisible && !controller.isPlaybackBlocked {
                    Color.clear
                        .contentShape(Rectangle())
                        .ignoresSafeArea()
                        .focusable()
                        .focusEffectDisabled()
                        .focused($focusedControl, equals: .reveal)
                        .playerRemoteAdapter(
                            .surface,
                            state: inputState,
                            apply: applyPlayerInputOutcome
                        )
                        .accessibilityLabel("Show playback controls")
                }
                #endif

                #if os(iOS)
                Color.clear
                    .contentShape(Rectangle())
                    .ignoresSafeArea()
                    .onTapGesture {
                        let input = PlayerContractInput.tapSurface
                        _ = applyPlayerInputOutcome(
                            PlayerInputRouting.route(
                                surface: .touch,
                                state: inputState(),
                                input: input
                            ),
                            input: input
                        )
                    }
                    .accessibilityLabel("Show or hide playback controls")
                #endif

                if controller.isPlaybackBlocked {
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
                            title: title,
                            mode: $statsMode,
                            onDismiss: dismissPlaybackInfo
                        )
                        .transition(.opacity.combined(with: .scale(scale: 0.98)))
                        #else
                        PlaybackStatsView(
                            controller: controller,
                            title: title,
                            mode: $statsMode,
                            onDismiss: dismissPlaybackInfo
                        )
                        .frame(maxWidth: .infinity, alignment: .trailing)
                        .transition(.opacity.combined(with: .move(edge: .trailing)))
                        #endif
                    }
                }

                // ONE progress surface, drawn by the presenter's verdict.
                // `isChangingStream` used to draw a spinner here and
                // `isPlaybackWaiting` a second box below it, so two things
                // decided what covered the picture — the exact defect the
                // contract exists to kill. Both are faults now
                // (`client_preparing`, `media_waiting`).
                //
                // One owner, two renders, and §3.1 is emphatic about the
                // difference: the box covers the picture, the indicator sits
                // beside one that is playing. They must never be the same view.
                if let progress = controller.progressSurfaceRender, !findingNext {
                    switch progress {
                    case .blocking: playbackProgressSurface
                    case .indicator: playbackProgressIndicator
                    }
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

                if let banner = playbackBannerSurface,
                   let message = Self.bannerMessage(for: banner) {
                    playbackBanner(banner, message: message)
                }
            }
        }
        #if os(tvOS)
        .playerRemoteAdapter(
            .root,
            state: inputState,
            apply: applyPlayerInputOutcome
        )
        #endif
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
            #if os(iOS)
            controller.remoteInput = nil
            #endif
            lifecycle.teardown { teardownPlayback() }
        }
        .onAppear {
            // The lock screen and the control centre reach the same player, so
            // they route through the same table: a skip while a scrub is
            // pending is `ignore`, and play/pause commits it first.
            #if os(iOS)
            controller.remoteInput = { input in
                applyPlayerInputOutcome(
                    PlayerInputRouting.route(
                        surface: .touch,
                        state: inputState(),
                        input: input
                    ),
                    input: input
                )
            }
            #endif
        }
        .task(id: autoHideGeneration) {
            guard Self.shouldAutoHideControls(
                visible: controlsVisible,
                playing: controller.isPlaying,
                scrubbing: isScrubbing,
                changingStream: controller.isChangingStream,
                optionMenuOpen: optionMenuOpen,
                failed: controller.isPlaybackBlocked,
                infoOpen: Self.infoSuppressesAutoHide(showStats: showStats, statsMode: statsMode),
                previewPending: pendingMs != nil,
                tearingDown: lifecycle.isTearingDown
            ) else { return }
            try? await Task.sleep(nanoseconds: Self.controlAutoHideDelayNanoseconds)
            guard !Task.isCancelled,
                  Self.shouldAutoHideControls(
                      visible: controlsVisible,
                      playing: controller.isPlaying,
                      scrubbing: isScrubbing,
                      changingStream: controller.isChangingStream,
                      optionMenuOpen: optionMenuOpen,
                      failed: controller.isPlaybackBlocked,
                      infoOpen: Self.infoSuppressesAutoHide(showStats: showStats, statsMode: statsMode),
                      previewPending: pendingMs != nil,
                      tearingDown: lifecycle.isTearingDown
                  ) else { return }
            hideControls()
        }
        .onChange(of: controller.isPlaying) { _, _ in revealControls() }
        .onChange(of: controller.isChangingStream) { _, _ in revealControls() }
        // PiP declares its own presentation and its own failures. Neither is
        // this controller's to guess at, so the view hands both over and the
        // banner reads only the presenter (contract §3.4, §4).
        .onChange(of: pictureInPicture.isActive) { _, active in
            controller.notePictureInPictureActive(active)
        }
        .onChange(of: pictureInPicture.errorMessage) { _, message in
            controller.notePictureInPictureFailure(message)
        }
        .onChange(of: showStats) { _, _ in restartAutoHideTimer() }
        .onChange(of: isScrubbing) { _, _ in restartAutoHideTimer() }
        .onChange(of: pendingMs) { _, _ in restartAutoHideTimer() }
        .onChange(of: statsMode) { _, mode in
            mode.persist()
            restartAutoHideTimer()
        }
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
        .onChange(of: controller.isPlaybackBlocked) { _, blocked in
            guard blocked else { return }
            focusedControl = Self.failureFocusTarget(for: controller.surface.surface)
        }
        .onChange(of: focusedControl) { oldControl, newControl in
            if let newControl, newControl.isChromeControl {
                lastFocusedControl = newControl
            }
            if let oldControl,
               oldControl.isTransportControl,
               newControl == nil,
               controlsVisible,
               !showStats {
                menuOpener = oldControl
                tvMenuOpen = true
            } else if newControl != nil {
                tvMenuOpen = false
            }
            if controlsVisible { restartAutoHideTimer() }
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

    /// The red strip beside the picture, read from the presenter and from
    /// nothing else. A PiP failure is a `degraded` fault in the same model
    /// (contract §4), so there is no second source to fall back to.
    ///
    /// It carries whatever the presenter is showing that is not a prompt or a
    /// terminal — a notice, a hold, a recovery step, a refused change — which
    /// is the same set of sentences `playbackNotice ?? playbackError` used to
    /// carry whenever `failed` was false. The prompt and the terminal are
    /// `failureView`, and the call site already excludes them.
    private var playbackBannerSurface: PlaybackSurface? {
        let surface = controller.surface.surface
        // The KIND, not "not blocked". `banner` is the contract's notice strip
        // and the other kinds each have their own render — `blocking` is
        // `failureView` or the full-screen progress box, `indicator` is the
        // in-chrome capsule. Asking only whether the viewer was blocked drew
        // this strip UNDERNEATH the capsule for every indicator: one fault,
        // two overlays, which is the thing §3.1 separates the kinds to stop.
        guard surface.kind == .banner else { return nil }
        return surface
    }

    private var playbackBannerMessage: String? {
        playbackBannerSurface.flatMap(Self.bannerMessage(for:))
    }

    /// The sentence a banner carries.
    ///
    /// `detail` is the sentence in the ordinary case — a server's refusal, a
    /// recovery step's reason, a PiP failure's own words — and `title` is the
    /// heading the full-screen view puts above it. A DEMOTED fault is the
    /// exception, and §3.2 is the reason: the demotion rewrites the title to
    /// "Playback recovered" and deliberately KEEPS the old failure sentence in
    /// `detail`, because the fault's Try again still has to know what it is
    /// about. Reading `detail` there made the banner announce the thing that
    /// failed at the exact moment the picture came back.
    static func bannerMessage(for surface: PlaybackSurface) -> String? {
        guard let fault = surface.fault else { return nil }
        if surface.isDemoted { return fault.title ?? fault.detail }
        return fault.detail ?? fault.title
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
                || controller.isPlaybackBlocked
                || controller.isChangingStream
                || findingNext
                // Every surface the presenter is drawing, not only the strip.
                // `playbackBannerMessage` used to answer for the indicator too,
                // by accident; now that it answers only for the banner, the
                // progress renders say so themselves.
                || controller.progressSurfaceRender != nil
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

    /// Mini is a strip, not a modal: the transport stays live beside it, it
    /// is not the `info` state, and so it does not suppress the idle hide.
    /// Standard and Debug do.
    static func infoSuppressesAutoHide(showStats: Bool, statsMode: PlaybackStatsMode) -> Bool {
        showStats && statsMode != .mini
    }

    /// Where `reveal` puts focus. The marker button is drawn only while a
    /// marker is offered, so remembering it across the marker's life means
    /// restoring focus to a view nobody is rendering.
    static func revealFocusTarget(
        remembered: PlayerControl,
        markerOffered: Bool
    ) -> PlayerControl {
        if remembered == .marker && !markerOffered { return .playPause }
        return remembered
    }

    static func shouldAutoHideControls(
        visible: Bool,
        playing: Bool,
        scrubbing: Bool,
        changingStream: Bool,
        optionMenuOpen: Bool,
        failed: Bool,
        infoOpen: Bool,
        previewPending: Bool,
        tearingDown: Bool = false
    ) -> Bool {
        visible && playing && !scrubbing && !changingStream && !optionMenuOpen
            && !failed && !infoOpen && !previewPending && !tearingDown
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
        pendingMs = nil
        #if os(iOS)
        activeOptionMenu = nil
        #else
        tvMenuOpen = false
        #endif
        pictureInPicture.detach()
        controller.stop(deactivateAudioSession: deactivateAudioSession)
        onPlaybackStopped?(stoppedAt)
    }

    /// A presented touch menu is hosted outside the control hierarchy. Removing
    /// the controls therefore removes its presentation anchor and dismisses the
    /// menu too. On tvOS, focus on a menu button is the equivalent interaction:
    /// keep the chrome up while the viewer is opening or navigating that menu.
        /// A system menu holds it open for the same reason.
    private var optionMenuOpen: Bool {
        #if os(iOS)
        activeOptionMenu != nil
        #else
        tvMenuOpen
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
        guard controlsVisible, !controller.isPlaybackBlocked else { return }
        // Mini is a strip beside the transport, not a panel of its own: it no
        // longer suppresses the idle hide, so it has to leave with the chrome.
        // Left behind it would sit on screen with no transport and no producer
        // to close it — and `hidden × back` is exit, not close.
        if showStats && statsMode == .mini { showStats = false }
        #if os(iOS)
        activeOptionMenu = nil
        #else
        if let focusedControl, focusedControl.isChromeControl {
            lastFocusedControl = focusedControl
        }
        tvMenuOpen = false
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
        revealControls()
        Task { @MainActor in
            await Task.yield()
            if controlsVisible { focusedControl = .stats }
        }
        #else
        revealControls()
        #endif
    }

    /// The `close` control: not `back`. The touch table answers `hide` to
    /// `back` while chrome is visible — right for a key, wrong for the one
    /// button whose purpose is to leave. `close_control` closes whatever is
    /// open and always ends in `exit`; `finishPlayback()` tears everything
    /// down once, so the earlier steps only keep the reducer's bookkeeping
    /// honest for the frame the cover is still on screen.
    private func closePlayer() {
        for outcome in PlayerInputRouting.closeSteps(state: inputState()) {
            applyPlayerInputOutcome(outcome, input: .select)
        }
    }

    private func inputState() -> PlayerInputState {
        // The input contract's `failed` state is a BLOCKING surface whose
        // class is a prompt or a terminal — never a full-screen `preparing`,
        // which covers the picture but asks the viewer nothing
        // (PLAYBACK-SURFACE-CONTRACT.md §4).
        if controller.isPlaybackBlocked { return .failed }
        // Contract §2.2 orders the precedence scrub → menu → info. A pending
        // position is the most local thing on screen: on the phone the slider
        // stays live under the panel, so asking `info` first meant a drag with
        // Debug open answered `close_info` where the contract says `cancel`.
        #if os(iOS)
        if isScrubbing { return .scrub }
        if activeOptionMenu != nil { return .menu }
        #else
        if focusedControl == .progress && pendingMs != nil { return .scrub }
        if tvMenuOpen { return .menu }
        #endif
        if showStats && statsMode != .mini { return .info }
        // Chrome-hidden outranks the focus flag, as it does on Android: the
        // reveal surface takes focus when the chrome goes, but not before the
        // next press can arrive.
        if !controlsVisible { return .hidden }
        #if os(tvOS)
        if focusedControl == .progress { return .timeline }
        #endif
        return .transport
    }

    @discardableResult
    private func applyPlayerInputOutcome(
        _ outcome: PlayerInputOutcome,
        input: PlayerContractInput
    ) -> Bool {
        #if os(tvOS)
        if input != .left && input != .right {
            repeatedInput = nil
            repeatCount = 0
        }
        #endif
        switch outcome {
        case .reveal:
            #if os(tvOS)
            revealControlsFromRemote()
            #else
            revealControls()
            #endif
            return true
        case .focusRow:
            return false
        case .focusMarkerOrIgnore:
            #if os(tvOS)
            // `offeredMarker`, not `activeMarker`: the button is drawn only
            // for a marker the viewer is offered. An auto-skip-eligible
            // marker seeked back into is active with no button on screen, and
            // focusing it dropped focus entirely.
            if controller.offeredMarker != nil { focusedControl = .marker }
            #endif
            return true
        case .focusTransport:
            #if os(tvOS)
            focusedControl = lastFocusedControl.isTransportControl
                ? lastFocusedControl
                : .playPause
            #endif
            revealControls()
            return true
        case .activate, .menuFocus, .ignore:
            return false
        case .togglePlay:
            controller.togglePlayPause()
            revealControls()
            return true
        case .skip:
            if input == .skipBack { controller.skip(seconds: -10) }
            if input == .skipForward { controller.skip(seconds: 10) }
            revealControls()
            return true
        case .preview:
            #if os(tvOS)
            let direction = input == .left ? -1 : input == .right ? 1 : 0
            guard direction != 0 else { return false }
            let repeats = registerPreviewInput(input)
            let step = Int(PlayerInputRouting.previewStepSeconds(repeatCount: repeats) * 1_000)
            let duration = max(controller.knownDurationMs, 0)
            let base = pendingMs ?? controller.currentMs
            pendingMs = min(max(base + direction * step, 0), duration)
            revealControls()
            return true
            #else
            return false
            #endif
        case .commit:
            commitPendingPosition()
            return true
        case .cancel:
            cancelPendingPosition()
            return true
        case .cancelThenFocusTransport:
            cancelPendingPosition()
            #if os(tvOS)
            focusedControl = lastFocusedControl.isTransportControl
                ? lastFocusedControl
                : .playPause
            #endif
            revealControls()
            return true
        case .cancelThenFocusMarkerOrIgnore:
            cancelPendingPosition()
            #if os(tvOS)
            if controller.offeredMarker != nil { focusedControl = .marker }
            #endif
            revealControls()
            return true
        case .commitThenTogglePlay:
            commitPendingPosition()
            controller.togglePlayPause()
            revealControls()
            return true
        case .closeMenu:
            #if os(iOS)
            dismissOptionMenu()
            #else
            tvMenuOpen = false
            focusedControl = menuOpener
            #endif
            revealControls()
            return true
        case .closeInfo:
            dismissPlaybackInfo()
            return true
        case .hide:
            hideControls()
            return true
        case .exit:
            finishPlayback()
            return true
        case .toggleChrome:
            #if os(iOS)
            toggleControls()
            #else
            if controlsVisible { hideControls() } else { revealControlsFromRemote() }
            #endif
            return true
        }
    }

    private func commitPendingPosition() {
        if let pendingMs {
            controller.seek(toMs: pendingMs)
            self.pendingMs = nil
        } else if isScrubbing {
            controller.seek(toMs: Int(scrubMs))
            isScrubbing = false
        }
        revealControls()
    }

    private func cancelPendingPosition() {
        pendingMs = nil
        isScrubbing = false
        revealControls()
    }

    #if os(tvOS)
    private func registerPreviewInput(_ input: PlayerContractInput) -> Int {
        let now = Date.timeIntervalSinceReferenceDate
        if repeatedInput == input, now - lastMoveAt <= 0.25 {
            repeatCount += 1
        } else {
            repeatedInput = input
            repeatCount = 0
        }
        lastMoveAt = now
        return repeatCount
    }

    private func revealControlsFromRemote() {
        revealControls()
        Task { @MainActor in
            await Task.yield()
            guard controlsVisible else { return }
            focusedControl = Self.revealFocusTarget(
                remembered: lastFocusedControl,
                markerOffered: controller.offeredMarker != nil
            )
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

    /// A progress fault over a picture that IS presenting (§3.1's `indicator`):
    /// a small capsule in the corner of the chrome, covering nothing.
    ///
    /// Deliberately not the full-screen box. `recovering` is retired by
    /// presentation that postdates its raise, so a compatibility rung, a node
    /// failover or a readiness deadline is an indicator over a picture that is
    /// playing perfectly well — and giving it the centered full-screen box
    /// would put a modal spinner over a moving picture, which is the defect
    /// this contract exists to remove. It takes no hits: a viewer's tap and a
    /// remote's focus belong to the chrome behind it.
    @ViewBuilder
    private var playbackProgressIndicator: some View {
        let surface = controller.surface.surface
        HStack(spacing: 8) {
            ProgressView().tint(.white)
            if let title = surface.title, !title.isEmpty {
                Text(title)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundColor(.white.opacity(0.85))
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(.ultraThinMaterial, in: Capsule())
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        .allowsHitTesting(false)
    }

    /// A progress fault over a picture that is NOT presenting (§3.1's
    /// full-screen half), in the two shapes this player has always drawn: a
    /// bare spinner for a fault with nothing to say (the staged open), and the
    /// spinner with the fault's own sentence under it for one that does (a
    /// wait). No new copy — the words are the ones the raising owner put on
    /// the fault.
    @ViewBuilder
    private var playbackProgressSurface: some View {
        let surface = controller.surface.surface
        if let title = surface.title, !title.isEmpty {
            VStack(spacing: 10) {
                ProgressView().tint(.white)
                Text(title)
                    .font(.system(.callout, design: .monospaced).weight(.semibold))
                if let detail = surface.detail, !detail.isEmpty {
                    Text(detail)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(.white.opacity(0.72))
                }
            }
            .foregroundColor(.white)
            .padding(18)
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12))
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            streamChangeProgress
                .tint(.white)
                .padding(18)
                .background(.ultraThinMaterial, in: Circle())
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    /// The notice strip: §3.1's `banner` kind, which is "a notice strip with
    /// **the fault's actions**; the picture is untouched".
    ///
    /// It used to be a bare `Text`. `failureView` was the only thing that drew
    /// `surface.actions` and it is gated on a blocking surface, so a `refused`
    /// change — the whole class whose point is that the predecessor keeps
    /// playing — offered the viewer a sentence and no way to re-issue the
    /// change. Physical recipe (b) forbids exactly that, and §3.2's demotion
    /// exists so a fault the picture overruled KEEPS its actions for when the
    /// buffer drains; both were dead ends here.
    ///
    /// Same button renderer and the same labels as the full-screen view, so
    /// "Try Again" cannot come to mean two things.
    private func playbackBanner(_ surface: PlaybackSurface, message: String) -> some View {
        let actions = Self.renderedActions(for: surface)
        return VStack(spacing: 10) {
            Text(message)
                .font(.system(.caption, design: .monospaced))
                .foregroundColor(.white)
            if !actions.isEmpty {
                HStack(spacing: 12) {
                    ForEach(actions, id: \.self) { action in
                        failureActionButton(action)
                    }
                }
            }
        }
        .padding(10)
        .background(Palette.accent.opacity(0.9), in: RoundedRectangle(cornerRadius: 8))
        .frame(maxWidth: .infinity, alignment: .top)
        .padding(.top, 20)
        .padding(.horizontal, 80)
    }

    private var failureView: some View {
        let surface = controller.surface.surface
        return VStack(spacing: 14) {
            Text(surface.title ?? PlayerController.playbackStartFailureTitle)
                .font(.system(.body, design: .monospaced))
                .foregroundColor(.white)
            if let error = surface.detail {
                Text(error)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundColor(Palette.muted)
                    .multilineTextAlignment(.center)
            }
            HStack(spacing: 12) {
                ForEach(Self.failureActions(for: surface), id: \.self) { action in
                    failureActionButton(action)
                }
            }
        }
        .padding(30)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black)
    }

    /// The buttons a blocking surface offers, in the fault's own order.
    ///
    /// Actions are a property of the FAULT (contract §3.1), so this is a
    /// projection and not a hard-coded pair. Two rules on top of the fault's
    /// list, each with a reason:
    ///
    /// * `keep_waiting` and `retry` are the same primitive on this client —
    ///   §3.1 says Apple's Keep waiting IS `retryAfterPlaybackFailure`, which
    ///   resets `recoveryReopenBudget` and only that — so offering both would
    ///   be two buttons that do the same thing;
    /// * Close is guaranteed. A full screen with no way out is worse than an
    ///   extra button, and every blocking class's defaults include it anyway.
    static func failureActions(for surface: PlaybackSurface) -> [PlaybackFault.Action] {
        var actions = renderedActions(for: surface)
        if !actions.contains(.close) { actions.append(.close) }
        return actions
    }

    /// The fault's own actions, in its own order, minus the ones this client
    /// has no control for and minus the second of two buttons that are one
    /// primitive here. Shared by both things that draw actions.
    ///
    /// Close is NOT added. A guaranteed way out belongs to a full screen; on a
    /// banner the picture behind it is the viewer's film, and a Close on a
    /// notice is a button that ends playback which is fine. `refused`'s whole
    /// offer is `retry`, and that is what it gets.
    static func renderedActions(for surface: PlaybackSurface) -> [PlaybackFault.Action] {
        var actions = surface.actions.filter(rendersFailureAction)
        if actions.contains(.retry) { actions.removeAll { $0 == .keepWaiting } }
        return actions
    }

    /// Which button the remote lands on when a blocking surface appears: the
    /// first one the fault actually offers, so a 401 opens on Sign In rather
    /// than on a Try Again that is not drawn.
    static func failureFocusTarget(for surface: PlaybackSurface) -> PlayerControl {
        guard let first = failureActions(for: surface).first else { return .close }
        switch first {
        case .retry, .keepWaiting: return .retry
        case .signIn: return .signIn
        case .close, .forceTranscode: return .close
        }
    }

    /// `force_transcode` is the web's, offered on its diagnosed-stall path.
    /// This client has no such control, and a button that does nothing is
    /// worse than no button.
    static func rendersFailureAction(_ action: PlaybackFault.Action) -> Bool {
        switch action {
        case .retry, .keepWaiting, .close, .signIn: true
        case .forceTranscode: false
        }
    }

    @ViewBuilder
    private func failureActionButton(_ action: PlaybackFault.Action) -> some View {
        switch action {
        // One button for both, never two: `failureActions` above strips
        // `keep_waiting` whenever `retry` is present, and every blocking
        // class's defaults offer `retry`, so a separate "Keep Waiting" was a
        // label no viewer could ever reach. Apple does not offer it separately
        // because on this client it is not a separate thing — §3.1 says Keep
        // waiting IS `retryAfterPlaybackFailure`. (The web is adding a real
        // one; the web's Keep waiting re-arms `armStall` and its Try again
        // re-opens, which genuinely differ.)
        case .retry, .keepWaiting:
            Button("Try Again") {
                controller.retryAfterPlaybackFailure()
            }
            .buttonStyle(.borderedProminent)
            .tint(Palette.accent)
            #if os(tvOS)
            .focused($focusedControl, equals: .retry)
            #endif
        case .signIn:
            Button("Sign In") { signInAfterPlaybackFailure() }
                .buttonStyle(.borderedProminent)
                .tint(Palette.accent)
                #if os(tvOS)
                .focused($focusedControl, equals: .signIn)
                #endif
        case .close:
            Button("Close") { closePlayer() }
                .buttonStyle(.bordered)
                #if os(tvOS)
                .focused($focusedControl, equals: .close)
                #endif
        case .forceTranscode:
            EmptyView()
        }
    }

    /// Sign in belongs to the app, not to the player: the bearer is no longer
    /// honoured, so the player tears down and `AppModel` puts the login screen
    /// up through the same path every other screen uses. This adds no in-player
    /// credential prompt.
    private func signInAfterPlaybackFailure() {
        finishPlayback {
            model.noteAuthFailure(APIError.http(401))
            dismiss()
        }
    }

    #if os(iOS)
    private var closeButton: some View {
        Button {
            closePlayer()
        } label: {
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

            // `offeredMarker`, not `activeMarker`: a marker the Skip preference
            // is about to take gets no button, so nothing flashes before the
            // automatic seek and no offer is reported for a skip nobody chose.
            if let marker = controller.offeredMarker {
                PlayerTrailingControlRow {
                    markerButton(marker)
                }
            }

            #if os(tvOS)
            if controller.knownDurationMs > 0 {
                HStack(spacing: 12) {
                    playbackTimeLabel(pendingMs ?? controller.currentMs)
                    tvTimeline
                        .layoutPriority(1)
                    playbackTimeLabel(controller.knownDurationMs)
                }
            }
            HStack(spacing: 12) {
                transportControlGroup
                Spacer(minLength: 8)
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
            displayHDR: Caps.displayIsHDR,
            deliveredDolbyVisionProfile: controller.deliveredDolbyVisionProfile
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
        displayHDR: Bool = true,
        deliveredDolbyVisionProfile: Int? = nil
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
            displayHDR: displayHDR,
            deliveredDolbyVisionProfile: deliveredDolbyVisionProfile
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
        displayHDR: Bool,
        deliveredDolbyVisionProfile: Int? = nil
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
        guard rendered != source else {
            // Same grade — but not necessarily the same profile. A Profile 7
            // disc remux reaching a device that takes 8 is converted on the
            // fly: every picture byte is copied and only the per-frame
            // metadata is rewritten, so the grade really is unchanged and what
            // is on screen really is Dolby Vision.
            //
            // NOT DIMMED, and that is the whole reason this is its own state
            // rather than a spelling of the downgrade below. The dimmed source
            // half means "this capability is unavailable"; here it is
            // *active*. Dimming it would say the opposite of what happened.
            guard
                source == DynamicRange.dolbyVision,
                let deliveredProfile = deliveredDolbyVisionProfile,
                let onDisk = DynamicRange.dolbyVisionProfileNumber(in: hdrFormat),
                onDisk != deliveredProfile
            else { return lit }
            return PlayerMetadataBadge(
                kind: .dynamicRange,
                tone: tone,
                mark: sourceMark,
                accessibilityLabel:
                    "\(DynamicRange.longLabel(source)), playing as Dolby Vision Profile \(deliveredProfile)",
                renderedMark: "DV P\(deliveredProfile)",
                dimmed: false
            )
        }
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
            if controller.audioTracks.count > 1 { audioMenu }
            if !controller.subtitles.isEmpty { subtitleMenu }
            if !controller.qualityRungs.isEmpty { qualityMenu }
            settingsMenu
            statsButton
            if pictureInPicture.isSupported { pictureInPictureButton }
        }
    }

    #if os(iOS)
    private var compactControlRow: some View {
        VStack(alignment: .leading, spacing: 8) {
            transportControlGroup
            playbackOptionGroup
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

    private var settingsMenu: some View {
        #if os(iOS)
        Button {
            presentOptionMenu(.settings)
        } label: {
            Image(systemName: "gearshape.fill")
        }
        .accessibilityLabel("Playback settings")
        .popover(isPresented: optionMenuBinding(.settings), arrowEdge: .bottom) {
            optionMenuPanel("Playback settings") { autoplaySettingsButton }
        }
        #else
        Menu { autoplaySettingsButton } label: {
            Image(systemName: "gearshape.fill")
        }
        .accessibilityLabel("Playback settings")
        .buttonStyle(TVPlayerControlButtonStyle())
        .focusEffectDisabled()
        .focused($focusedControl, equals: .settings)
        #endif
    }

    private var autoplaySettingsButton: some View {
        Button {
            model.setAutoplay(!model.autoplay)
            #if os(iOS)
            dismissOptionMenu()
            #endif
            revealControls()
        } label: {
            Label(model.autoplay ? "Turn off autoplay" : "Turn on autoplay",
                  systemImage: "play.square.stack.fill")
        }
    }

    #if os(tvOS)
    /// SwiftUI's Slider is unavailable on tvOS. This focusable bar previews
    /// Left/Right without seeking, commits with Select, and cancels with Menu.
    /// Up reaches the marker row and Down restores the transport row. This is
    /// not a Button:
    /// tvOS adds a large white pressed/focus surround to Buttons even when the
    /// ordinary focus effect is disabled.
    private var tvTimeline: some View {
        GeometryReader { geometry in
            let playedFraction = controller.knownDurationMs > 0
                ? min(max(Double(controller.currentMs) / Double(controller.knownDurationMs), 0), 1)
                : 0
            let previewFraction = controller.knownDurationMs > 0
                ? min(max(Double(pendingMs ?? controller.currentMs) / Double(controller.knownDurationMs), 0), 1)
                : 0
            ZStack(alignment: .leading) {
                Capsule().fill(.white.opacity(0.25))
                Capsule()
                    .fill(Palette.accent)
                    .frame(width: geometry.size.width * playedFraction)
                if pendingMs != nil {
                    Capsule()
                        .fill(Palette.accent.opacity(0.5))
                        .frame(width: geometry.size.width * previewFraction)
                }
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
        }
        .frame(height: 8)
        .contentShape(Rectangle())
        .focusable()
        .focusEffectDisabled()
        .focused($focusedControl, equals: .progress)
        .playerRemoteAdapter(
            .timeline,
            state: inputState,
            apply: applyPlayerInputOutcome
        )
        .accessibilityLabel("Playback position")
        .accessibilityValue(pendingMs == nil ? "Timeline" : "Previewing \(formatTime(pendingMs!))")
        .accessibilityHint("Left or right previews. Select commits. Menu cancels.")
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
                systemImage: controller.selectedHeight == nil && !controller.selectedQualityIsOriginal
                    ? "checkmark"
                    : "wand.and.stars"
            )
        }
        Button {
            controller.selectOriginalQuality()
            #if os(iOS)
            dismissOptionMenu()
            #endif
            revealControls()
        } label: {
            Label(
                "Original",
                systemImage: controller.selectedQualityIsOriginal
                    ? "checkmark"
                    : "film"
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
    case details
    case debug

    var id: Self { self }

    private static let defaultsKey = "plurx.playbackInfoMode"

    static var persisted: Self {
        guard let raw = UserDefaults.standard.string(forKey: defaultsKey),
              let mode = Self(rawValue: raw)
        else { return .standard }
        return mode
    }

    func persist() {
        UserDefaults.standard.set(rawValue, forKey: Self.defaultsKey)
    }

    var label: String {
        switch self {
        case .mini: return "Compact"
        case .standard: return "Overview"
        case .details: return "Details"
        case .debug: return "Diagnostics"
        }
    }
}

enum PlaybackStatTone {
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

struct PlaybackWaitPresentation: Equatable {
    let title: String
    let detail: String

    static func make(runwaySeconds: Double?, httpWaitCount: Int?) -> Self {
        let runway = max(0, runwaySeconds ?? 0)
        let waitText: String
        if let httpWaitCount {
            let waits = max(0, httpWaitCount)
            switch waits {
            case 0: waitText = "no server HTTP waits"
            case 1: waitText = "1 server HTTP wait"
            default: waitText = "\(waits) server HTTP waits"
            }
        } else {
            waitText = "server wait state unavailable"
        }
        return Self(
            title: "Presentation waiting…",
            detail: String(format: "%.1f s client loaded · %@", runway, waitText)
        )
    }
}

/// Whether a row belongs in the two-column grid or in the notes strip that
/// spans the panel underneath it. Sentence-shaped values leave the grid so
/// they can never stretch a column.
enum PlaybackLedgerPlacement: Equatable {
    case grid
    case notes
}

/// One label/value pair. Rows are plain data rather than views so the
/// alignment and density decisions below can inspect them before anything is
/// laid out.
struct ApplePlaybackInfoField: Identifiable {
    let id: String
    let label: String
    let section: String
    let modes: Set<PlaybackStatsMode>
    let placement: PlaybackLedgerPlacement
    let always: Bool

    init(
        _ id: String,
        _ label: String,
        _ section: String,
        _ modes: [PlaybackStatsMode],
        placement: PlaybackLedgerPlacement = .grid,
        always: Bool = false
    ) {
        self.id = id
        self.label = label
        self.section = section
        self.modes = Set(modes)
        self.placement = placement
        self.always = always
    }
}

/// Apple-applicable rows from playback-info-fields.json, in fixture order.
/// The renderer and parity test both consume this list.
let applePlaybackInfoFields: [ApplePlaybackInfoField] = [
    .init("method", "Method", "PLAYBACK", [.standard, .details, .debug]),
    .init("position", "Position", "PLAYBACK", [.standard, .details, .debug]),
    .init("reason", "Reason", "PLAYBACK", [.standard, .details, .debug], placement: .notes),
    .init("build", "Build", "PLAYBACK", [.debug], always: true),
    .init("transport", "Transport", "PLAYBACK", [.debug], placement: .notes),
    .init("file_id", "File ID", "PLAYBACK", [.debug]),
    .init("session", "Session", "PLAYBACK", [.debug], placement: .notes),
    .init("source_video", "Original video", "SOURCE", [.standard, .details, .debug], placement: .notes),
    .init("source_resolution", "Original resolution", "SOURCE", [.standard, .details, .debug]),
    .init("source_bitrate", "Source bitrate", "SOURCE", [.standard, .details, .debug]),
    .init("container", "Container", "SOURCE", [.standard, .details, .debug]),
    .init("source_audio", "Source audio track", "SOURCE", [.standard, .details, .debug], placement: .notes),
    .init("source_file", "File", "SOURCE", [.debug], placement: .notes),
    .init("av_offset", "AV offset", "SOURCE", [.debug], always: true),
    .init("decode_resolution", "Playing resolution", "NOW DECODING", [.mini, .standard, .details, .debug], always: true),
    .init("stream_format", "Stream format", "NOW DECODING", [.standard, .details, .debug], always: true),
    .init("device_audio", "Device audio output", "NOW DECODING", [.standard, .details, .debug], always: true),
    .init("dynamic_range", "Dynamic range", "NOW DECODING", [.standard, .details, .debug], placement: .notes),
    .init("decode_audio", "Stream audio track", "NOW DECODING", [.standard, .details, .debug], placement: .notes),
    .init("player_state", "Player state", "NOW DECODING", [.mini, .standard, .details, .debug]),
    .init("waiting_reason", "Waiting reason", "NOW DECODING", [.debug], placement: .notes),
    .init("stalls", "Buffering interruptions", "NOW DECODING", [.standard, .details, .debug]),
    .init("subtitles", "Subtitles", "NOW DECODING", [.standard, .details, .debug]),
    .init("source_read", "Source read", "BUFFERING / DELIVERY", [.debug], always: true),
    .init("server_ready", "Ready on server", "BUFFERING / DELIVERY", [.standard, .details, .debug], always: true),
    .init("ready_state", "Ready state", "BUFFERING / DELIVERY", [.debug], always: true),
    .init("ready_anchor", "Ready anchor", "BUFFERING / DELIVERY", [.debug]),
    .init("ready_end", "Ready end", "BUFFERING / DELIVERY", [.debug]),
    .init("later_ready", "Later ready", "BUFFERING / DELIVERY", [.debug], placement: .notes),
    .init("http_wait", "HTTP wait", "BUFFERING / DELIVERY", [.standard, .details, .debug]),
    .init("client_loaded", "Buffered on device", "BUFFERING / DELIVERY", [.mini, .standard, .details, .debug]),
    .init("presentation", "Presentation", "BUFFERING / DELIVERY", [.standard, .details, .debug]),
    .init("presentation_age", "Last advance", "BUFFERING / DELIVERY", [.standard, .details, .debug]),
    .init("delivery_rate", "Server response rate", "BUFFERING / DELIVERY", [.standard, .details, .debug]),
    .init("delivered", "Server responses completed", "BUFFERING / DELIVERY", [.standard, .details, .debug]),
    .init("delivery_idle", "Delivery idle", "BUFFERING / DELIVERY", [.debug]),
    .init("status_age", "Status sample age", "BUFFERING / DELIVERY", [.debug]),
    .init("observed_rate", "Observed download rate", "NETWORK", [.standard, .details, .debug]),
    .init("stream_rate", "Stream rate", "NETWORK", [.standard, .details, .debug]),
    .init("transferred", "Transferred", "NETWORK", [.debug]),
    .init("requests", "Requests", "NETWORK", [.debug]),
    .init("started_in", "Started in", "NETWORK", [.debug]),
    .init("status", "Server state", "SERVER", [.standard, .details, .debug], always: true),
    .init("encoder", "Encoder", "SERVER", [.standard, .details, .debug]),
    .init("encode_speed", "Encode speed", "SERVER", [.standard, .details, .debug]),
    .init("production_actual", "Production actual", "SERVER", [.standard, .details, .debug]),
    .init("production_target", "Production target", "SERVER", [.standard, .details, .debug]),
    .init("producer_state", "Producer", "SERVER", [.standard, .details, .debug]),
    .init("fetch_reserve", "Fetch reserve", "SERVER", [.debug]),
    .init("ahead_bytes", "Fetch reserve bytes", "SERVER", [.debug]),
    .init("produced", "Produced", "SERVER", [.debug]),
    .init("pacing", "Pacing", "SERVER", [.debug]),
    .init("held", "Held", "SERVER", [.debug]),
    .init("hold_reason", "Hold reason", "SERVER", [.debug]),
    .init("suspend_count", "Suspend count", "SERVER", [.debug]),
    .init("request_idle", "Request idle", "SERVER", [.debug]),
    .init("last_request", "Last request", "SERVER", [.debug], placement: .notes),
    .init("playlist", "Playlist", "SERVER", [.debug]),
    .init("published_end", "Published end", "SERVER", [.debug]),
    .init("fetched_end", "Fetched end", "SERVER", [.debug]),
    .init("control", "Control", "SERVER", [.standard, .details, .debug]),
    .init("surface_kind", "Surface", "SURFACE", [.standard, .details, .debug]),
    .init("surface_class", "Fault", "SURFACE", [.standard, .details, .debug]),
    .init("surface_source", "Source", "SURFACE", [.debug]),
    .init("surface_ids", "Attached/intent", "SURFACE", [.debug]),
    .init("surface_history", "History", "SURFACE", [.debug], placement: .notes),
    // PREPARED SWITCH (M3). Debug only, and always a string: "Not measured"
    // and "0 dropped" are different facts and the row has to be able to say
    // which one it is.
    .init("switch_frames", "Frames at the switch", "PREPARED SWITCH", [.debug], always: true),
    .init("switch_audio", "Audio at the switch", "PREPARED SWITCH", [.debug], always: true),
    .init("switch_visible_in", "Tap to new quality", "PREPARED SWITCH", [.debug], always: true),
]

/// Adapts the attached finite player into the shared playback-info presentation.
struct PlaybackStatsView: View {
    @ObservedObject var controller: PlayerController
    let title: String
    @Binding var mode: PlaybackStatsMode
    let onDismiss: () -> Void

    static func contractFieldLabels(for mode: PlaybackStatsMode) -> [String] {
        applePlaybackInfoFields.filter { $0.modes.contains(mode) }.map(\.label)
    }

    var body: some View {
        GeometryReader { geometry in
            let inset: CGFloat = 20
            #if os(tvOS)
            let maximum: CGFloat = 1100
            #else
            let maximum: CGFloat = 736
            #endif
            ZStack(alignment: .topTrailing) {
                #if os(iOS)
                if mode != .mini {
                    Color.clear.contentShape(Rectangle()).onTapGesture { onDismiss() }
                }
                #endif
                PlaybackInfoPanel(
                    title: title,
                    facts: presentationFacts,
                    mode: $mode,
                    onClose: onDismiss
                )
                .frame(width: max(0, min(maximum, geometry.size.width - inset * 2)))
                .frame(maxHeight: max(0, geometry.size.height - inset * 2), alignment: .top)
                .padding(inset)
            }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        }
    }

    private var presentationFacts: [PlaybackInfoFact] {
        applePlaybackInfoFields.compactMap { field in
            let supplied = contractValue(for: field.id)
            guard let text = supplied?.value ?? (field.always ? "Not reported" : nil) else { return nil }
            let notes = [playbackInfoExplanation(field.id), supplied?.note].compactMap { $0 }.joined(separator: " ")
            return PlaybackInfoFact(
                id: field.id, label: field.label, value: text,
                note: notes.isEmpty ? nil : notes,
                group: playbackInfoGroup(field.section),
                diagnosticOnly: !field.modes.contains(.standard)
            )
        }
    }

    // MARK: - Debug field set

    private struct ContractFieldValue {
        let value: String
        var tone: PlaybackStatTone = .neutral
        var note: String?
    }

    private func contractValue(for id: String) -> ContractFieldValue? {
        let snapshot = controller.currentDiagnosticSnapshot
        let status = controller.sessionStatus
        let source = controller.decision?.source
        switch id {
        case "method":
            return ContractFieldValue(value: controller.methodLabel, tone: playbackTone)
        case "position":
            let rate = controller.player.rate
            let rateClause = rate != 0 && rate != 1 ? String(format: " · %.1f×", rate) : ""
            return ContractFieldValue(
                value: "\(formatTime(controller.currentMs)) / \(formatTime(controller.knownDurationMs))\(rateClause)"
            )
        case "reason":
            guard let reasons = controller.decision?.reasons, !reasons.isEmpty else { return nil }
            return ContractFieldValue(value: reasons.joined(separator: " · "))
        case "build":
            return ContractFieldValue(value: buildLabel)
        case "transport":
            return ContractFieldValue(value: controller.currentSessionId == nil
                ? "Continuous file · range requests · Apple AVPlayer"
                : "Segmented HLS · Apple AVPlayer")
        case "file_id":
            guard let fileID = controller.decision?.fileId else { return nil }
            return ContractFieldValue(value: "#\(fileID)")
        case "session":
            return controller.currentSessionId.map { ContractFieldValue(value: $0) }
        case "source_video":
            guard let source else { return nil }
            let parts = [
                source.videoCodec?.uppercased(),
                source.videoProfile,
                source.bitDepth.map { "\($0)-bit" },
                source.hdrFormat ?? source.hdr?.uppercased(),
            ].compactMap { $0 }
            guard !parts.isEmpty else { return nil }
            return ContractFieldValue(value: parts.joined(separator: " · "))
        case "source_resolution":
            guard let width = source?.width, let height = source?.height else { return nil }
            return ContractFieldValue(value: "\(width)×\(height)")
        case "source_bitrate":
            return source?.bitrate.map { ContractFieldValue(value: bitRate($0)) }
        case "container":
            return source?.container.map { ContractFieldValue(value: $0.uppercased()) }
        case "source_audio":
            guard let selectedAudioDescription else { return nil }
            let extra = max(0, controller.audioTracks.count - 1)
            return ContractFieldValue(
                value: selectedAudioDescription + (extra > 0 ? " · +\(extra) tracks" : "")
            )
        case "source_file":
            guard let raw = controller.decision?.delivery?.url ?? controller.decision?.playUrl else {
                return nil
            }
            let decoded = raw.removingPercentEncoding ?? raw
            return ContractFieldValue(value: decoded)
        case "av_offset":
            let applied = controller.decision?.audioOffsetMs ?? 0
            let declared = controller.decision?.declaredOffsetMs
            let note = declared.flatMap { $0 != applied ? "Container declared \($0) ms" : nil }
            return ContractFieldValue(value: "\(applied) ms", note: note)
        case "decode_resolution":
            return ContractFieldValue(value: playbackInfoResolution(controller.presentationSize))
        case "stream_format", "device_audio":
            return ContractFieldValue(value: "Not reported", tone: .muted)
        case "dynamic_range":
            guard let range = PlayerView.dynamicRangeSummary(
                source: source,
                delivered: controller.deliveredRange,
                displayHDR: Caps.displayIsHDR,
                reasons: controller.decision?.reasons
            ) else { return nil }
            return ContractFieldValue(value: range)
        case "decode_audio":
            return nil
        case "source_read":
            return ContractFieldValue(value: "Unavailable", tone: .muted)
        case "server_ready":
            switch status?.serverReadyState?.lowercased() {
            case "missing":
                return ContractFieldValue(value: "0.0 s", tone: .critical)
            case "ready":
                guard let seconds = status?.serverReadySeconds else {
                    return ContractFieldValue(value: "Unavailable", tone: .muted)
                }
                return ContractFieldValue(
                    value: String(format: "%.1f s", max(0, seconds)),
                    tone: runwayTone(seconds)
                )
            default:
                return ContractFieldValue(value: "Unavailable", tone: .muted)
            }
        case "ready_state":
            let state = status?.serverReadyState?.lowercased()
            let value = state == "ready" ? "Ready" : state == "missing" ? "Missing" : "Unavailable"
            let tone: PlaybackStatTone = state == "ready" ? .good : state == "missing" ? .critical : .muted
            return ContractFieldValue(value: value, tone: tone)
        case "ready_anchor":
            return status?.serverReadyAnchorMs.map { ContractFieldValue(value: "\($0) ms") }
        case "ready_end":
            return status?.serverReadyEndMs.map { ContractFieldValue(value: "\($0) ms") }
        case "later_ready":
            guard let start = status?.serverNextReadyStartMs,
                  let end = status?.serverNextReadyEndMs else { return nil }
            return ContractFieldValue(value: "\(start)–\(end) ms")
        case "http_wait":
            guard let count = status?.httpWaitCount else { return nil }
            let note = count > 0 ? [
                status?.httpWaitOldestMs.map { "oldest \($0) ms" },
                status?.httpWaitSegment.map { "segment \($0)" },
            ].compactMap { $0 }.joined(separator: " · ") : nil
            return ContractFieldValue(
                value: "\(max(0, count)) active",
                tone: count > 0 ? .warning : .good,
                note: note
            )
        case "client_loaded":
            guard let runway = snapshot.runway else { return nil }
            return ContractFieldValue(value: String(format: "%.1f s", runway), tone: runwayTone(runway))
        case "presentation":
            let playerState = normalizedPlayerState(snapshot.timeControlStatus)
            return ContractFieldValue(
                value: playerState == "Playing" ? "Advancing" : playerState,
                tone: playerStateTone(snapshot.timeControlStatus)
            )
        case "presentation_age":
            guard let age = controller.presentationProgressAgeMs else { return nil }
            return ContractFieldValue(value: "\(age) ms", tone: idleTone(age))
        case "player_state":
            return ContractFieldValue(
                value: normalizedPlayerState(snapshot.timeControlStatus),
                tone: playerStateTone(snapshot.timeControlStatus)
            )
        case "waiting_reason":
            return snapshot.waitingReason.map { ContractFieldValue(value: $0, tone: .warning) }
        case "stalls":
            guard let supply = snapshot.accessStalls ?? controller.stalls else { return nil }
            return ContractFieldValue(
                value: "\(supply) (\(supply) supply · 0 decode)",
                tone: stallTone(supply)
            )
        case "subtitles":
            let subtitle = selectedSubtitleParts
            return ContractFieldValue(value: subtitle.name, note: subtitle.delivery)
        case "delivery_rate":
            guard let delivered = status?.deliveredBps else { return nil }
            let idle = (status?.deliveredIdleMs ?? 0) >= 1_000 ? " · idle" : ""
            return ContractFieldValue(value: bitRate(delivered) + idle, tone: networkTone)
        case "observed_rate":
            return snapshot.observedBitrateBps.map { ContractFieldValue(value: bitRate(Int($0))) }
        case "stream_rate":
            return snapshot.indicatedBitrateBps.map { ContractFieldValue(value: bitRate(Int($0))) }
        case "delivered":
            return status?.deliveredBytes.map { ContractFieldValue(value: byteCount($0)) }
        case "transferred":
            return snapshot.bytesTransferred.map { ContractFieldValue(value: byteCount($0)) }
        case "requests":
            return snapshot.mediaRequests.map { ContractFieldValue(value: String($0)) }
        case "delivery_idle":
            return status?.deliveredIdleMs.map {
                ContractFieldValue(value: "\($0) ms", tone: idleTone($0, suspended: status?.suspended ?? false))
            }
        case "status_age":
            return controller.sessionStatusAgeMs.map {
                ContractFieldValue(value: "\($0) ms", tone: idleTone($0))
            }
        case "started_in":
            return controller.lastTTFFMs.map {
                ContractFieldValue(value: String(format: "%.1f s", Double($0) / 1_000.0))
            }
        case "status":
            return playbackServerStatus
        case "encoder":
            return (status?.encoder ?? controller.encoder).map { ContractFieldValue(value: $0) }
        case "encode_speed":
            if let recent = status?.recentSpeed {
                return ContractFieldValue(value: String(format: "%.2f×", recent), tone: encodeTone(speed: recent, status: status!))
            }
            if let average = status?.speed {
                return ContractFieldValue(value: String(format: "%.2f× (avg)", average), tone: encodeTone(speed: average, status: status!))
            }
            return nil
        case "production_actual":
            return status?.productionAheadSeconds.map {
                ContractFieldValue(value: String(format: "%.1f s", Double(max(0, $0))))
            }
        case "production_target":
            return status?.productionTargetSeconds.map {
                ContractFieldValue(value: String(format: "%.1f s", Double(max(0, $0))))
            }
        case "producer_state":
            return status?.producerState.map {
                ContractFieldValue(value: $0.replacingOccurrences(of: "_", with: " ").capitalized)
            }
        case "fetch_reserve":
            guard let seconds = status?.aheadSeconds else { return nil }
            let note = status?.suspended == true
                ? "Holding buffer\(status.map(holdReleaseDescription) ?? "")"
                : nil
            return ContractFieldValue(
                value: String(format: "%.1f s", Double(max(0, seconds))),
                note: note
            )
        case "ahead_bytes":
            return status?.aheadBytes.map { ContractFieldValue(value: byteCount($0)) }
        case "produced":
            return status?.outTimeMs.map { ContractFieldValue(value: formatTime($0)) }
        case "pacing":
            return status?.readrate.map { ContractFieldValue(value: String(format: "%.2f×", $0)) }
        case "held":
            return status?.suspended.map { ContractFieldValue(value: $0 ? "Yes" : "No") }
        case "hold_reason":
            return status?.holdReason.map { ContractFieldValue(value: $0) }
        case "suspend_count":
            return status?.suspendCount.map { ContractFieldValue(value: String($0)) }
        case "request_idle":
            return status?.idleSeconds.map { ContractFieldValue(value: String(format: "%.1f s", Double($0))) }
        case "last_request":
            return status?.lastRequest.map { ContractFieldValue(value: $0) }
        case "playlist":
            return status?.playlistShape.map { ContractFieldValue(value: $0) }
        case "published_end":
            return status?.publishedEndMs.map { ContractFieldValue(value: "\($0) ms") }
        case "fetched_end":
            return status?.fetchedEndMs.map { ContractFieldValue(value: "\($0) ms") }
        case "control":
            return controller.playbackControlSummary.map { ContractFieldValue(value: $0) }
        // "What was that overlay" has an answer inside the product, which is
        // the whole of the contract's §5.
        case "surface_kind":
            let surface = controller.surface.surface
            return ContractFieldValue(
                value: surface.kind.rawValue,
                tone: surface.kind == .blocking ? .critical : .neutral
            )
        case "surface_class":
            guard let fault = controller.surface.surface.fault else {
                return ContractFieldValue(value: "none", tone: .muted)
            }
            let demoted = fault.demoted ? " · demoted" : ""
            let stopped = fault.playerStopped ? " · owner stopped" : ""
            return ContractFieldValue(
                value: "\(fault.cls.rawValue)\(demoted)\(stopped)",
                tone: fault.playerStopped ? .critical : .neutral,
                note: fault.detail
            )
        case "surface_source":
            return controller.surface.surface.source.map { ContractFieldValue(value: $0) }
        case "surface_ids":
            guard let fault = controller.surface.surface.fault else { return nil }
            let intent = fault.intent.map { String($0) } ?? "—"
            let position = fault.positionMs.map { " · \($0) ms" } ?? ""
            return ContractFieldValue(value: "\(fault.attached) / \(intent)\(position)")
        case "surface_history":
            let summary = controller.surfaceHistory.ledgerSummary
            return summary.isEmpty ? nil : ContractFieldValue(value: summary)
        case "switch_frames":
            return ContractFieldValue(value: controller.preparedSwitchReading.frames)
        case "switch_audio":
            return ContractFieldValue(value: controller.preparedSwitchReading.audio)
        case "switch_visible_in":
            return ContractFieldValue(value: controller.preparedSwitchReading.visibleIn)
        default:
            return nil
        }
    }

    private var playbackServerStatus: ContractFieldValue {
        guard let status = controller.sessionStatus else {
            return ContractFieldValue(value: "No server-side session", tone: .muted)
        }
        if let state = status.producerState { return ContractFieldValue(value: state) }
        if status.suspended == true {
            return ContractFieldValue(value: "Holding buffer", tone: .good)
        }
        return ContractFieldValue(value: "Active", tone: .good)
    }

    private func normalizedPlayerState(_ raw: String?) -> String {
        if controller.isPlaybackBlocked { return "Failed" }
        if controller.finished { return "Ended" }
        guard let raw = raw?.lowercased() else { return controller.isPlaying ? "Playing" : "Paused" }
        if raw.contains("wait") { return "Buffering" }
        if raw.contains("play") { return "Playing" }
        return controller.isPlaying ? "Playing" : "Paused"
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
        let megabits = Double(bits) / 1_000_000.0
        if megabits < 1 {
            return String(format: "%.0f kb/s", Double(bits) / 1_000.0)
        }
        if megabits < 10 {
            return String(format: "%.1f Mb/s", megabits)
        }
        return String(format: "%.0f Mb/s", megabits)
    }

    private func byteCount(_ bytes: Int) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }

}
