package tv.plurx.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import tv.plurx.app.BuildConfig
import tv.plurx.app.data.Appearance
import tv.plurx.app.data.HomeGrouping
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.OfflineNetwork
import tv.plurx.app.data.OfflineQuality
import tv.plurx.app.data.PosterSize
import tv.plurx.app.data.SubtitleReadiness
import tv.plurx.app.data.ThemeId
import tv.plurx.app.ui.components.ChoicePicker
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.SafeTopRow
import tv.plurx.app.ui.components.TvIconButton
import tv.plurx.app.ui.components.tvFocusRing
import tv.plurx.app.ui.theme.Muted

private val LANGS = listOf(
    "eng" to "English", "jpn" to "Japanese", "spa" to "Spanish", "fre" to "French",
    "ger" to "German", "ita" to "Italian", "por" to "Portuguese", "kor" to "Korean",
    "chi" to "Chinese", "rus" to "Russian", "hin" to "Hindi", "ara" to "Arabic",
)
private val SUB_LANGS = listOf("off" to "Off") + LANGS

internal fun appVersionLabel(versionName: String, versionCode: Int): String =
    "$versionName ($versionCode)"

@Composable
fun SettingsScreen(vm: AppViewModel, onBack: () -> Unit) {
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val offlineRecords by vm.offlineRecords.collectAsStateWithLifecycle()
    val offlineBookRecords by vm.offlineBookRecords.collectAsStateWithLifecycle()
    val formFactor = currentFormFactor()
    val side = formFactor.horizontalPadding()
    var audio by remember { mutableStateOf(LANGS.firstOrNull { it.first == vm.audioLang } ?: LANGS.first()) }
    var sub by remember { mutableStateOf(SUB_LANGS.firstOrNull { it.first == vm.subLang } ?: SUB_LANGS.first()) }
    var confirmingSignOut by remember { mutableStateOf(false) }
    val currentProfileHasDownloads = offlineRecords.any {
        it.serverInstanceId == vm.serverInstanceId && it.userId == vm.currentUserId
    } || offlineBookRecords.any {
        it.serverInstanceId == vm.serverInstanceId && it.userId == vm.currentUserId
    }

    if (confirmingSignOut) {
        AlertDialog(
            onDismissRequest = { confirmingSignOut = false },
            title = { Text("Keep this profile's downloads?") },
            text = {
                Text("Kept downloads stay on this device and reappear when this Cinema profile signs in again.")
            },
            confirmButton = {
                TextButton(onClick = vm::logout) { Text("Keep and sign out") }
            },
            dismissButton = {
                Row {
                    TextButton(onClick = { confirmingSignOut = false }) { Text("Cancel") }
                    TextButton(onClick = vm::logoutAndRemoveDownloads) {
                        Text("Remove and sign out")
                    }
                }
            },
        )
    }

    // This screen arrived with focus nowhere, so a television's first D-pad
    // press was spent finding a stop instead of moving between them. Back is
    // the one control that is always composed here.
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)

    Column(Modifier.fillMaxSize().navigationBarsPadding()) {
        SafeTopRow(
            Modifier.fillMaxWidth().padding(start = side - 12.dp, end = side, top = 8.dp),
        ) {
            TvIconButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
            }
            Text("Settings", style = MaterialTheme.typography.titleLarge)
        }

        val sectionModifier = Modifier.weight(1f)
        val playback: @Composable () -> Unit = {
            SettingsSection(
                "Playback",
                "Track choices apply when a title has more than one. Instant subtitle switching prepares " +
                    "subtitles from the first frame; After a short pause builds them the first time you turn them on.",
            ) {
                ChoicePicker("Quality", preferences.playbackQuality, PlaybackQuality.entries, { it.label }, vm::setPlaybackQuality)
                ChoicePicker("Audio language", audio, LANGS, { it.second }, onSelect = {
                    audio = it
                    vm.setLanguages(audio.first, sub.first)
                })
                ChoicePicker("Subtitle language", sub, SUB_LANGS, { it.second }, onSelect = {
                    sub = it
                    vm.setLanguages(audio.first, sub.first)
                })
                ChoicePicker(
                    "Subtitle switching",
                    preferences.subtitleReadiness,
                    SubtitleReadiness.entries,
                    { it.label },
                    vm::setSubtitleReadiness,
                )
                PreferenceSwitch("Autoplay next episode", preferences.autoplayNext, vm::setAutoplayNext)
                PreferenceSwitch("Skip intros and credits", preferences.autoSkip, vm::setAutoSkip)
            }
        }
        val appearance: @Composable () -> Unit = {
            SettingsSection("Appearance", "Theme and room brightness are independent, matching the web viewer.") {
                ChoicePicker("Theme", preferences.theme, ThemeId.entries, { it.label }, vm::setTheme)
                ChoicePicker("Appearance", preferences.appearance, Appearance.entries, { it.label }, vm::setAppearance)
                ChoicePicker("Poster size", preferences.posterSize, PosterSize.entries, { it.label }, vm::setPosterSize)
                ChoicePicker("Home layout", preferences.homeGrouping, HomeGrouping.entries, { it.label }, vm::setHomeGrouping)
            }
        }
        val downloads: @Composable () -> Unit = {
            if (formFactor != FormFactor.Television) {
                SettingsSection("Downloads", "Applied to new downloads on this device.") {
                    ChoicePicker(
                        "Quality",
                        preferences.offlineQuality,
                        OfflineQuality.entries,
                        { it.label },
                        vm::setOfflineQuality,
                    )
                    ChoicePicker(
                        "Download over",
                        preferences.offlineNetwork,
                        OfflineNetwork.entries,
                        { it.label },
                        vm::setOfflineNetwork,
                    )
                }
            }
        }

        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = side, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(28.dp),
        ) {
            // Playback → Downloads → Appearance → Account → About on every
            // form factor. Wide layouts keep two columns: Playback (with
            // Downloads beneath it, where the form factor has downloads) on
            // the left and Appearance on the right, so reading order matches
            // the single column a phone shows.
            if (formFactor == FormFactor.Compact) {
                playback()
                downloads()
                appearance()
            } else {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(32.dp)) {
                    Column(sectionModifier, verticalArrangement = Arrangement.spacedBy(28.dp)) {
                        playback()
                        downloads()
                    }
                    Column(sectionModifier) { appearance() }
                }
            }

            SettingsSection("Account", null) {
                LabeledValueRow("Signed in as", vm.username ?: "—")
                LabeledValueRow("Server", vm.serverName ?: vm.origin)
                PreferenceAction("Change server", onClick = vm::changeServer)
                PreferenceAction(
                    "Sign out",
                    destructive = true,
                    onClick = {
                        if (currentProfileHasDownloads) confirmingSignOut = true else vm.logout()
                    },
                )
            }

            SettingsSection("About", null) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text("App version", style = MaterialTheme.typography.bodyLarge)
                    Text(
                        appVersionLabel(BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE),
                        color = Muted,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
        }
    }
}

