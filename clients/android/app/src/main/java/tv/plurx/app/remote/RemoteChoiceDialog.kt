package tv.plurx.app.remote

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogWindowProvider
import tv.plurx.app.ui.components.TvTextButton
import tv.plurx.app.ui.components.RequestInitialFocus
import java.util.UUID

internal data class RemoteChoice(val key: String, val label: String, val selected: Boolean = false, val activate: () -> RemoteOutcome)
@Composable
internal fun RemoteChoiceDialog(title: String, choices: List<RemoteChoice>, onClose: () -> Unit) {
    val navigation = LocalRemoteNavigation.current
    val scope = LocalRemoteScope.current
    val token = remember(scope) { UUID.randomUUID().toString() }
    val choiceKeys = choices.take(128).map { "choice-menu:" + token + ":" + it.key }
    val closeKey = "choice-menu:" + token + ":close"
    val keys = choiceKeys + closeKey
    val close by rememberUpdatedState(onClose)
    var owned by remember { mutableStateOf(false) }
    DisposableEffect(navigation, scope, token) {
        owned = navigation?.enterMenu(token, keys) { close() } == true
        onDispose { navigation?.closeMenu(token) }
    }
    SideEffect { if (owned) navigation?.updateMenu(token, keys) }
    Dialog(onDismissRequest = { navigation?.closeMenu(token); onClose() }) {
        if (!owned) RemoteRestricted()
        val view = LocalView.current
        val windowView = (view.parent as? DialogWindowProvider)?.window?.decorView ?: view
        DisposableEffect(navigation, windowView, owned) {
            if (owned) navigation?.ownedWindow(token, windowView)
            onDispose { navigation?.ownedWindow(token, null) }
        }
        val first = remember { FocusRequester() }
        RequestInitialFocus(first, enabled = choices.isNotEmpty(), reinforce = false)
        Surface(shape = MaterialTheme.shapes.large) {
            Column(Modifier.widthIn(max = 640.dp).heightIn(max = 600.dp).verticalScroll(rememberScrollState()).padding(24.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(title, style = MaterialTheme.typography.titleLarge)
                choices.take(128).forEachIndexed { index, choice ->
                    val key = keys[index]
                    TvTextButton(onClick = { navigation?.physicalInput(); choice.activate(); navigation?.closeMenu(token); onClose() },
                        modifier = Modifier.fillMaxWidth().semantics { selected = choice.selected }
                            .then(if (owned) Modifier.remoteAction(key, choice.label) { val result = choice.activate(); if (result == RemoteOutcome.Applied) { navigation?.closeMenu(token); close() }; result } else Modifier)
                            .then(if (index == 0) Modifier.focusRequester(first) else Modifier)) { Text(choice.label) }
                }
                TextButton(modifier = if (owned) Modifier.remoteAction(closeKey, "Close") { if (navigation?.menuOwned(token) != true) RemoteOutcome.StaleContext else { navigation.closeMenu(token); close(); RemoteOutcome.Applied } } else Modifier, onClick = { navigation?.physicalInput(); navigation?.closeMenu(token); onClose() }) { Text("Close") }
            }
        }
    }
}
