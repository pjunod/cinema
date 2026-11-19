import SwiftUI
import UIKit

extension ViewerAppearance {
    var preferredColorScheme: ColorScheme? {
        switch self {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }
}

extension ViewerTheme {
    var fontDesign: Font.Design {
        self == .terminal ? .monospaced : .default
    }
}

private struct PaletteValues {
    let bg: UInt32
    let surface: UInt32
    let surfaceHi: UInt32
    let accent: UInt32
    let buttonInk: UInt32
    let onBg: UInt32
    let muted: UInt32
    let outline: UInt32
}

private extension UIColor {
    convenience init(rgb: UInt32) {
        self.init(
            red: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: 1
        )
    }
}

/// The three native palettes shared with Android. UIKit's dynamic provider
/// resolves light/dark from the effective trait collection, while the selected
/// theme comes from the same device-local preference that AppModel publishes.
enum Palette {
    static var selectedFill: Color { onBg }
    static var selectedInk: Color { bg }
    static var focusRing: Color { accent }
    static var heroScrim: Color { bg }
    static var heroInk: Color { onBg }
    static var bg: Color { adaptive(\.bg) }
    static var surface: Color { adaptive(\.surface) }
    static var surfaceHi: Color { adaptive(\.surfaceHi) }
    static var accent: Color { adaptive(\.accent) }
    static var buttonInk: Color { adaptive(\.buttonInk) }
    static var onBg: Color { adaptive(\.onBg) }
    static var muted: Color { adaptive(\.muted) }
    static var outline: Color { adaptive(\.outline) }

    /// Playback stays true black in every theme and appearance. These surfaces
    /// sit over the movie, not the app's room chrome.
    static let playerChrome = Color(red: 0x0A / 255, green: 0x0A / 255, blue: 0x0C / 255)

    private static func adaptive(_ keyPath: KeyPath<PaletteValues, UInt32>) -> Color {
        Color(uiColor: UIColor { traits in
            let stored = UserDefaults.standard.string(forKey: "plurx.theme")
            let theme = ViewerTheme(rawValue: stored ?? "") ?? .noirr
            let values = values(theme: theme, dark: traits.userInterfaceStyle != .light)
            return UIColor(rgb: values[keyPath: keyPath])
        })
    }

    private static func values(theme: ViewerTheme, dark: Bool) -> PaletteValues {
        switch (theme, dark) {
        case (.meridian, true):
            return PaletteValues(
                bg: 0x141416, surface: 0x1E1E21, surfaceHi: 0x29292D,
                accent: 0xE5A00D, buttonInk: 0x171205, onBg: 0xF5F5F6,
                muted: 0xAAAAB2, outline: 0xAAAAB2
            )
        case (.meridian, false):
            return PaletteValues(
                bg: 0xF5F4F1, surface: 0xFFFFFF, surfaceHi: 0xEAE8E3,
                accent: 0x805600, buttonInk: 0xFFFFFF, onBg: 0x202024,
                muted: 0x606068, outline: 0x606068
            )
        case (.classic, true):
            return PaletteValues(
                bg: 0x0E0F13, surface: 0x171922, surfaceHi: 0x1E2230,
                accent: 0x6EA8FE, buttonInk: 0x0E0F13, onBg: 0xE8EAF0,
                muted: 0x9AA2B4, outline: 0x2A2F3E
            )
        case (.classic, false):
            return PaletteValues(
                bg: 0xF6F7F9, surface: 0xFFFFFF, surfaceHi: 0xEEF1F6,
                accent: 0x2F6FE0, buttonInk: 0xFFFFFF, onBg: 0x1A1D24,
                muted: 0x5B6472, outline: 0xDDE2EA
            )
        case (.terminal, true):
            return PaletteValues(
                bg: 0x050705, surface: 0x0B100B, surfaceHi: 0x111A12,
                accent: 0x3FE170, buttonInk: 0x041508, onBg: 0xC9E8C0,
                muted: 0x7A9670, outline: 0x1E3020
            )
        case (.terminal, false):
            return PaletteValues(
                bg: 0xEEE8D5, surface: 0xFDF6E3, surfaceHi: 0xE6DFC8,
                accent: 0x6E7F00, buttonInk: 0xFDF6E3, onBg: 0x073642,
                muted: 0x657B83, outline: 0xD5CDB4
            )
        case (.noirr, true):
            return PaletteValues(
                bg: 0x0A0A0C, surface: 0x101014, surfaceHi: 0x16161B,
                accent: 0xE5484D, buttonInk: 0x0A0A0C, onBg: 0xEDEDEF,
                muted: 0x9A9AA3, outline: 0x29292E
            )
        case (.noirr, false):
            return PaletteValues(
                bg: 0xF2EFE8, surface: 0xFAF8F2, surfaceHi: 0xFFFFFF,
                accent: 0xC2343A, buttonInk: 0xFFFFFF, onBg: 0x1A1A1E,
                muted: 0x5D5C63, outline: 0xDEDAD2
            )
        }
    }
}

#if os(tvOS)
/// The house tvOS button: a quiet capsule at rest that turns solid
/// `Palette.onBg` with `Palette.bg` ink when focused. White means focus; the
/// accent colour is kept for progress and live marks, so no red here.
/// `prominent` survives for call-site compatibility and only strengthens the
/// resting fill a little.
struct TVReadableButtonStyle: ButtonStyle {
    let prominent: Bool
    /// Compact is the toolbar / list-row size (56 pt, 24 pt semibold text);
    /// the default 66 pt size is for a button that is the only thing on its line.
    var compact: Bool = false
    var focusScale: CGFloat = 1.05

