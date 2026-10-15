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

    func testEmptyLibraryKeepsToolsAndBackReachable() throws { try smallLibraryJourney(itemCount: 0) }
    func testSingleMovieLibraryKeepsToolsAndBackReachable() throws { try smallLibraryJourney(itemCount: 1) }

    private func smallLibraryJourney(itemCount: Int) throws {
        let server = try TVNavigationServer(itemCount: itemCount)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        let view = app.buttons["library:view"]
        assertFocus(view, in: app, context: "Small library View remains reachable")
        remote.press(.down)
        let sort = app.buttons["library:sort"]
        assertFocus(sort, in: app, context: "Small library Sort")
        remote.press(.select)
        assertFocus(app.buttons["choice:title"], in: app, context: "Small library selected Title sort")
        remote.press(.menu)
        assertFocus(sort, in: app, context: "Small library Sort Back")
        remote.press(.down)
        let filter = app.buttons["library:filter"]
        assertFocus(filter, in: app, context: "Small library Filter")
        remote.press(.select)
        assertFocus(app.buttons["choice:all"], in: app, context: "Small library selected Everything")
        remote.press(.menu)
        assertFocus(filter, in: app, context: "Small library Filter Back")
        if itemCount == 0 {
            XCTAssertFalse(app.buttons["library:jump"].isEnabled)
            XCTAssertTrue(app.staticTexts["This library is empty"].exists)
        } else {
            remote.press(.right)
            assertFocus(app.buttons["item:1"], in: app, context: "Single movie content return")
        }
        remote.press(.menu)
        XCTAssertTrue(app.buttons["library-open-category:movie"].waitForExistence(timeout: 5), app.debugDescription)
    }

    func testGridToolsAndJump416() throws { try toolsJourney(itemCount: 416, presentation: "grid") }
    func testGridToolsAndJump5000() throws { try toolsJourney(itemCount: 5000, presentation: "grid") }
    func testRowsViewAllOpensGroupAndBackRestoresHeader() throws {
        let server = try TVNavigationServer(itemCount: 416)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin, presentation: "rows")
        defer { app.terminate() }
        let first = app.buttons["item:1"]
        reachForSetup(first, in: app, limit: 6)
        assertFocus(first, in: app, context: "Rows first title before group header")
        remote.press(.up)
        let viewAll = app.buttons["View all A items"]
        assertFocus(viewAll, in: app, context: "Up from first title reaches readable View all")
        remote.press(.select)
        let groupTools = app.buttons["group:tools"]
        reachForSetup(groupTools, in: app, limit: 4)
        assertFocus(groupTools, in: app, context: "Expanded group has reachable tools")
        remote.press(.menu)
        assertFocus(viewAll, in: app, context: "Expanded group Back restores View all")
        let viewport = app.descendants(matching: .any)
            .matching(identifier: "library-results").firstMatch.frame
        XCTAssertLessThanOrEqual(viewAll.frame.maxX, viewport.maxX - 8,
                                 "The focused group action must retain clearance inside its viewport")
    }

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
            var bookmarkID = item.identifier
            for _ in 0..<6 {
                if tools.contains(where: { $0.hasFocus }) { break }
                let focusedItem = app.buttons.matching(NSPredicate(format: "hasFocus == true AND identifier BEGINSWITH %@", "item:")).firstMatch
                if focusedItem.exists { bookmarkID = focusedItem.identifier }
                remote.press(.left)
            }
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
            let bookmark = app.buttons[bookmarkID]
            assertFocus(bookmark, in: app, context: "Right restores exact content entry bookmark")
            let bookmarkLetter = String(bookmark.label.prefix(1))
            previousGroup = (bookmarkLetter, Int(bookmarkLetter.unicodeScalars.first!.value) - 65)
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
        remote.press(.up)
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
            XCTAssertTrue(app.descendants(matching: .any)["root-screen-" + label].waitForExistence(timeout: 5),
                          label + " must display its route without Select: " + app.debugDescription)
        }
        for label in ["Search", "Channels", "Recordings", "Live TV", "Libraries", "Home"] {
            remote.press(.left)
            assertFocus(app.tabBars.buttons[label], in: app, context: label + " reverse root tab")
            XCTAssertTrue(app.descendants(matching: .any)["root-screen-" + label].exists, app.debugDescription)
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
        let moves: [(XCUIRemote.Button, String)] = [(.up, "Rows"), (.down, "Grid"), (.down, "Close"),
                                                     (.up, "Grid"), (.left, "Grid"), (.right, "Grid")]
        for (direction, label) in moves {
            remote.press(direction)
            assertFocus(app.buttons[label], in: app, context: "Modal direction reaches " + label)
        }
        remote.press(.menu)
        assertFocus(view, in: app, context: "Back restores View")
        XCTAssertFalse(grid.exists, "Back must dismiss only the choice panel")
        remote.press(.select)
        assertFocus(grid, in: app, context: "reopened selection")
        remote.press(.menu)
        assertFocus(view, in: app, context: "second Back restores View")
        remote.press(.select)
        assertFocus(grid, in: app, context: "Grid before Rows selection")
        remote.press(.up)
        let rows = app.buttons["Rows"]
        assertFocus(rows, in: app, context: "Rows before Select")
        remote.press(.select)
        assertFocus(view, in: app, context: "Select dismisses once and restores View")
        remote.press(.select)
        assertFocus(rows, in: app, context: "Selected Rows on next entry")
        XCTAssertTrue(rows.isSelected, "Rows selection must persist after a single Select")
        remote.press(.menu)
        assertFocus(view, in: app, context: "Rows Back restores View")
        remote.press(.menu)
        XCTAssertTrue(app.tabBars.buttons["Libraries"].waitForExistence(timeout: 5),
                      "Back must return to Libraries, not leave the app")
    }

}
#endif