@Composable
private fun SettingsSection(
    title: String,
    description: String?,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        description?.let { Text(it, color = Muted, style = MaterialTheme.typography.bodyMedium) }
        content()
    }
}

/** A label on the left and its read-only value on the right, like the About row. */
@Composable
private fun LabeledValueRow(label: String, value: String) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 6.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = MaterialTheme.typography.bodyLarge)
        Text(value, color = Muted, style = MaterialTheme.typography.bodyMedium)
    }
}

/**
 * A full-width action row. The same focusable `tvFocusRing` + `clickable`
 * shape as [PreferenceSwitch] and `ChoicePicker`, so the D-pad lands on it
 * like any other setting; [destructive] paints the label in the theme's error
 * colour, the one convention this app has for an irreversible action.
 */
@Composable
private fun PreferenceAction(label: String, destructive: Boolean = false, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .tvFocusRing(MaterialTheme.shapes.small, focusedScale = 1.02f)
            .clickable(onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            label,
            color = if (destructive) MaterialTheme.colorScheme.error else Color.Unspecified,
            style = MaterialTheme.typography.bodyLarge,
            modifier = Modifier.weight(1f),
        )
    }
}

@Composable
private fun PreferenceSwitch(label: String, checked: Boolean, onCheckedChange: (Boolean) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .tvFocusRing(MaterialTheme.shapes.small, focusedScale = 1.02f)
            .clickable { onCheckedChange(!checked) }
            .padding(horizontal = 8.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = null)
    }
}
