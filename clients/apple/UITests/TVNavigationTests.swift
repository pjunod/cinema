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
        remote.press(.right)
        let sort = app.buttons["library:sort"]
        assertFocus(sort, in: app, context: "Small library Sort")
        remote.press(.select)
        assertFocus(app.buttons["choice:title"], in: app, context: "Small library selected Title sort")
        remote.press(.menu)
        assertFocus(sort, in: app, context: "Small library Sort Back")
        remote.press(.right)
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
            remote.press(.down)
            assertFocus(app.buttons["item:1"], in: app, context: "Single movie content return")
            remote.press(.menu)
            assertFocus(view, in: app, context: "Single movie Back reaches the toolbar")
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

    func testRowsKeepWholeCardsVisibleAtMediumSize() throws { try wholeRowJourney(posterSize: "m") }
    func testRowsKeepWholeCardsVisibleAtExtraLargeSize() throws { try wholeRowJourney(posterSize: "xl") }

    func testShowRowsKeepWholeCardsVisibleAtExtraLargeSize() throws {
        let server = try TVNavigationServer(includeShows: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin, presentation: "rows", collectionID: "show", posterSize: "xl")
        defer { app.terminate() }
        let card = app.buttons["item:10001"]
        remote.press(.down)
        assertFocus(card, in: app, context: "Whole show card")
        assertWholeCardVisible(card, in: app)
    }

    private func wholeRowJourney(posterSize: String) throws {
        let server = try TVNavigationServer(itemCount: 416)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin, presentation: "rows", posterSize: posterSize)
        defer { app.terminate() }
        let jump = app.buttons["library:jump"]
        reachForSetup(jump, in: app, limit: 8)
        assertFocus(jump, in: app, context: "Whole row jump opener")
        remote.press(.select)
        assertFocus(app.buttons["choice:A"], in: app, context: "Whole row initial group")
        for _ in 0..<12 { remote.press(.down) }
        assertFocus(app.buttons["choice:M"], in: app, context: "Whole row deep group")
        remote.press(.select)
        let first = app.buttons["item:193"]
        assertFocus(first, in: app, context: "Whole row first title")
        assertWholeCardVisible(first, in: app)
        for id in 194...200 {
            remote.press(.right)
            let card = app.buttons["item:\(id)"]
            assertFocus(card, in: app, context: "Whole row horizontal move \(id)")
            assertWholeCardVisible(card, in: app)
        }
        remote.press(.menu)
        assertFocus(app.buttons["library:view"], in: app, context: "Whole row Back reaches tools")
        remote.press(.down)
        let bookmark = app.buttons["item:200"]
        assertFocus(bookmark, in: app, context: "Whole row tools return")
        assertWholeCardVisible(bookmark, in: app)
    }

    private func assertWholeCardVisible(_ card: XCUIElement, in app: XCUIApplication,
                                       file: StaticString = #filePath, line: UInt = #line) {
        let results = app.scrollViews["library-results"]
        let visible = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            let viewport = results.frame
            let frame = card.frame
            return frame.height > 0 && frame.minY >= viewport.minY + 4
                && frame.maxY <= viewport.maxY - 4
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [visible], timeout: 5), .completed,
                       "The entire focused poster, title and metadata must fit below the toolbar: card=\(card.frame), viewport=\(results.frame)",
                       file: file, line: line)
    }

    private func toolsJourney(itemCount: Int, presentation: String) throws {
        let server = try TVNavigationServer(itemCount: itemCount)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin, presentation: presentation)
        defer { app.terminate() }
        var previousGroup = (letter: "A", index: 0)
        for (letter, groupIndex) in [("A", 0), ("M", 12), ("Z", 25), ("M", 12)] {
            let jump = app.buttons["library:jump"]
            // From content, one Back reaches the toolbar.
            if app.buttons.matching(NSPredicate(format: "hasFocus == true AND identifier BEGINSWITH %@", "item:")).firstMatch.exists {
                remote.press(.menu)
            }
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
            // Leave the jump target so the bookmark is a title the viewer chose.
            remote.press(.right)
            let focusedItem = app.buttons.matching(NSPredicate(format: "hasFocus == true AND identifier BEGINSWITH %@", "item:")).firstMatch
            XCTAssertTrue(focusedItem.waitForExistence(timeout: 5), app.debugDescription)
            let bookmarkID = focusedItem.identifier
            remote.press(.menu)
            assertFocus(tools[0], in: app, context: "One Back from deep content reaches the toolbar")
            for tool in tools.dropFirst() {
                remote.press(.right)
                assertFocus(tool, in: app, context: "Right reaches " + tool.identifier)
            }
            for tool in tools.dropLast().reversed() {
                remote.press(.left)
                assertFocus(tool, in: app, context: "Left reaches " + tool.identifier)
            }
            remote.press(.down)
            let bookmark = app.buttons[bookmarkID]
            assertFocus(bookmark, in: app, context: "Down restores exact content entry bookmark")
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
        let libraries = app.viewerTab("Libraries")
        reachForSetup(libraries, in: app, limit: 4)
        assertFocus(libraries, in: app, context: "Libraries root tab")
        remote.press(.left)
        assertFocus(app.viewerTab("Home"), in: app, context: "Home root tab")
        for label in ["Libraries", "Live TV", "Recordings", "Channels", "Search", "Settings"] {
            remote.press(.right)
            let tab = app.viewerTab(label)
            assertFocus(tab, in: app, context: label + " root tab")
            XCTAssertTrue(app.descendants(matching: .any)["root-screen-" + label].waitForExistence(timeout: 5),
                          label + " must display its route without Select: " + app.debugDescription)
        }
        for label in ["Search", "Channels", "Recordings", "Live TV", "Libraries", "Home"] {
            remote.press(.left)
            assertFocus(app.viewerTab(label), in: app, context: label + " reverse root tab")
            XCTAssertTrue(app.descendants(matching: .any)["root-screen-" + label].waitForExistence(timeout: 5),
                          label + " must display its route without Select: " + app.debugDescription)
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
        XCTAssertTrue(app.viewerTab("Libraries").waitForExistence(timeout: 5),
                      "Back must return to Libraries, not leave the app")
    }

}
#endif
