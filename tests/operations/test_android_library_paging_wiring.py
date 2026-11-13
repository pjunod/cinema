"""Bridge the local-loader regressions to the installed production grid."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


class AndroidLibraryPagingWiringCase(unittest.TestCase):
    def test_view_model_route_uses_the_same_grid_and_watch_filter_policy(self):
        screen = (ROOT / "clients/android/app/src/main/java/tv/plurx/app/ui/LibraryScreen.kt").read_text()
        public = screen.split("fun LibraryScreen(", 1)[1].split("internal fun LibraryScreen(", 1)[0]
        body = screen.split("internal fun LibraryScreen(", 1)[1]
        # The presence filter (on disk, offloaded, everything) is part of the
        # pager's identity, so the public route rebuilds it per filter.
        self.assertIn("remember(vm, libraryIds, presence) { { order: String -> vm.libraryPager(libraryIds, order, presence) } }", public)
        self.assertIn("libraryIds, title, preferences, kind, pagerFactory, onOpenItem, onBack, vm::setLibraryPresentation,\n        presence = presence, onPresenceChange = vm::setLibraryPresence,", public)
        # The ordinary Grid and the expanded single group share the same
        # route, pager, cards, and stable item IDs.
        self.assertEqual(screen.count("LazyVerticalGrid("), 2)
        self.assertIn("rowItems(groups, key = { it.key })", body)
        self.assertIn("pager.setDriveToCompletion(rows || filter != WatchFilter.Everything || query.isNotBlank())", body)
        self.assertIn("RequestInitialFocus(backFocus)", body)
        self.assertIn("items(shown, key = { it.id })", body)
        pager = (ROOT / "clients/android/app/src/main/java/tv/plurx/app/ui/LibraryPager.kt").read_text()
        self.assertIn("fun setWatchFilter(filter: WatchFilter) = setDriveToCompletion(filter != WatchFilter.Everything)", pager)


if __name__ == "__main__":
    unittest.main()
