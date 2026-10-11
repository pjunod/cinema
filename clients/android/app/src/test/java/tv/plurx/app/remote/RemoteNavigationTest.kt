package tv.plurx.app.remote

import androidx.compose.ui.geometry.Rect
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.*
import org.junit.Test

class RemoteNavigationTest {
    private fun action(type: String, direction: String? = null) = RemoteAction(type, buildJsonObject { direction?.let { put("direction", it) } })
    @Test fun lazyFocusRequestCannotSelectUntilRealizedNativeAcknowledgement() {
        val navigation = RemoteNavigationCoordinator()
        navigation.enter("library", "library") { true }
        navigation.viewport = Rect(0f, 0f, 500f, 500f)
        navigation.setOrder("library", listOf("first", "lazy"))
        var applied = 0
        navigation.register("library", "first", RemoteNavigationCoordinator.Entry("one", "First", Rect(0f, 0f, 100f, 100f), {}) { applied++; RemoteOutcome.Applied })
        navigation.nativeFocus("library", "first", "one", true)
        assertEquals(RemoteOutcome.Applied, navigation.dispatch(action("navigate", "right"), navigation.context))
        assertEquals("lazy", navigation.requestedFocus)
        assertEquals(RemoteOutcome.Unsupported, navigation.dispatch(action("select"), navigation.context))
        navigation.register("library", "lazy", RemoteNavigationCoordinator.Entry("two", "Lazy", Rect(600f, 0f, 700f, 100f), {}) { applied++; RemoteOutcome.Applied })
        navigation.nativeFocus("library", "lazy", "two", true)
        assertEquals(RemoteOutcome.Unsupported, navigation.dispatch(action("select"), navigation.context))
        navigation.geometry("library", "lazy", "two", Rect(100f, 0f, 200f, 100f))
        assertEquals(RemoteOutcome.Applied, navigation.dispatch(action("select"), navigation.context))
        assertEquals(1, applied)
    }
    @Test fun unknownModalAndRetiredRouteFenceCapturedActionsAndSensitiveState() {
        val navigation = RemoteNavigationCoordinator()
        navigation.enter("home", "home") { true }; navigation.setOrder("home", listOf("button"))
        val captured = navigation.context
        navigation.restrict("settings", true)
        assertEquals(RemoteOutcome.Restricted, navigation.dispatch(action("navigate", "down"), captured))
        assertTrue(navigation.snapshot().capabilities.isEmpty()); assertNull(navigation.snapshot().label)
        navigation.unrestrict("settings")
        assertEquals(RemoteOutcome.StaleContext, navigation.dispatch(action("back"), captured))
        navigation.releaseScope("home"); navigation.resetIdentity()
        assertTrue(navigation.snapshot().blocked); assertNull(navigation.onBack)
    }
    @Test fun ownedMenuBackRestoresExactOpenerOrderAfterBackgroundRefresh() {
        val navigation = RemoteNavigationCoordinator()
        navigation.enter("library", "library") { true }; navigation.setOrder("library", listOf("sort", "item1"))
        var closed = 0
        assertTrue(navigation.enterMenu("choices", listOf("option")) { closed++ })
        navigation.setOrder("library", listOf("sort", "item1", "item2"))
        assertEquals(RemoteOutcome.Applied, navigation.dispatch(action("back"), navigation.context))
        assertEquals(1, closed); assertEquals("library", navigation.route)
        assertEquals(RemoteOutcome.Applied, navigation.dispatch(action("navigate", "down"), navigation.context))
        assertEquals("sort", navigation.requestedFocus)
    }
    @Test fun detailPlayOwnerCannotSurviveHomeOrOldDisposalClearNewDetail() {
        val navigation = RemoteNavigationCoordinator()
        navigation.enter("old-detail", "details") { true }
        var oldApplied = 0; var newApplied = 0
        navigation.registerPlayItem("old-detail", "old") { oldApplied++; RemoteOutcome.Applied }
        navigation.enter("home", "home") { true }
        assertFalse("play_item" in navigation.snapshot().capabilities)
        assertEquals(RemoteOutcome.Unsupported, navigation.dispatch(RemoteAction("play_item", buildJsonObject { put("item_id", 1) }), navigation.context))
        navigation.enter("new-detail", "details") { true }
        navigation.registerPlayItem("new-detail", "new") { newApplied++; RemoteOutcome.Applied }
        navigation.unregisterPlayItem("old-detail", "old")
        navigation.registerPlayItem("old-detail", "old") { oldApplied++; RemoteOutcome.Applied }
        assertEquals(RemoteOutcome.Applied, navigation.dispatch(RemoteAction("play_item", buildJsonObject { put("item_id", 2) }), navigation.context))
        assertEquals(0, oldApplied); assertEquals(1, newApplied)
    }

