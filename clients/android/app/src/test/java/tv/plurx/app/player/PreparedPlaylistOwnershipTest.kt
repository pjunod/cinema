package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.Session

class PreparedPlaylistOwnershipTest {
    private val session = "11111111-1111-4111-8111-111111111111"
    private val origin = "https://cinema.example"
    private val path = "/api/v1/hls/$session/index.m3u8"

    @Test fun fullPlaylistResourcePathHasTheCredentialAuthority() {
        // The reproduced guard passed a resource URL to an origin-only API.
        assertNull(Session.canonicalOrigin(origin + path))
        assertEquals(PreparedPlaylistOwnership.Owned, preparedPlaylistOwnership(origin + path, origin, session))
        assertEquals(PreparedPlaylistOwnership.Owned,
            preparedPlaylistOwnership("HTTPS://CINEMA.EXAMPLE:443$path", origin, session))
        assertEquals(PreparedPlaylistOwnership.Owned,
            preparedPlaylistOwnership(origin + path + "?cache=1", origin, session))
        assertEquals(PreparedPlaylistOwnership.Owned,
            preparedPlaylistOwnership("$origin/api/v1/hls/$session/audio/index.m3u8", origin, session))
    }

    @Test fun foreignOriginOrSessionCannotOwnTheManifest() {
        assertEquals(PreparedPlaylistOwnership.OriginMismatch,
            preparedPlaylistOwnership("https://foreign.example$path", origin, session))
        assertEquals(PreparedPlaylistOwnership.OriginMismatch,
            preparedPlaylistOwnership("http://cinema.example$path", origin, session))
        assertEquals(PreparedPlaylistOwnership.OriginMismatch,
            preparedPlaylistOwnership("https://cinema.example:444$path", origin, session))
        assertEquals(PreparedPlaylistOwnership.SessionPath,
            preparedPlaylistOwnership("$origin/api/v1/hls/other-session/index.m3u8", origin, session))
        assertEquals(PreparedPlaylistOwnership.SessionPath,
            preparedPlaylistOwnership("$origin/foreign/$session/index.m3u8", origin, session))
    }

    @Test fun resourceParsingDoesNotRelaxAuthorityOrCanonicalPathValidation() {
        assertEquals(PreparedPlaylistOwnership.UserInfo,
            preparedPlaylistOwnership("https://operator@cinema.example$path", origin, session))
        assertEquals(PreparedPlaylistOwnership.Fragment,
            preparedPlaylistOwnership(origin + path + "#fragment", origin, session))
        assertEquals(PreparedPlaylistOwnership.Traversal,
            preparedPlaylistOwnership("$origin/api/v1/hls/$session/../other/index.m3u8", origin, session))
        assertEquals(PreparedPlaylistOwnership.PathEncoding,
            preparedPlaylistOwnership("$origin/api/v1/hls/$session/%2e%2e/index.m3u8", origin, session))
        assertEquals(PreparedPlaylistOwnership.PathEncoding,
            preparedPlaylistOwnership("$origin/api/v1/hls/$session/a%2fb.m3u8", origin, session))
        assertEquals(PreparedPlaylistOwnership.Scheme,
            preparedPlaylistOwnership("file:$path", origin, session))
        assertEquals(PreparedPlaylistOwnership.Malformed,
            preparedPlaylistOwnership("https://cinema.example/invalid path/index.m3u8", origin, session))
        assertEquals(PreparedPlaylistOwnership.CredentialOrigin,
            preparedPlaylistOwnership(origin + path, origin + "/unexpected", session))
    }
}
