package tv.plurx.app.data

import kotlinx.serialization.Serializable
import java.util.UUID

@Serializable
enum class LibraryChannelVisibility {
    personal, shared,
}

@Serializable
enum class LibraryChannelOrdering {
    balanced_shuffle, release_order,
}

@Serializable
data class LibraryChannelRecipe(
    val version: Int = 1,
    val library_ids: List<Long> = emptyList(),
    val kinds: List<String> = listOf("movie", "episode"),
    val genres_any: List<String> = emptyList(),
    val tags_any: List<String> = emptyList(),
    val keywords_any: List<String> = emptyList(),
    val year_min: Int? = null,
    val year_max: Int? = null,
    val include_item_ids: List<Long> = emptyList(),
    val include_show_ids: List<Long> = emptyList(),
    val exclude_item_ids: List<Long> = emptyList(),
    val exclude_show_ids: List<Long> = emptyList(),
    val ordering: LibraryChannelOrdering = LibraryChannelOrdering.balanced_shuffle,
    val include_specials: Boolean = false,
    val auto_refresh: Boolean = true,
    val match_all_in_scope: Boolean = false,
)

@Serializable
data class LibraryChannelOccurrence(val cycle: Long, val ordinal: Int)

@Serializable
data class LibraryChannelProgramme(
    val source: String,
    val channel_id: String,
    val generation_id: String,
    val cycle: Long,
    val ordinal: Int,
    val starts_at_ms: Long,
    val ends_at_ms: Long,
    val item_id: Long,
    val file_id: Long,
    val title: String,
) {
    val identity: String get() = "$channel_id:$generation_id:$cycle:$ordinal"
}

@Serializable
data class LibraryChannel(
    val id: String,
    val owner_user_id: Long,
    val name: String,
    val description: String = "",
    val visibility: LibraryChannelVisibility = LibraryChannelVisibility.personal,
    val enabled: Boolean = false,
    val revision: Long,
    val recipe: LibraryChannelRecipe,
    val seed: List<Int> = emptyList(),
    val active_generation_id: String? = null,
    val active_epoch_ms: Long? = null,
    val pending_generation_id: String? = null,
    val pending_epoch_ms: Long? = null,
    val favourite: Boolean = false,
    val created_at_ms: Long = 0,
    val updated_at_ms: Long = 0,
    val source: String? = null,
    val can_edit: Boolean = false,
    val can_delete: Boolean = false,
    val can_share: Boolean = false,
    val now: LibraryChannelProgramme? = null,
    val next: LibraryChannelProgramme? = null,
)

@Serializable
data class LibraryChannelDefinition(
    val request_id: String = UUID.randomUUID().toString(),
    val name: String,
    val description: String,
    val visibility: LibraryChannelVisibility,
    val enabled: Boolean,
    val recipe: LibraryChannelRecipe,
)

@Serializable
data class LibraryChannelUpdate(
    val expected_revision: Long,
    val request_id: String = UUID.randomUUID().toString(),
    val name: String,
    val description: String,
    val visibility: LibraryChannelVisibility,
    val enabled: Boolean,
    val recipe: LibraryChannelRecipe,
)

@Serializable
data class LibraryChannelMutation(val channel: LibraryChannel, val build_state: String)

@Serializable
data class LibraryChannelPreviewRequest(val recipe: LibraryChannelRecipe, val limit: Int = 50)

@Serializable
data class LibraryChannelPreviewCandidate(
    val item_id: Long,
    val file_id: Long,
    val title: String,
    val duration_ms: Long,
)

@Serializable
data class LibraryChannelMatch(
    val candidate: LibraryChannelPreviewCandidate,
    val reasons: List<String> = emptyList(),
)

@Serializable
data class LibraryChannelPreview(
    val eligible_count: Int,
    val unique_duration_ms: Long,
    val repeat_description: String,
    val content_digest: String,
    val matches: List<LibraryChannelMatch> = emptyList(),
)

@Serializable
data class LibraryChannelFavourite(val favourite: Boolean)

@Serializable
data class LibraryChannelRebuild(
    val expected_revision: Long,
    val request_id: String = UUID.randomUUID().toString(),
    val activation: String,
    val reshuffle: Boolean = false,
)

@Serializable
data class LibraryChannelBuild(
    val state: String,
    val active_generation_id: String? = null,
    val pending_generation_id: String? = null,
    val pending_activation_ms: Long? = null,
)

@Serializable
data class LibraryChannelCapabilities(
    val join_schedule: Boolean,
    val watch_from_start: Boolean,
    val seek_while_following: Boolean,
    val record: Boolean,
)

@Serializable
data class LibraryChannelResolved(
    val source: String,
    val channel_id: String,
    val definition_revision: Long,
    val generation_id: String,
    val occurrence: LibraryChannelOccurrence,
    val server_now_ms: Long,
    val starts_at_ms: Long,
    val ends_at_ms: Long,
    val item_id: Long,
    val file_id: Long,
    val position_ms: Long,
    val capabilities: LibraryChannelCapabilities,
)

@Serializable
data class LibraryChannelSessionRequest(
    val generation_id: String,
    val occurrence: LibraryChannelOccurrence,
    val tune_sequence: Long,
    val playback: CreateSessionReq,
)

@Serializable
data class LibraryChannelPurpose(
    val channel_id: String,
    val generation_id: String,
    val cycle: Long,
    val ordinal: Int,
    val tune_sequence: Long,
    val starts_at_ms: Long,
    val ends_at_ms: Long,
)

@Serializable
data class LibraryChannelSession(val playback: HlsStart, val library_channel: LibraryChannelPurpose)

@Serializable
data class DeveloperRequirement(
    val id: String,
    val title: String,
    val status: String,
    val evidence: String,
)

@Serializable
data class DeveloperEnableItem(
    val id: String,
    val title: String,
    val enabled: Boolean? = null,
    val setting: String? = null,
    val requirements: List<DeveloperRequirement> = emptyList(),
)

@Serializable
data class DeveloperReadiness(val items: List<DeveloperEnableItem> = emptyList())
