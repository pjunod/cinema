package tv.plurx.app.livetv

/**
 * Every server Live TV card Android draws, and which settings screen owns it.
 *
 * Paul's Developer rule (AGENTS.md, SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS):
 * Developer is a waiting room, not a home. A card that is a finished, permanent
 * setting lives in its proper section; a card that is still waiting on evidence
 * stays in Developer and says what it waits on. The web moved the Live TV
 * enable switch and the tuner, guide, recording and Library channel cards to
 * Settings → Live TV; this client follows it, in the web's order.
 */
enum class LiveTvSettingsCard(val title: String) {
    EnableLiveTv("Enable Live TV"),
    HdHomeRun("HDHomeRun Live TV"),
    ProgrammeGuide("Programme guide"),
    Recording("Recording"),
    LibraryChannels("Library channels"),
    DisplayCadence("Android TV display cadence · advisory enablement"),
}

object LiveTvSettingsPlacement {
    /** Settings → Live TV, in the web's `liveTvPanel` order. */
    val liveTvSettings: List<LiveTvSettingsCard> = listOf(
        LiveTvSettingsCard.EnableLiveTv,
        LiveTvSettingsCard.HdHomeRun,
        LiveTvSettingsCard.ProgrammeGuide,
        LiveTvSettingsCard.Recording,
        LiveTvSettingsCard.LibraryChannels,
    )

    /** Settings → Developer: only what is still waiting on evidence. */
    val developer: List<LiveTvSettingsCard> = listOf(
        LiveTvSettingsCard.DisplayCadence,
    )

    /** What Android's display-cadence switch waits on (ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET, D-01). */
    const val DISPLAY_CADENCE_GRADUATION: String =
        "Leaves Developer when: D-01's M0 three-television measurement, M4 decoder buffer budget " +
            "and M5 HDMI verification are recorded. " +
            "Then: Paul chooses: the switch moves to Settings → Playback as a permanent on/off, " +
            "or it is removed and matching is simply on."
}
