package tv.plurx.app

import androidx.navigation.NavBackStackEntry
import androidx.navigation.NavController

internal fun NavController.popBackStackFrom(entry: NavBackStackEntry): Boolean {
    // An outgoing screen can still receive taps during its exit animation.
    // Bind Back to that exact entry, not its route (detail pages share a route),
    // so a repeated or delayed callback cannot pop the page underneath it.
    if (currentBackStackEntry?.id != entry.id || previousBackStackEntry == null) return false
    return popBackStack()
}