    static func foregroundColor(prominent: Bool, focused: Bool) -> Color {
        focused ? Palette.bg : Palette.onBg
    }

    static func backgroundColor(prominent: Bool, focused: Bool) -> Color {
        focused ? Palette.onBg : Palette.onBg.opacity(prominent ? 0.2 : 0.12)
    }

    func makeBody(configuration: Configuration) -> Body {
        Body(configuration: configuration, prominent: prominent, compact: compact, focusScale: focusScale)
    }

    struct Body: View {
        let configuration: ButtonStyle.Configuration
        let prominent: Bool
        var compact: Bool = false
        var focusScale: CGFloat = 1.05
        @Environment(\.isFocused) private var isFocused
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @Environment(\.isEnabled) private var isEnabled

        var body: some View {
            configuration.label
                .font(compact ? Font.system(size: 24, weight: .semibold) : nil)
                .foregroundStyle(
                    TVReadableButtonStyle.foregroundColor(
                        prominent: prominent,
                        focused: isFocused
                    )
                )
                .padding(.horizontal, compact ? 24 : 32)
                .padding(.vertical, compact ? 10 : 14)
                .frame(minHeight: compact ? 56 : 66)
                .background(
                    TVReadableButtonStyle.backgroundColor(
                        prominent: prominent,
                        focused: isFocused
                    ),
                    in: Capsule()
                )
                .contentShape(Capsule())
                .scaleEffect(reduceMotion ? 1 : (isFocused ? focusScale : (configuration.isPressed ? 0.98 : 1)))
                .shadow(color: .black.opacity(isFocused ? 0.4 : 0), radius: 16, y: 8)
                .opacity(isEnabled ? 1 : 0.45)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.14), value: isFocused)
                .remoteNativeFocus(isFocused)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.08), value: configuration.isPressed)
        }
    }
}

/// Fullscreen Live TV chrome sits over video, so it uses fixed colours rather
/// than the app palette: a translucent white pill that turns solid white with
/// black ink on focus.
struct LiveSurfacePillStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        Renderer(configuration: configuration)
    }

    private struct Renderer: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var isFocused
        @Environment(\.accessibilityReduceMotion) private var reduceMotion

        var body: some View {
            configuration.label
                .labelStyle(.titleAndIcon)
                .font(LiveTvType.surfaceButton)
                .foregroundStyle(isFocused ? Color.black : Color.white)
                .padding(.leading, 22)
                .padding(.trailing, 26)
                .frame(height: 56)
                .background(isFocused ? Color.white : Color.white.opacity(0.14), in: Capsule())
                .contentShape(Capsule())
                .scaleEffect(reduceMotion ? 1 : (isFocused ? 1.05 : (configuration.isPressed ? 0.98 : 1)))
                .shadow(color: .black.opacity(isFocused ? 0.45 : 0), radius: 14, y: 8)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.14), value: isFocused)
                .remoteNativeFocus(isFocused)
        }
    }
}

/// Compact trailing action for shelf headers: the same quiet pill as the
/// compact `TVReadableButtonStyle`, solid `Palette.onBg` only when focused.
struct TVShelfActionButtonStyle: ButtonStyle {
    static func foregroundColor(focused: Bool) -> Color {
        TVReadableButtonStyle.foregroundColor(prominent: false, focused: focused)
    }
    static func backgroundColor(focused: Bool) -> Color {
        TVReadableButtonStyle.backgroundColor(prominent: false, focused: focused)
    }

    func makeBody(configuration: Configuration) -> Body {
        Body(configuration: configuration)
    }

    struct Body: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var isFocused
        @Environment(\.accessibilityReduceMotion) private var reduceMotion

