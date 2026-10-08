package tv.plurx.app.remote

import androidx.compose.runtime.*
import androidx.compose.ui.geometry.Rect
import java.util.UUID
import android.view.View
import java.lang.ref.WeakReference

internal class RemoteNavigationCoordinator {
    data class Entry(val identity: String, var label: String, var frame: Rect, val requestFocus: () -> Unit, val activate: () -> RemoteOutcome)
    data class Context(val epoch: String, val revision: Long, val focusRevision: Long)
    data class Snapshot(val context: Context, val route: String, val blocked: Boolean, val label: String?, val textNonce: String?, val capabilities: List<String>)
    var scope by mutableStateOf("restricted"); private set
    var route by mutableStateOf("restricted"); private set
    var requestedFocus by mutableStateOf<String?>(null); private set
    var contextRevision by mutableLongStateOf(1); private set
    var focusRevision by mutableLongStateOf(1); private set
    var focusRequestRevision by mutableLongStateOf(1); private set
    private data class Menu(val token: String, val scope: String, val route: String, val keys: List<String>, val opener: String?, val dismiss: () -> Unit)
    private var menu: Menu? = null
    private val ownedWindows = mutableMapOf<String, Pair<String, WeakReference<View>>>()
    private var epoch = UUID.randomUUID().toString()
    private var focusedKey: String? = null
    private var focusedIdentity: String? = null
    private val entries = linkedMapOf<String, LinkedHashMap<String, Entry>>()
    private val orders = linkedMapOf<String, List<String>>()
    private val restrictions = mutableStateMapOf<String, Boolean>()
    private var search: Triple<String, String, (String) -> Unit>? = null
    var viewport: Rect = Rect.Zero
    private val realizers = linkedMapOf<String, LinkedHashMap<String, (String) -> Unit>>()
    var onPhysicalInput: (() -> Unit)? = null
    var onBack: (() -> Boolean)? = null
    var onHome: (() -> Unit)? = null
    private data class PlayOwner(val scope: String, val token: String, val play: (Long) -> RemoteOutcome)
    private var playOwner: PlayOwner? = null
    val onPlayItem get() = playOwner?.takeIf { it.scope == scope }?.play
    fun registerPlayItem(value: String, token: String, play: (Long) -> RemoteOutcome) { if (value == scope) playOwner = PlayOwner(value, token, play) }
    fun unregisterPlayItem(value: String, token: String) { if (playOwner?.scope == value && playOwner?.token == token) playOwner = null }
    var realize: ((String) -> Unit)? = null
    val context get() = Context(epoch, contextRevision, focusRevision)
    val blocked get() = route !in setOf("home", "library", "details", "search", "playback", "tracks") || restrictions.values.any { it }
    fun windowContextChanged() {
        contextRevision++; focusRevision++; focusedIdentity = null
        if (requestedFocus == null && focusedKey in orders[scope].orEmpty()) requestedFocus = focusedKey
        focusRequestRevision++
    }
    fun enterMenu(token: String, keys: List<String>, dismiss: () -> Unit): Boolean {
        if (blocked || menu != null) return false
        menu = Menu(token, scope, route, orders[scope].orEmpty(), focusedKey, dismiss)
        route = "tracks"; focusedKey = null; changedContext(); setOrder(scope, keys, ownedMenu = true)
        requestedFocus = keys.firstOrNull(); focusRequestRevision++
        return true
    }
    fun updateMenu(token: String, keys: List<String>) { if (menu?.token == token) setOrder(scope, keys, ownedMenu = true) }
    fun menuOwned(token: String) = menu?.token == token && menu?.scope == scope
    fun closeMenu(token: String) {
        val active = menu?.takeIf { it.token == token } ?: return
        menu = null; ownedWindows.remove(token)
        if (scope == active.scope) { route = active.route; setOrder(scope, active.keys); changedContext(); focusedKey = null; requestedFocus = active.opener; focusRequestRevision++ }
    }
    fun ownedWindow(token: String, view: View?) {
        val active = menu?.takeIf { it.token == token && it.scope == scope } ?: return
        if (view == null) ownedWindows.remove(token) else ownedWindows[token] = active.scope to WeakReference(view)
    }
    private fun visibleViewport(): Rect {
        val active = menu ?: return viewport
        val view = ownedWindows[active.token]?.takeIf { it.first == scope }?.second?.get() ?: return Rect.Zero
        return Rect(0f, 0f, view.width.toFloat(), view.height.toFloat())
    }
    val ownedChoicesReady get() = route == "tracks" && ownedWindowFocused()
    fun ownedWindowFocused(): Boolean {
        val active = menu ?: return false
        return ownedWindows[active.token]?.takeIf { it.first == scope }?.second?.get()?.hasWindowFocus() == true
    }
    fun enter(nextScope: String, category: String, back: () -> Boolean) {
        if (scope == nextScope && route == category) { onBack = back; return }
        menu?.dismiss?.invoke(); menu = null; ownedWindows.clear()
        scope = nextScope; route = category; onBack = back; playOwner = null; search = null; realize = null
        requestedFocus = null; focusedKey = null; focusedIdentity = null; changedContext()
    }
    fun resetIdentity() {
        entries.clear(); orders.clear(); realizers.clear(); restrictions.clear(); menu = null; ownedWindows.clear(); search = null; scope = "restricted"; route = "restricted"
        onBack = null; onHome = null; playOwner = null; realize = null; suspend()
    }
    fun suspend() { epoch = UUID.randomUUID().toString(); requestedFocus = null; focusedKey = null; focusedIdentity = null; changedContext() }
    fun changedContext() { contextRevision++; focusRevision++; focusRequestRevision++; requestedFocus = null; focusedIdentity = null }
    fun restrict(token: String, value: Boolean) { if (!value) { unrestrict(token); return }; if (restrictions[token] != value) { restrictions[token] = value; changedContext() } }
    fun unrestrict(token: String) { if (restrictions.remove(token) != null) changedContext() }
    fun releaseScope(value: String) { entries.remove(value); orders.remove(value); realizers.remove(value); if (scope == value) { search = null; focusedKey = null; focusedIdentity = null; requestedFocus = null; changedContext() } }
    fun setOrder(value: String, keys: List<String>, ownedMenu: Boolean = false) {
        require(keys.size <= 16384 && keys.distinct().size == keys.size)
        val active = menu
        if (active != null && active.scope == value && !ownedMenu) { menu = active.copy(keys = keys); return }
        if (value !in orders && orders.size >= 32) return
        if (orders[value] != keys) {
            orders[value] = keys
            if (scope == value) {
                contextRevision++; focusRevision++
                if (focusedKey !in keys) { focusedKey = null; focusedIdentity = null }
                if (requestedFocus !in keys) requestedFocus = null
            }
        }
    }
    fun registerRealizer(value: String, token: String, callback: (String) -> Unit) {
        if (value !in realizers && realizers.size >= 32) return
        val map = realizers.getOrPut(value) { linkedMapOf() }
        if (token in map || map.size < 64) map[token] = callback
    }
    fun unregisterRealizer(value: String, token: String) { realizers[value]?.remove(token) }
    fun register(value: String, key: String, entry: Entry) {
        if (value !in entries && entries.size >= 32) return
        val map = entries.getOrPut(value) { linkedMapOf() }
        if (key !in map && entries.values.sumOf { it.size } >= 512) return
        val old = map.put(key, entry)
        if (scope == value && focusedKey == key && old?.identity != entry.identity) { focusedIdentity = null; focusRevision++ }
    }
    fun unregister(value: String, key: String, identity: String) {
        val map = entries[value] ?: return
        if (map[key]?.identity != identity) return
        map.remove(key)
        if (scope == value && focusedKey == key) { focusedIdentity = null; focusedKey = null; focusRevision++ }
    }
    fun label(value: String, key: String, identity: String, label: String) { entries[value]?.get(key)?.takeIf { it.identity == identity }?.label = label }
    fun geometry(value: String, key: String, identity: String, frame: Rect) { entries[value]?.get(key)?.takeIf { it.identity == identity }?.frame = frame }
    fun nativeFocus(value: String, key: String, identity: String, focused: Boolean) {
        if (scope != value || key !in orders[value].orEmpty() || entries[value]?.get(key)?.identity != identity) return
        if (focused) {
            val requested = requestedFocus == key
            if (focusedKey != key || focusedIdentity != identity) { focusedKey = key; focusedIdentity = identity; focusRevision++ }
            requestedFocus = null
            if (!requested) physicalInput()
        } else if (focusedKey == key && focusedIdentity == identity) { focusedKey = null; focusedIdentity = null; focusRevision++ }
    }
    fun physicalInput() { requestedFocus = null; focusRevision++; focusRequestRevision++; onPhysicalInput?.invoke() }
    fun search(value: String, nonce: String, edit: (String) -> Unit) {
        if (search?.first != value || search?.second != nonce) changedContext()
        search = Triple(value, nonce, edit)
    }
    fun clearSearch(value: String, nonce: String) { if (search?.first == value && search?.second == nonce) { search = null; changedContext() } }
    fun snapshot(): Snapshot {
        if (blocked) return Snapshot(context, "restricted", true, null, null, emptyList())
        val label = focusedKey?.let { entries[scope]?.get(it)?.label }?.let { RemoteWire.safeLabel(it, fallback = "").ifEmpty { null } }
        return Snapshot(context, route, false, label, search?.takeIf { it.first == scope }?.second,
            buildList { if (!orders[scope].isNullOrEmpty()) add("navigate"); add("select"); add("back"); add("home"); if (search?.first == scope) add("text_replace"); if (onPlayItem != null) add("play_item") })
    }
    fun dispatch(action: RemoteAction, expected: Context): RemoteOutcome {
        if (blocked) return RemoteOutcome.Restricted
        if (expected.epoch != epoch || expected.revision != contextRevision) return RemoteOutcome.StaleContext
        when (action.type) {
            "select" -> {
                if (expected.focusRevision != focusRevision) return RemoteOutcome.StaleFocus
                val entry = focusedKey?.let { entries[scope]?.get(it) } ?: return RemoteOutcome.Unsupported
                if (requestedFocus != null || entry.identity != focusedIdentity || entry.frame.isEmpty || !entry.frame.overlaps(visibleViewport())) return RemoteOutcome.Unsupported
                focusRevision++; return entry.activate()
            }
            "navigate" -> {
                val keys = orders[scope].orEmpty(); if (keys.isEmpty()) return RemoteOutcome.Unsupported
                val key = requestedFocus ?: focusedKey
                val index = keys.indexOf(key)
                val direction = action.text("direction") ?: return RemoteOutcome.Invalid
                val spatial = key?.let { spatial(it, direction) }
                val next = spatial ?: keys[if (index < 0) 0 else (index + if (direction in setOf("up", "left")) -1 else 1).coerceIn(0, keys.lastIndex)]
                requestedFocus = next; focusedIdentity = null; focusRevision++; focusRequestRevision++
                realize?.invoke(next); realizers[scope]?.values?.toList()?.forEach { it(next) }; entries[scope]?.get(next)?.requestFocus?.invoke()
            }
            "back" -> { val active = menu; if (active != null) { closeMenu(active.token); active.dismiss() } else if (onBack?.invoke() != true) return RemoteOutcome.Unsupported }
            "home" -> { if (menu != null) return RemoteOutcome.Unsupported; val home = onHome ?: return RemoteOutcome.Unsupported; home() }
            "text_replace" -> {
                val field = search ?: return RemoteOutcome.StaleContext
                if (field.first != scope || field.second != action.text("text_nonce")) return RemoteOutcome.StaleContext
                field.third(action.text("text") ?: return RemoteOutcome.Invalid)
            }
            "play_item" -> return onPlayItem?.invoke(action.integer("item_id") ?: return RemoteOutcome.Invalid) ?: RemoteOutcome.Unsupported
            else -> return RemoteOutcome.Unsupported
        }
        return RemoteOutcome.Applied
    }
    private fun spatial(from: String, direction: String): String? {
        val current = entries[scope]?.get(from)?.frame?.takeUnless { it.isEmpty } ?: return null
        val center = current.center
        return entries[scope].orEmpty().filter { (key, entry) ->
            if (key == from || entry.frame.isEmpty || key !in orders[scope].orEmpty()) false
            else when (direction) { "left" -> entry.frame.center.x < center.x; "right" -> entry.frame.center.x > center.x; "up" -> entry.frame.center.y < center.y; else -> entry.frame.center.y > center.y }
        }.minWithOrNull(compareBy<Map.Entry<String, Entry>> { (_, entry) ->
            val dx = kotlin.math.abs(entry.frame.center.x - center.x); val dy = kotlin.math.abs(entry.frame.center.y - center.y)
            if (direction in setOf("left", "right")) dx + dy * 4 else dy + dx * 4
        }.thenBy { it.key })?.key
    }
}
internal val LocalRemoteNavigation = staticCompositionLocalOf<RemoteNavigationCoordinator?> { null }
internal val LocalRemoteScope = staticCompositionLocalOf { "restricted" }

internal object RemoteRoutes {
    fun category(route: String): String = when (route) {
        "home" -> "home"
        "library/{ids}/{name}" -> "library"
        "detail/{id}" -> "details"
        "search" -> "search"
        "player/{itemId}/{fileId}/{startMs}?audio={audio}&subtitle={subtitle}&returnChannel={returnChannel}", "live-tv?channel={channel}" -> "playback"
        else -> "restricted"
    }
}
