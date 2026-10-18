package tv.plurx.app.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewmodel.compose.viewModel

/** Non-serializable presentation state belongs to the NavBackStackEntry, never the shell.
 * Navigation retains that entry's ViewModelStore while a primary branch is saved.
 * Removing the source/profile/entry clears this holder with the store. Controllers,
 * coroutine scopes and native views must continue to use their existing owners.
 */
internal class EntryPresentationState : ViewModel() {
    private val values = object : LinkedHashMap<List<Any?>, Any?>(32, .75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<List<Any?>, Any?>?): Boolean = size > 128
    }
    @Suppress("UNCHECKED_CAST")
    fun <T> value(keys: List<Any?>, factory: () -> T): T =
        if (values.containsKey(keys)) values[keys] as T else factory().also { values[keys] = it }
    override fun onCleared() { values.clear() }
}

@Composable
internal fun <T> rememberEntryPresentation(name: String, vararg keys: Any?, factory: () -> T): T {
    val owner: EntryPresentationState = viewModel()
    return remember(owner, name, *keys) { owner.value(listOf(name) + keys.toList(), factory) }
}
