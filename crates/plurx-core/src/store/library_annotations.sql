-- Item ratings and per-user item lists (watchlist, favorites). An
-- independently recognized extension: it creates only its own objects.
CREATE TABLE library_annotations_schema(
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 version INTEGER NOT NULL CHECK(version=1)
) STRICT;
-- next statement
CREATE TABLE library_annotations_ratings(
 item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 source TEXT NOT NULL CHECK(source IN ('imdb','rotten_tomatoes','metacritic','tmdb')),
 value REAL NOT NULL CHECK(value>=0),
 scale INTEGER NOT NULL CHECK(scale IN (10,100)),
 votes INTEGER CHECK(votes IS NULL OR votes>=0),
 PRIMARY KEY(item_id,source)
) STRICT;
-- next statement
CREATE TABLE library_annotations_rating_fetches(
 item_id INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
 fetched_at_ms INTEGER NOT NULL,
 omdb_consulted INTEGER NOT NULL CHECK(omdb_consulted IN (0,1))
) STRICT;
-- next statement
CREATE TABLE library_annotations_lists(
 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 list TEXT NOT NULL CHECK(list IN ('watchlist','favorite')),
 item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 added_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_id,list,item_id)
) STRICT;
-- next statement
CREATE INDEX library_annotations_lists_recent ON library_annotations_lists(user_id,list,added_at_ms DESC);
-- next statement
CREATE INDEX library_annotations_lists_item ON library_annotations_lists(item_id);
-- next statement
INSERT INTO library_annotations_schema VALUES(1,1);