        var body: some View {
            configuration.label
                .font(.system(size: 24, weight: .semibold))
                .foregroundStyle(TVShelfActionButtonStyle.foregroundColor(focused: isFocused))
                .padding(.horizontal, 24)
                .padding(.vertical, 10)
                .frame(minWidth: 56, minHeight: 56)
                .background(
                    TVShelfActionButtonStyle.backgroundColor(focused: isFocused),
                    in: Capsule()
                )
                .contentShape(Capsule())
                .scaleEffect(reduceMotion ? 1 : (isFocused ? 1.05 : (configuration.isPressed ? 0.97 : 1)))
                .shadow(color: .black.opacity(isFocused ? 0.4 : 0), radius: 14, y: 6)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.14), value: isFocused)
                .remoteNativeFocus(isFocused)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.08), value: configuration.isPressed)
        }
    }
}

/// Player controls sit over the picture, so they use fixed colours: a
/// translucent white labelled pill that turns solid white with black ink when
/// focused. Icon-only buttons keep a 64 pt minimum width.
struct TVPlayerControlButtonStyle: ButtonStyle {
    static let height: CGFloat = 64
    static let minWidth: CGFloat = 64
    static let iconSize: CGFloat = 26
    static let fontSize: CGFloat = 24
    static let horizontalPadding: CGFloat = 22

    func makeBody(configuration: Configuration) -> Body {
        Body(configuration: configuration)
    }

    struct Body: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var isFocused
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @Environment(\.isEnabled) private var isEnabled

        var body: some View {
            configuration.label
                .font(.system(size: TVPlayerControlButtonStyle.fontSize, weight: .semibold))
                .lineLimit(1)
                .foregroundStyle(isFocused ? Color.black : Color.white)
                .padding(.horizontal, TVPlayerControlButtonStyle.horizontalPadding)
                .frame(
                    minWidth: TVPlayerControlButtonStyle.minWidth,
                    minHeight: TVPlayerControlButtonStyle.height,
                    maxHeight: TVPlayerControlButtonStyle.height
                )
                .background(isFocused ? Color.white : Color.white.opacity(0.14), in: Capsule())
                .contentShape(Capsule())
                .scaleEffect(reduceMotion ? 1 : (isFocused ? 1.05 : (configuration.isPressed ? 0.97 : 1)))
                .shadow(color: .black.opacity(isFocused ? 0.45 : 0), radius: 12, y: 6)
                .opacity(isEnabled ? 1 : 0.42)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.12), value: isFocused)
                .remoteNativeFocus(isFocused)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.08), value: configuration.isPressed)
        }
    }
}

/// Ten-foot player chrome metrics: a large title, the quality pills, a
/// timeline whose focus is obvious (taller bar, white knob and halo) and
/// labelled control pills.
enum TVPlayerChromeMetrics {
    static let titleFontSize: CGFloat = 48
    static let subtitleFontSize: CGFloat = 27
    static let subtitleOpacity: Double = 0.82
    static let timeFontSize: CGFloat = 27
    static let timelineHeight: CGFloat = 8
    static let focusedTimelineHeight: CGFloat = 14
    static let knobSize: CGFloat = 30
    static let knobHaloSize: CGFloat = 48
}

/// The featured hero lifts and gains a `Palette.onBg` ring when focused.
struct TVHeroButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> Body {
        Body(configuration: configuration)
    }

    struct Body: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var isFocused
        @Environment(\.accessibilityReduceMotion) private var reduceMotion

        var body: some View {
            configuration.label
                .overlay {
                    RoundedRectangle(cornerRadius: 24, style: .continuous)
                        .stroke(Palette.onBg.opacity(isFocused ? 1 : 0),
                                lineWidth: 4)
                }
                .scaleEffect(reduceMotion ? 1 : (isFocused ? 1.012 : (configuration.isPressed ? 0.994 : 1)))
                .shadow(
                    color: .black.opacity(isFocused ? 0.5 : 0),
                    radius: 20,
                    y: 8
                )
                .animation(reduceMotion ? nil : .easeOut(duration: 0.15), value: isFocused)
                .remoteNativeFocus(isFocused)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.08), value: configuration.isPressed)
        }
    }
}

/// Shelf cards: on focus a 4 pt `Palette.onBg` ring about 6 pt outside the
/// card, a 6% lift and a black shadow. No accent strokes.
struct TVMediaCardButtonStyle: ButtonStyle {
    static let ringWidth: CGFloat = 4
    /// Gap between the card edge and the inner edge of the ring.
    static let contentClearance: CGFloat = 6
    static let focusScale: CGFloat = 1.06

    func makeBody(configuration: Configuration) -> Body {
        Body(configuration: configuration)
    }

    struct Body: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var isFocused
        @Environment(\.accessibilityReduceMotion) private var reduceMotion

