package tv.plurx.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.background
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.VideoLibrary
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.AccountCircle
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.LiveTv
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import tv.plurx.app.data.Features
import tv.plurx.app.data.Library
import tv.plurx.app.remote.*
import tv.plurx.app.ui.components.tvFocusRing
import tv.plurx.app.ui.components.RequestInitialFocus

internal data class MeridianDestination(val id: String, val label: String, val route: String)

internal fun meridianDestinations(features: Features, television: Boolean): List<MeridianDestination> = buildList {
    add(MeridianDestination("home", "Home", "home"))
    add(MeridianDestination("libraries", "Libraries", "libraries"))
    add(MeridianDestination("search", "Search", "search"))
    if (features.shared_libraries) add(MeridianDestination("shared-libraries", "Shared libraries", "shared-libraries"))
    if (features.live_tv) add(MeridianDestination("live-tv", "Live TV", "live-tv"))
    if (features.recordings) add(MeridianDestination("recordings", "Recordings", "recordings"))
    if (features.library_channels) add(MeridianDestination("library-channels", "Library channels", "library-channels"))
    if (!television) add(MeridianDestination("downloads", "Downloads", "downloads"))
    add(MeridianDestination("settings", "Settings", "settings"))
}

/** Chrome is a sibling of the single production NavHost. No content owner lives in it. */
@Composable
internal fun MeridianChrome(
    enabled: Boolean,
    formFactor: FormFactor,
    selected: String,
    destinations: List<MeridianDestination>,
    libraries: List<Library>,
    onSelect: (MeridianDestination) -> Unit,
    onLibrary: (Library) -> Unit,
) {
    var expanded by rememberSaveable { mutableStateOf(false) }
    val compact = formFactor == FormFactor.Compact
    val television = formFactor == FormFactor.Television
    val opener = remember { FocusRequester() }
    val navigation = LocalRemoteNavigation.current
    val scope = LocalRemoteScope.current
    LaunchedEffect(enabled, scope) { expanded = false }
    if (!enabled) return
    Box(Modifier.fillMaxSize()) {
        Surface(Modifier.align(Alignment.TopCenter).fillMaxWidth().statusBarsPadding().padding(start = if (compact) 0.dp else if (television) 88.dp else 208.dp), color = MaterialTheme.colorScheme.background) {
            Row(Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 20.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Cinema", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                IconButton(onClick = { onSelect(MeridianDestination("search", "Search", "search")) }) { Icon(Icons.Filled.Search, "Search Cinema") }
                IconButton(onClick = { onSelect(MeridianDestination("settings", "Settings", "settings")) }) { Icon(Icons.Filled.AccountCircle, "Account and appearance") }
            }
        }
        if (compact) {
            Surface(Modifier.align(Alignment.BottomCenter).fillMaxWidth().navigationBarsPadding(), color = MaterialTheme.colorScheme.surface) {
                NavigationBar(containerColor = MaterialTheme.colorScheme.surface, tonalElevation = 0.dp) {
                    (destinations.take(3) + MeridianDestination("more", "More", "more")).forEach { destination ->
                        val active = selected == destination.id || (destination.id == "more" && selected !in setOf("home", "libraries", "search"))
                        NavigationBarItem(selected = active,
                            onClick = { if (destination.id == "more") expanded = true else onSelect(destination) },
                            icon = { Icon(meridianIcon(destination.id), contentDescription = null) },
                            label = { Text(destination.label) },
                            colors = NavigationBarItemDefaults.colors(
                                selectedIconColor = MaterialTheme.colorScheme.onSecondaryContainer,
                                selectedTextColor = MaterialTheme.colorScheme.onSurface,
                                indicatorColor = MaterialTheme.colorScheme.secondaryContainer,
                                unselectedIconColor = MaterialTheme.colorScheme.onSurfaceVariant,
                                unselectedTextColor = MaterialTheme.colorScheme.onSurfaceVariant))
                    }
                }
            }
        } else {
            Surface(Modifier.width(if (television) 88.dp else 208.dp).fillMaxHeight().statusBarsPadding(), color = MaterialTheme.colorScheme.surface) {
                Column(Modifier.padding(12.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = { expanded = true }, modifier = Modifier.focusRequester(opener).heightIn(min = 56.dp).testTag("meridian-menu")) { Icon(Icons.Filled.Menu, "Open navigation") }
                    if (television) destinations.forEach { destination ->
                        val active = selected == destination.id
                        Surface(color = if (active) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surface, shape = MaterialTheme.shapes.extraLarge) {
                            IconButton(onClick = { expanded = true }, modifier = Modifier.heightIn(min = 48.dp).tvFocusRing()) { Icon(meridianIcon(destination.id), destination.label, tint = if (active) MaterialTheme.colorScheme.onSecondaryContainer else MaterialTheme.colorScheme.onSurfaceVariant) }
                        }
                    }
                    if (!television) {
                        destinations.forEach { destination -> MeridianDestinationButton(destination, selected) { onSelect(destination) } }
                        Text("Libraries", style = MaterialTheme.typography.labelLarge, modifier = Modifier.padding(8.dp))
                        libraries.forEach { library -> TextButton(onClick = { onLibrary(library) }, modifier = Modifier.heightIn(min = 48.dp)) { Text(library.name) } }
                    }
                }
            }
        }
    }
    if (expanded) {
        // A real modal window transfers native focus and blocks the covered route.
        // Owned semantic controls are confined to the rail; restricted content stays restricted.
        androidx.compose.ui.window.Dialog(onDismissRequest = { expanded = false }, properties = androidx.compose.ui.window.DialogProperties(usePlatformDefaultWidth = !television)) {
            val token = remember(scope) { "meridian-menu:$scope" }
            val initialFocus = remember { FocusRequester() }
            RequestInitialFocus(initialFocus, token = token)
            val keys = destinations.map { "meridian:${it.id}" } + libraries.map { "meridian:source:${it.id}" }
            RemoteOwnedWindow(token, keys, RemotePresentationKind.Navigation) { expanded = false }
            Box(Modifier.then(if (television) Modifier.fillMaxSize() else Modifier)) {
            Surface(shape = MaterialTheme.shapes.extraLarge, modifier = Modifier.align(if (television) Alignment.CenterStart else Alignment.Center).padding(if (television) 24.dp else 0.dp).widthIn(max = 360.dp).fillMaxHeight(.9f)) {
                Column(Modifier.padding(20.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Cinema", style = MaterialTheme.typography.headlineMedium)
                    destinations.forEach { destination ->
                        val select = { expanded = false; onSelect(destination) }
                        MeridianDestinationButton(destination, selected, Modifier.then(if (destination.id == selected) Modifier.focusRequester(initialFocus) else Modifier).remoteAction("meridian:${destination.id}", destination.label) { select(); RemoteOutcome.Applied }, select)
                    }
                    libraries.forEach { library ->
                        TextButton(onClick = { expanded = false; onLibrary(library) }, modifier = Modifier.heightIn(min = 48.dp).remoteAction("meridian:source:${library.id}", library.name) { expanded = false; onLibrary(library); RemoteOutcome.Applied }) { Text(library.name) }
                    }
                    TextButton(onClick = { expanded = false }) { Text("Close navigation") }
                }
            }
            }
            BackHandler { expanded = false }
        }
    }
}

@Composable
private fun MeridianDestinationButton(destination: MeridianDestination, selected: String, modifier: Modifier = Modifier, select: () -> Unit) {
    if (selected == destination.id) {
        FilledTonalButton(onClick = select, modifier = modifier.fillMaxWidth().heightIn(min = 48.dp).tvFocusRing()) { Icon(meridianIcon(destination.id), null); Spacer(Modifier.width(12.dp)); Text(destination.label) }
    } else {
        TextButton(onClick = select, modifier = modifier.fillMaxWidth().heightIn(min = 48.dp).tvFocusRing()) { Icon(meridianIcon(destination.id), null); Spacer(Modifier.width(12.dp)); Text(destination.label) }
    }
}

@Composable
internal fun MeridianLibraries(libraries: List<Library>, notice: String?, onLibrary: (Library) -> Unit) {
    Column(Modifier.fillMaxSize().statusBarsPadding().padding(24.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("Libraries", style = MaterialTheme.typography.headlineMedium)
        notice?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
        libraries.forEach { library ->
            OutlinedButton(onClick = { onLibrary(library) }, modifier = Modifier.fillMaxWidth().heightIn(min = 64.dp)) { Text(library.name) }
        }
        if (libraries.isEmpty()) Text("No libraries available for this account.")
    }
}

/** The existing item/detail/playback owners remain authoritative. */
@Composable
internal fun MeridianFeature(item: tv.plurx.app.data.Item, side: androidx.compose.ui.unit.Dp, featureFocus: FocusRequester, onPlay: () -> Unit, onDetails: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Box(Modifier.fillMaxWidth().padding(horizontal = side, vertical = 12.dp).height(300.dp)) {
        tv.plurx.app.ui.components.NetworkImage(tv.plurx.app.ui.components.imageUrl(item.backdrop ?: item.poster), Modifier.fillMaxSize())
        Box(Modifier.fillMaxSize().then(Modifier.background(androidx.compose.ui.graphics.Brush.horizontalGradient(listOf(colors.background, colors.background.copy(alpha = .94f), colors.background.copy(alpha = .8f))))))
        Column(Modifier.align(Alignment.BottomStart).padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("FEATURED IN YOUR LIBRARY", color = colors.primary, style = MaterialTheme.typography.labelLarge)
            Text(item.show_title ?: item.title, color = colors.onBackground, style = MaterialTheme.typography.headlineMedium)
            Text(listOfNotNull(item.year?.toString(), item.kind.replaceFirstChar { it.uppercase() }).joinToString(" · "), color = colors.onSurfaceVariant)
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Button(onClick = onPlay, modifier = Modifier.heightIn(min = 48.dp).focusRequester(featureFocus).tvFocusRing().remoteAction("meridian:feature:play", "Play featured title") { onPlay(); RemoteOutcome.Applied }) { Text(if ((item.watch?.position_ms ?: 0) > 3_000) "Resume" else "Play") }
                OutlinedButton(onClick = onDetails, modifier = Modifier.heightIn(min = 48.dp).tvFocusRing().remoteAction("meridian:feature:details", "Featured title details") { onDetails(); RemoteOutcome.Applied }) { Text("Details") }
            }
        }
    }
}

private fun meridianIcon(id: String): ImageVector = when (id) {
    "home" -> Icons.Filled.Home
    "libraries", "shared-libraries" -> Icons.Filled.VideoLibrary
    "search" -> Icons.Filled.Search
    "settings" -> Icons.Filled.Settings
    "downloads" -> Icons.Filled.Download
    "live-tv", "recordings", "library-channels" -> Icons.Filled.LiveTv
    else -> Icons.Filled.MoreHoriz
}
