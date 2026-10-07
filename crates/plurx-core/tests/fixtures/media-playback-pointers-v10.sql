-- Historical replicated pointer declaration before e8bae7647.
CREATE TABLE IF NOT EXISTS media_playback_pointers (
    user_id                INTEGER NOT NULL,
    playback_id            TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
    current_incarnation_id TEXT NOT NULL UNIQUE,
    updated_at_ms          INTEGER NOT NULL,
    PRIMARY KEY (user_id, playback_id)
) STRICT;
