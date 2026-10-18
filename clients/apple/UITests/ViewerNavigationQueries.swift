import XCTest

enum ViewerLayoutChoice: String { case standard, meridian }
extension XCUIApplication {
    /// Root destinations have a semantic identifier under either chrome. This
    /// query remains valid when a rail replaces the platform's tab-bar widget.
    func viewerTab(_ title: String) -> XCUIElement {
        let meridian = buttons["viewer-tab-" + title]
        if meridian.exists { return meridian }
        return tabBars.buttons.matching(NSPredicate(format: "label == %@ OR label BEGINSWITH %@", title, title + " · ")).firstMatch
    }
}
