#if os(tvOS)
import XCTest

/// Simulator journeys through the shipping authentication, tab shell and routes.
/// Setup may enter text; the measured navigation uses only the native remote.
@MainActor
final class TVNavigationTests: TVNavigationTestCase {
    func testViewModalOwnsFocusAndBackRestoresOpener416() throws {
        try viewModalJourney(itemCount: 416)
    }

    func testViewModalOwnsFocusAndBackRestoresOpener5000() throws {
        try viewModalJourney(itemCount: 5000)
    }

    func testGridToolsAndJump416() throws { try toolsJourney(itemCount: 416, presentation: "grid") }
    func testGridToolsAndJump5000() throws { try toolsJourney(itemCount: 5000, presentation: "grid") }
    func testRowsToolsAndJump416() throws { try toolsJourney(itemCount: 416, presentation: "rows") }
    func testRowsToolsAndJump5000() throws { try toolsJourney(itemCount: 5000, presentation: "rows") }

    private func toolsJourney(itemCount: Int, presentation: String) throws {
        let server = try TVNavigationServer(itemCount: itemCount)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin, presentation: presentation)
        defer { app.terminate() }
        var previousGroup = (letter: "A", index: 0)
        for (letter, groupIndex) in [("A", 0), ("M", 12), ("Z", 25), ("M", 12)] {
            let jump = app.buttons["library:jump"]
            reachForSetup(jump, in: app, limit: 8)
            assertFocus(jump, in: app, context: "Jump opener")
            remote.press(.select)
            let entry = app.buttons["choice:" + previousGroup.letter]
            XCTAssertTrue(entry.waitForExistence(timeout: 30), app.debugDescription)
            assertFocus(entry, in: app, context: "Jump retains selected group")
            let steps = groupIndex - previousGroup.index
            for _ in 0..<abs(steps) { remote.press(steps > 0 ? .down : .up) }
            let choice = app.buttons["choice:" + letter]
            assertFocus(choice, in: app, context: "Jump \(letter) choice")
            remote.press(.select)
            let index = (groupIndex * itemCount + 25) / 26
            let item = app.buttons["item:\(index + 1)"]
            assertFocus(item, in: app, context: "Jump \(letter) focuses first item")
            let tools = ["view", "sort", "filter", "jump"].map { app.buttons["library:" + $0] }
            for _ in 0..<6 where !tools.contains(where: { $0.hasFocus }) { remote.press(.left) }
            guard let reachedTool = tools.first(where: { $0.hasFocus }) else {
                XCTFail("Tools must receive native focus within six Left presses: \(app.debugDescription)")
                return
            }
            assertFocus(reachedTool, in: app, context: "Tools within six Left presses")
            let view = tools[0]
            for _ in 0..<3 where !view.hasFocus { remote.press(.up) }
            assertFocus(view, in: app, context: "View within three Up presses inside tools")
            for tool in tools.dropFirst() {
                remote.press(.down)
                assertFocus(tool, in: app, context: "Down reaches " + tool.identifier)
            }
            for tool in tools.dropLast().reversed() {
                remote.press(.up)
                assertFocus(tool, in: app, context: "Up reaches " + tool.identifier)
            }
            remote.press(.right)
            assertFocus(item, in: app, context: "Right restores \(letter) bookmark")
            previousGroup = (letter, groupIndex)
        }
    }

    func testSortFilterAndGroupingOwnFocusAndRestoreOpeners() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        for (openerLabel, selectedLabel) in [("Sort", "Title (A–Z)"), ("Filter", "Everything")] {
            let opener = app.buttons["library:" + openerLabel.lowercased()]
            reachForSetup(opener, in: app, limit: 8)
            assertFocus(opener, in: app, context: openerLabel + " opener")
            remote.press(.select)
            assertFocus(app.buttons[selectedLabel], in: app, context: openerLabel + " selected choice")
            remote.press(.menu)
            assertFocus(opener, in: app, context: openerLabel + " Back restores opener")
        }
        remote.press(.menu)
        let grouping = app.buttons["Group by"]
        XCTAssertTrue(grouping.waitForExistence(timeout: 5), app.debugDescription)
        reachForSetup(grouping, in: app, limit: 8)
        assertFocus(grouping, in: app, context: "Group by opener")
        remote.press(.select)
        assertFocus(app.buttons["Category"], in: app, context: "Group by selected category")
        remote.press(.menu)
        assertFocus(grouping, in: app, context: "Group by Back restores opener")
    }

    func testEveryEnabledRootTabReceivesNativeFocus() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let libraries = app.tabBars.buttons["Libraries"]
        reachForSetup(libraries, in: app, limit: 4)
        assertFocus(libraries, in: app, context: "Libraries root tab")
        remote.press(.left)
        assertFocus(app.tabBars.buttons["Home"], in: app, context: "Home root tab")
        for label in ["Libraries", "Live TV", "Recordings", "Channels", "Search", "Settings"] {
            remote.press(.right)
            let tab = app.tabBars.buttons[label]
            assertFocus(tab, in: app, context: label + " root tab")
            remote.press(.select)
            XCTAssertTrue(tab.isSelected, label + " must remain the selected route")
        }
        for label in ["Search", "Channels", "Recordings", "Live TV", "Libraries", "Home"] {
            remote.press(.left)
            assertFocus(app.tabBars.buttons[label], in: app, context: label + " reverse root tab")
        }
    }

    private func viewModalJourney(itemCount: Int) throws {
        let server = try TVNavigationServer(itemCount: itemCount)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        let view = app.buttons["library:view"]
        reachForSetup(view, in: app, limit: 8)
        assertFocus(view, in: app, context: "View opener")
        remote.press(.select)
        let grid = app.buttons["Grid"]
        XCTAssertTrue(grid.waitForExistence(timeout: 5), app.debugDescription)
        assertFocus(grid, in: app, context: "selected Grid on modal entry")
        // Every direction stays within the panel. Mere panel existence is not
        // acceptance: the historical failure moves onto a movie underneath it.
        for direction: XCUIRemote.Button in [.down, .up, .left, .right] {
            remote.press(direction)
            let choices = [app.buttons["Rows"], grid, app.buttons["Close"]]
            XCTAssertTrue(choices.contains { $0.exists && $0.hasFocus },
                          "Modal lost native focus: \(app.debugDescription)")
        }
        remote.press(.menu)
        assertFocus(view, in: app, context: "Back restores View")
        XCTAssertFalse(grid.exists, "Back must dismiss only the choice panel")
        remote.press(.select)
        assertFocus(grid, in: app, context: "reopened selection")
        remote.press(.menu)
        assertFocus(view, in: app, context: "second Back restores View")
        remote.press(.menu)
        XCTAssertTrue(app.tabBars.buttons["Libraries"].waitForExistence(timeout: 5),
                      "Back must return to Libraries, not leave the app")
    }

}
#endif