        var body: some View {
            let shape = RoundedRectangle(cornerRadius: 13, style: .continuous)
            // A stroke is centred on its path, so the path sits half a ring
            // width further out than the clearance.
            let ringShape = shape.inset(
                by: -(TVMediaCardButtonStyle.contentClearance + TVMediaCardButtonStyle.ringWidth / 2)
            )
            configuration.label
                .background(Palette.bg.opacity(isFocused ? 0.96 : 0), in: shape)
                .overlay {
                    ringShape
                        .stroke(Palette.onBg, lineWidth: TVMediaCardButtonStyle.ringWidth)
                        .opacity(isFocused ? 1 : 0)
                }
                .scaleEffect(reduceMotion ? 1 : (isFocused ? TVMediaCardButtonStyle.focusScale : (configuration.isPressed ? 0.985 : 1)))
                .shadow(color: .black.opacity(isFocused ? 0.72 : 0), radius: 19, y: 10)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.14), value: isFocused)
                .remoteNativeFocus(isFocused)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.08), value: configuration.isPressed)
        }
    }
}
#endif

#if os(iOS)
/// Touch-sized counterpart to the Apple TV player control. It carries the same
/// dark, hairline-outlined formatting without importing tvOS focus behavior.
struct IOSPlayerControlButtonStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    static let width: CGFloat = 40
    static let height: CGFloat = 36
    static let iconSize: CGFloat = 17
    static let cornerRadius: CGFloat = 9

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: Self.iconSize, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: Self.width, height: Self.height)
            .background(
                Palette.playerChrome.opacity(0.68),
                in: RoundedRectangle(cornerRadius: Self.cornerRadius, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: Self.cornerRadius, style: .continuous)
                    .stroke(Palette.outline.opacity(0.7), lineWidth: 0.5)
            }
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .opacity(configuration.isPressed ? 0.76 : 1)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.08), value: configuration.isPressed)
    }
}

/// Full-width touch actions own their complete shape. The system bordered
/// styles add horizontal chrome outside an already-flexible label, which can
/// paint past a compact detail column's trailing inset.
struct IOSFullWidthActionButtonStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let prominent: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(prominent ? Color.white : Palette.onBg.opacity(0.7))
            .frame(maxWidth: .infinity, minHeight: 24)
            .padding(.vertical, 12)
            .background(
                prominent ? Palette.accent : Palette.surfaceHi,
                in: Capsule()
            )
            .overlay {
                if !prominent {
                    Capsule().stroke(Palette.outline, lineWidth: 1)
                }
            }
            .contentShape(Capsule())
            .opacity(configuration.isPressed ? 0.76 : 1)
    }
}
#endif

extension View {
    /// Poster/link focus treatment: the tvOS "card" lift on focus, a plain
    /// (untinted) button on touch platforms.
    @ViewBuilder
    func posterButtonStyle() -> some View {
        #if os(tvOS)
        self
            .buttonStyle(TVMediaCardButtonStyle())
            .focusEffectDisabled()
        #else
        self.buttonStyle(.plain)
        #endif
    }

    /// Large featured-card treatment kept separate from shelf cards so its
    /// focus indication follows the hero's rounded artwork bounds.
    @ViewBuilder
    func featuredButtonStyle() -> some View {
        #if os(tvOS)
        self
            .buttonStyle(TVHeroButtonStyle())
            .focusEffectDisabled()
        #else
        self.buttonStyle(.plain)
        #endif
    }

    /// Keeps shelf-header navigation readable under tvOS's tint and focus
    /// transformations while retaining a simple plain link on touch devices.
    @ViewBuilder
    func shelfActionButtonStyle() -> some View {
        #if os(tvOS)
        self
            .buttonStyle(TVShelfActionButtonStyle())
            .focusEffectDisabled()
        #else
        self.buttonStyle(.plain)
        #endif
    }

    /// Rounded-border text field on iOS; tvOS has no such style (its fields are
    /// focus-driven), so leave the default there.
    @ViewBuilder
    func plurxFieldStyle() -> some View {
        #if os(tvOS)
        self
        #else
        self.textFieldStyle(.roundedBorder)
        #endif
    }
}

// MARK: - Small formatting helpers

func progressFraction(_ watch: Watch?, runtimeMs: Int?) -> Double {
    guard let watch, let pos = watch.positionMs else { return 0 }
    let dur = watch.durationMs ?? runtimeMs ?? 0
    guard dur > 0 else { return 0 }
    return min(max(Double(pos) / Double(dur), 0), 1)
}

/// `m:ss` or `h:mm:ss` for scrubber / resume labels.
func formatTime(_ ms: Int) -> String {
    guard ms > 0 else { return "0:00" }
    let total = ms / 1000
    let h = total / 3600, m = (total % 3600) / 60, s = total % 60
    return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
}
