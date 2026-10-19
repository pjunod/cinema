// The menu-bar mark: the brand's geometric C with its dash (brand/icon-
// monochrome.svg), drawn as a template image so macOS tints it for light,
// dark and highlighted menu bars. State is carried by a shape in the dash
// position, not by colour: a dash when running, a hollow ring when starting,
// no dash when stopped, an exclamation bar when unhealthy and a question dot
// when unknown.
import AppKit
import PlurxTrayCore

public enum StatusIcon {
    public static let size = NSSize(width: 18, height: 18)

    public static func image(for state: ServiceState) -> NSImage {
        let image = NSImage(size: size, flipped: true) { rect in
            draw(state, in: rect)
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "plurx server \(state.title)"
        return image
    }

    /// Geometry scaled from the 1024-unit brand master: an arc of radius 250
    /// centred at (512, 512) opening to the right, stroke 110, and the dash
    /// from (445, 650) to (545, 650), stroke 48.
    static func draw(_ state: ServiceState, in rect: NSRect) {
        let scale = rect.width / 1024
        func p(_ x: CGFloat, _ y: CGFloat) -> NSPoint { NSPoint(x: rect.minX + x * scale, y: rect.minY + y * scale) }
        NSColor.black.setStroke()
        NSColor.black.setFill()

        let arc = NSBezierPath()
        // Flipped coordinates: 45° above the opening to 45° below it, the long way round.
        arc.appendArc(withCenter: p(512, 512), radius: 250 * scale, startAngle: -45, endAngle: 45, clockwise: true)
        arc.lineWidth = 110 * scale
        arc.lineCapStyle = .round
        arc.stroke()

        let mark = NSBezierPath()
        mark.lineCapStyle = .round
        switch state {
        case .running:
            mark.move(to: p(445, 650))
            mark.line(to: p(545, 650))
            mark.lineWidth = 48 * scale * 1.4
            mark.stroke()
        case .starting:
            let ring = NSBezierPath(ovalIn: NSRect(origin: p(455, 600), size: NSSize(width: 100 * scale, height: 100 * scale)))
            ring.lineWidth = 34 * scale
            ring.stroke()
        case .stopped:
            break
        case .unhealthy:
            mark.move(to: p(500, 440))
            mark.line(to: p(500, 600))
            mark.lineWidth = 70 * scale
            mark.stroke()
            NSBezierPath(ovalIn: NSRect(origin: p(462, 650), size: NSSize(width: 76 * scale, height: 76 * scale))).fill()
        case .unknown:
            NSBezierPath(ovalIn: NSRect(origin: p(450, 580), size: NSSize(width: 110 * scale, height: 110 * scale))).fill()
            let gap = NSBezierPath(ovalIn: NSRect(origin: p(480, 610), size: NSSize(width: 50 * scale, height: 50 * scale)))
            NSGraphicsContext.current?.compositingOperation = .clear
            gap.fill()
            NSGraphicsContext.current?.compositingOperation = .sourceOver
        }
    }
}
