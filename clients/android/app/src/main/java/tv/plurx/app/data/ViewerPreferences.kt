package tv.plurx.app.data

enum class ThemeId(val storageValue: String, val label: String) {
    Classic("classic", "Classic"),
    Terminal("terminal", "Terminal"),
    Noirr("noirr", "noirr"),
    Meridian("meridian", "Meridian");

    companion object {
        fun fromStorage(value: String?): ThemeId = entries.firstOrNull {
            it.storageValue == value
        } ?: Classic
    }
}

enum class ViewerLayout(val storageValue: String, val label: String) {
    Standard("standard", "Standard"), Meridian("meridian", "Meridian");
    companion object {
        fun fromStorage(value: String?): ViewerLayout = entries.firstOrNull { it.storageValue == value } ?: Standard
    }
}

enum class Appearance(val storageValue: String, val label: String) {
    System("system", "Auto (system)"),
    Light("light", "Light"),
    Dark("dark", "Dark");

    companion object {
        fun fromStorage(value: String?): Appearance = entries.firstOrNull {
            it.storageValue == value
        } ?: System
    }
}

enum class PosterSize(val storageValue: String, val label: String, val widthDp: Int) {
    Small("small", "Small", 112),
    Medium("medium", "Medium", 136),
    Large("large", "Large", 164),
    ExtraLarge("extra_large", "Extra large", 196);

    companion object {
        fun fromStorage(value: String?): PosterSize = entries.firstOrNull {
            it.storageValue == value
        } ?: Medium
    }
}

enum class HomeGrouping(val storageValue: String, val label: String) {
    Category("category", "Category"),
    Library("library", "Share name");

    companion object {
        fun fromStorage(value: String?): HomeGrouping = entries.firstOrNull {
            it.storageValue == value
        } ?: Category
    }
}

enum class HdrDisplayPeak(val storageValue: String, val label: String, val nits: Int?) {
    Auto("auto", "Auto (display report or unknown)", null),
    N400("400", "400 nits", 400), N600("600", "600 nits", 600),
    N1000("1000", "1000 nits", 1000), N1600("1600", "1600 nits", 1600),
    N2000("2000", "2000 nits", 2000), N4000("4000", "4000 nits", 4000);

    companion object {
        fun fromStorage(value: String?): HdrDisplayPeak = entries.firstOrNull {
            it.storageValue == value
        } ?: Auto
    }
}

/**
 * The stored quality preference. The *menu* is built from the server's
 * advertised ladder (see `qualityOptions`); this enum is the storage vocabulary
 * and the fallback menu for a server too old to send one.
 */
enum class PlaybackQuality(val storageValue: String, val label: String) {
    Auto("auto", "Auto"),
    Original("original", "Original"),
    Q2160("2160", "4K · 2160p"),
    Q1440("1440", "1440p"),
    Q1080("1080", "1080p"),
    Q720("720", "720p"),
    Q480("480", "480p"),
    Q360("360", "360p"),
    Q240("240", "240p"),
    Q144("144", "144p");

    /** The transcode rung this preference names, or null for Auto/Original. */
    val rungHeight: Int? get() = storageValue.toIntOrNull()

    companion object {
        fun fromStorage(value: String?): PlaybackQuality = entries.firstOrNull {
            it.storageValue == value
        } ?: Auto
    }
}

/**
 * Settings → Playback → "Subtitle switching": when a title's text subtitles
 * are made selectable. Mirrors the Apple client's `SubtitleReadiness`, raw
 * values included, and like there it never reaches the server as a field of
 * its own — it decides whether a remux/transcode play opens its HLS session
 * with `native_subtitles` from the first frame ([Instant]) or only once a
 * subtitle is actually chosen ([OnDemand], the default). See
 * `subtitleRoute` in `player/SubtitlePolicy.kt`.
 */
enum class SubtitleReadiness(val storageValue: String, val label: String) {
    Instant("instant", "Instant"),
    OnDemand("onDemand", "After a short pause");

    companion object {
        fun fromStorage(value: String?): SubtitleReadiness = entries.firstOrNull {
            it.storageValue == value
        } ?: OnDemand
    }
}

enum class OfflineQuality(val storageValue: String, val label: String, val maximumHeight: Int) {
    Standard("standard", "Standard · up to 720p", 720),
    High("high", "High · up to 1080p", 1080);

    companion object {
        fun fromStorage(value: String?): OfflineQuality = entries.firstOrNull {
            it.storageValue == value
        } ?: Standard
    }
}

enum class OfflineNetwork(val storageValue: String, val label: String) {
    WifiOnly("wifi", "Wi-Fi only"),
    Any("any", "Any network");

    companion object {
        fun fromStorage(value: String?): OfflineNetwork = entries.firstOrNull {
            it.storageValue == value
        } ?: WifiOnly
    }
}

enum class LibraryPresentation(val storageValue: String, val label: String) {
    Rows("rows", "Rows"), Grid("grid", "Grid");
    companion object {
        fun fromStorage(value: String?): LibraryPresentation = entries.firstOrNull { it.storageValue == value } ?: Rows
    }
}

data class ViewerPreferences(
    val theme: ThemeId = ThemeId.Classic,
    val layout: ViewerLayout = ViewerLayout.Standard,
    val appearance: Appearance = Appearance.System,
    val posterSize: PosterSize = PosterSize.Medium,
    val libraryPresentation: LibraryPresentation = LibraryPresentation.Rows,
    val homeGrouping: HomeGrouping = HomeGrouping.Category,
    val playbackQuality: PlaybackQuality = PlaybackQuality.Auto,
    val hdrDisplayPeak: HdrDisplayPeak = HdrDisplayPeak.Auto,
    val subtitleReadiness: SubtitleReadiness = SubtitleReadiness.OnDemand,
    val autoSkip: Boolean = false,
    val autoplayNext: Boolean = true,
    val playbackInfoMode: String = "standard",
    val offlineQuality: OfflineQuality = OfflineQuality.Standard,
    val offlineNetwork: OfflineNetwork = OfflineNetwork.WifiOnly,
    /**
     * Settings -> Developer -> "Prepared replacement": whether this device
     * tells the server it can hold two live decode pipelines, which is what
     * lets a quality change hand over without an interruption.
     *
     * On by default. The measurement behind it is still per device class and
     * the protocol field is per platform, so the Developer screen lists what
     * was measured and whether this device meets it; none of it blocks or
     * overrides the switch.
     */
    val preparedReplacement: Boolean = true,
)
