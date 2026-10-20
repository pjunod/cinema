// Renders the MenuModel entries into an NSMenu. Items carry their Action so
// the target can dispatch without string matching.
import AppKit
import PlurxTrayCore

@MainActor
public final class ActionBox: NSObject {
    public let action: Action
    init(_ action: Action) { self.action = action }
}

@MainActor
public enum MenuRenderer {
    // Native menus do not wrap. Bound actual font width as well as input length;
    // tooltips and accessibility retain the complete text of shortened rows.
    public static let maximumTitleWidth: CGFloat = 320

    static func compactTitle(_ text: String, font: NSFont) -> String {
        let line = text.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
        guard !line.isEmpty else { return "" }
        var title = String(line.prefix(80))
        var shortened = title != line
        while !title.isEmpty {
            let candidate = title + (shortened ? "…" : "")
            if (candidate as NSString).size(withAttributes: [.font: font]).width <= maximumTitleWidth {
                return candidate
            }
            shortened = true
            title.removeLast()
        }
        return "…"
    }

    public static func render(_ entries: [MenuEntry], into menu: NSMenu, target: AnyObject, selector: Selector) {
        menu.removeAllItems()
        menu.autoenablesItems = false
        for entry in entries {
            let font = entry.kind == .header ? NSFont.menuFont(ofSize: 0).bold() : NSFont.menuFont(ofSize: 0)
            let title = compactTitle(entry.title, font: font)
            switch entry.kind {
            case .separator:
                menu.addItem(.separator())
            case .header:
                let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
                item.attributedTitle = NSAttributedString(
                    string: title,
                    attributes: [.font: font])
                item.isEnabled = false
                if title != entry.title { item.toolTip = entry.title }
                item.setAccessibilityLabel(entry.accessibility ?? entry.title)
                menu.addItem(item)
            case .info:
                let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
                item.isEnabled = false
                if title != entry.title { item.toolTip = entry.title }
                item.setAccessibilityLabel(entry.accessibility ?? entry.title)
                menu.addItem(item)
            case .command:
                let item = NSMenuItem(title: title, action: selector, keyEquivalent: "")
                item.target = target
                item.isEnabled = entry.enabled
                if let action = entry.action {
                    item.representedObject = ActionBox(action)
                    if action == .quit { item.keyEquivalent = "q" }
                }
                switch entry.check {
                case .on?: item.state = .on
                case .off?: item.state = .off
                case .mixed?: item.state = .mixed
                case nil: break
                }
                if title != entry.title { item.toolTip = entry.title }
                item.setAccessibilityLabel(entry.accessibility ?? entry.title)
                menu.addItem(item)
            }
        }
    }
}

extension NSFont {
    func bold() -> NSFont {
        NSFontManager.shared.convert(self, toHaveTrait: .boldFontMask)
    }
}