    @Test fun onlyExactOwnedRoutesCanAdvertiseSemanticControls() {
        assertEquals("playback", RemoteRoutes.category("live-tv?channel={channel}"))
        assertEquals("library", RemoteRoutes.category("library/{ids}/{name}"))
        assertEquals("library", RemoteRoutes.category("shared-libraries"))
        listOf("live-tv-settings", "live-tv-admin", "library/admin", "player/settings", "sharing-settings", "shared-libraries/admin", "remote-devices").forEach { route -> assertEquals("restricted", RemoteRoutes.category(route)) }
    }

    @Test fun programmeDetailsOwnsOnlyWatchCloseAndRestoresExactGuideCell() {
        val navigation = RemoteNavigationCoordinator()
        navigation.enter("live", "playback") { true }
        navigation.setOrder("live", listOf("guide:cell"))
        navigation.register("live", "guide:cell", RemoteNavigationCoordinator.Entry("actual-cell", "Programme", Rect(0f, 0f, 40f, 40f), {}) { RemoteOutcome.Applied })
        navigation.nativeFocus("live", "guide:cell", "actual-cell", true)
        var dismissed = 0
        assertTrue(navigation.enterMenu("programme", listOf("programme:close", "programme:watch"), RemotePresentationKind.ProgrammeDetails) { dismissed++ })
        assertEquals("details", navigation.snapshot().route)
        assertEquals("programme:close", navigation.requestedFocus)
        val old = navigation.context
        navigation.updateMenu("programme", listOf("programme:close")) // airing/lineup eligibility changed
        assertEquals(RemoteOutcome.StaleContext, navigation.dispatch(action("select"), old))
        assertEquals(RemoteOutcome.Unsupported, navigation.dispatch(action("select"), navigation.context))
        assertEquals(RemoteOutcome.Applied, navigation.dispatch(action("back"), navigation.context))
        assertEquals(1, dismissed)
        assertEquals("guide:cell", navigation.requestedFocus)
    }
    @Test fun sharedChildExitRestoresExactOpenerAndRetiredDisposalCannotResetParent() {
        val navigation = RemoteNavigationCoordinator()
        navigation.enter("shared-detail", "details") { true }
        navigation.setOrder("shared-detail", listOf("shared:back", "shared-file:authorized"))
        navigation.enter("shared-preplay", "details") { true }
        navigation.enter("shared-detail", "details") { true }
        navigation.restoreFocus("shared-detail", "shared-file:authorized")
        assertEquals("shared-file:authorized", navigation.requestedFocus)
        val expected = navigation.context
        navigation.releaseScope("shared-preplay")
        navigation.restoreFocus("shared-preplay", "shared:back")
        assertEquals(expected, navigation.context)
        assertEquals("shared-detail", navigation.scope)
        assertEquals(RemoteOutcome.Unsupported, navigation.dispatch(action("select"), navigation.context))
    }
    @Test fun expandedLibraryOwnsRealizationAndBackRestoresExactOpener() {
        val navigation = RemoteNavigationCoordinator()
        navigation.enter("library", "library") { true }
        navigation.setOrder("library", listOf("group:all", "other"))
        navigation.register("library", "group:all", RemoteNavigationCoordinator.Entry("opener", "View all", Rect(0f, 0f, 40f, 40f), {}) { RemoteOutcome.Applied })
        navigation.nativeFocus("library", "group:all", "opener", true)
        var backgroundScroll = 0
        var groupScroll: String? = null
        var closed = 0
        navigation.realize = { backgroundScroll++ }
        assertTrue(navigation.enterMenu("expanded", listOf("expanded:close", "expanded:lazy"), RemotePresentationKind.LibraryGroup, realize = { groupScroll = it }) { closed++ })
        assertEquals("library", navigation.snapshot().route)
        assertEquals(RemoteOutcome.Applied, navigation.dispatch(action("navigate", "down"), navigation.context))
        assertEquals("expanded:lazy", groupScroll)
        assertEquals(0, backgroundScroll)
        assertEquals(RemoteOutcome.Unsupported, navigation.dispatch(action("select"), navigation.context))
        val beforeReplace = navigation.context
        navigation.updateMenu("expanded", listOf("expanded:close", "replacement"))
        assertEquals(RemoteOutcome.StaleContext, navigation.dispatch(action("select"), beforeReplace))
        assertEquals(RemoteOutcome.Applied, navigation.dispatch(action("back"), navigation.context))
        assertEquals("group:all", navigation.requestedFocus)
        assertEquals(1, closed)
        assertFalse(navigation.menuOwned("expanded"))
    }
    @Test fun retiredExpandedDialogCannotCloseReplacementPresentation() {
        val navigation = RemoteNavigationCoordinator()
        navigation.enter("library", "library") { true }
        navigation.setOrder("library", listOf("opener"))
        assertTrue(navigation.enterMenu("first", listOf("first:close"), RemotePresentationKind.LibraryGroup) {})
        navigation.closeMenu("first")
        assertTrue(navigation.enterMenu("replacement", listOf("replacement:close")) {})
        val current = navigation.context
        navigation.closeMenu("first")
        navigation.updateMenu("first", listOf("stale"))
        assertEquals(current, navigation.context)
        assertTrue(navigation.menuOwned("replacement"))
    }

}
