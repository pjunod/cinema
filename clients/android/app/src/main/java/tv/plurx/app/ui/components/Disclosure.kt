package tv.plurx.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.unit.dp

/**
 * A row that expands to its detail. Collapsed until the viewer opens it, so a
 * setting reads as its name and switch and the explanation stays out of the way.
 */
@Composable
fun Disclosure(title: String = "Details", content: @Composable () -> Unit) {
    var expanded by rememberSaveable(title) { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        TvTextButton(onClick = { expanded = !expanded }) { Text(if (expanded) "Hide ${title.lowercase()}" else title) }
        if (expanded) content()
    }
}
