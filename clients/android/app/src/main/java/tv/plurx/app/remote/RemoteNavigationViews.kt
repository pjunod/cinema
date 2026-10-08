package tv.plurx.app.remote

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.platform.LocalView
import java.util.UUID

/** Only call sites with explicitly safe semantic actions attach this modifier. */
internal fun Modifier.remoteAction(key: String, label: String, enabled: Boolean = true, activate: () -> RemoteOutcome): Modifier = composed {
    val navigation = LocalRemoteNavigation.current
    val scope = LocalRemoteScope.current
    val identity = remember(scope, key) { UUID.randomUUID().toString() }
    val requester = remember(scope, key) { FocusRequester() }
    val view = LocalView.current
    var nativeFocused by remember { mutableStateOf(false) }
    val callback by rememberUpdatedState(activate)
    val currentLabel by rememberUpdatedState(label)
    DisposableEffect(navigation, scope, key, identity, enabled) {
        if (navigation != null && enabled) navigation.register(scope, key, RemoteNavigationCoordinator.Entry(identity, currentLabel, Rect.Zero,
            requestFocus = { runCatching { requester.requestFocus() } }, activate = { callback() }))
        onDispose { navigation?.unregister(scope, key, identity) }
    }
    SideEffect { navigation?.label(scope, key, identity, label) }
    LaunchedEffect(navigation?.requestedFocus, navigation?.focusRequestRevision, enabled) {
        if (enabled && navigation?.scope == scope && navigation.requestedFocus == key) {
            val ticket = navigation.focusRequestRevision
            fun currentRequest() = navigation.scope == scope && navigation.requestedFocus == key && navigation.focusRequestRevision == ticket
            withFrameNanos { }; if (!currentRequest()) return@LaunchedEffect
            runCatching { requester.requestFocus() }
            withFrameNanos { }; if (currentRequest() && nativeFocused && view.hasWindowFocus()) navigation.nativeFocus(scope, key, identity, true)
        }
    }
    this.pointerInput(navigation, scope, key) { awaitEachGesture { awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial); navigation?.physicalInput() } }
        .focusRequester(requester)
        .onGloballyPositioned { navigation?.geometry(scope, key, identity, it.boundsInWindow()) }
        .onFocusChanged { nativeFocused = it.isFocused; if (enabled) navigation?.nativeFocus(scope, key, identity, it.isFocused && view.hasWindowFocus()) }
        .onPreviewKeyEvent { navigation?.physicalInput(); false }
}
@Composable
internal fun RemoteRestricted(active: Boolean = true) {
    val navigation = LocalRemoteNavigation.current
    val token = remember { UUID.randomUUID().toString() }
    DisposableEffect(navigation, token, active) {
        navigation?.restrict(token, active)
        onDispose { navigation?.unrestrict(token) }
    }
}
@Composable
internal fun RemoteOrder(keys: List<String>, realize: ((String) -> Unit)? = null) {
    val navigation = LocalRemoteNavigation.current
    val scope = LocalRemoteScope.current
    SideEffect { navigation?.setOrder(scope, keys); if (navigation?.scope == scope) navigation.realize = realize }
}

@Composable
internal fun RemoteRealizer(realize: (String) -> Unit) {
    val navigation = LocalRemoteNavigation.current
    val scope = LocalRemoteScope.current
    val token = remember(scope) { UUID.randomUUID().toString() }
    val callback by rememberUpdatedState(realize)
    DisposableEffect(navigation, scope, token) {
        navigation?.registerRealizer(scope, token) { callback(it) }
        onDispose { navigation?.unregisterRealizer(scope, token) }
    }
}
internal val LocalRemoteItemPrefix = staticCompositionLocalOf { "item" }
