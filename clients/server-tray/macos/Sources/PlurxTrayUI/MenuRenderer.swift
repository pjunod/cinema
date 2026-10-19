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
    public static func render(_ entries: [MenuEntry], into menu: NSMenu, target: AnyObject, selector: Selector) {
        menu.removeAllItems()
        menu.autoenablesItems = false
        for entry in entries {
            switch entry.kind {
            case .separator:
                menu.addItem(.separator())
            case .header:
                let item = NSMenuItem(title: entry.title, action: nil, keyEquivalent: "")
                item.attributedTitle = NSAttributedString(
                    string: entry.title,
                    attributes: [.font: NSFont.menuFont(ofSize: 0).bold()])
                item.isEnabled = false
                item.setAccessibilityLabel(entry.accessibility)
                menu.addItem(item)
            case .info:
                let item = NSMenuItem(title: entry.title, action: nil, keyEquivalent: "")
                item.isEnabled = false
                menu.addItem(item)
            case .command:
                let item = NSMenuItem(title: entry.title, action: selector, keyEquivalent: "")
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
