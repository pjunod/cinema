package tv.plurx.app.player

import tv.plurx.app.data.CaptionProfile
import tv.plurx.app.data.CaptionSettings

internal fun captionPreferredProfile(profiles: List<CaptionProfile>, hint: String?, task: String,
    explicit: String? = null): CaptionProfile? {
    require(profiles.size <= 16 && profiles.map { it.profile_id }.distinct().size == profiles.size &&
        profiles.all { captionHash(it.profile_id) }) { "Invalid caption model profiles" }
    val hinted = hint?.let { id ->
        require(captionHash(id)) { "Invalid caption default profile" }
        requireNotNull(profiles.singleOrNull { it.profile_id == id }) { "Caption default is not an advertised profile" }
    }
    if (explicit != null) {
        val selected = profiles.singleOrNull { it.profile_id == explicit } ?: return null
        return if (selected.task == task) selected else profiles.singleOrNull { it.model_id == selected.model_id && it.task == task }
    }
    if (hinted != null) return if (hinted.task == task) hinted else profiles.singleOrNull {
        it.task == task && it.model_id == hinted.model_id
    }
    val preferred = profiles.filter { it.task == task && it.default == true }
    require(preferred.size <= 1) { "Ambiguous caption default profile" }
    if (preferred.isNotEmpty()) return preferred.single()
    // An explicit unavailable saved selection is advertised as all default:false.
    // Missing metadata is the legacy protocol's ordinary catalog ordering.
    return if (profiles.any { it.default != null }) null else profiles.firstOrNull { it.task == task }
}

internal fun captionSavedProfile(settings: CaptionSettings, profiles: List<CaptionProfile>, hint: String?): CaptionProfile? {
    captionPreferredProfile(profiles, hint, "translate") // Validate the returned metadata before using any preference.
    settings.profile_id?.let { id -> return profiles.singleOrNull { it.profile_id == id &&
        (settings.model_id == null || it.model_id == settings.model_id) } }
    settings.model_id?.let { model -> return profiles.firstOrNull { it.model_id == model && it.task == "translate" } }
    return captionPreferredProfile(profiles, hint, "translate")
}
