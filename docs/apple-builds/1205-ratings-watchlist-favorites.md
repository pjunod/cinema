# Ratings, watchlist and favorites

**Status:** built.

Build: 270
Issue: #1205

Movie and show pages show review-site scores (IMDb, Rotten Tomatoes,
Metacritic, TMDB) from `GET /api/v1/items/{id}/annotations`, each drawn by the
one `RatingMark` view. Watchlist and Favorite buttons sit with the actions on
iOS and tvOS and are in the companion remote order. Home gains Watchlist and
Favorites rows after Next Up (tvOS shelves 4 and 5). Lists clear on sign-out
and server change, and a toggle re-reads them so Home matches on return.
