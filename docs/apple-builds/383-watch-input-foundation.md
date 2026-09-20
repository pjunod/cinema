# Shared input foundation for watch and browse

Build: 173
Issue: #383

The pure player input policy now describes browser/fullscreen presentation
and inline/overlay chrome around the existing interaction states. The shared
fixture tests cover every native watch routing cell and the unmounted-browser
return fallback. Existing native player adapters explicitly route that fallback
to their current exit behavior; this build does not change native layout,
AVPlayer/PlayerView ownership, rotation or PiP lifetime.

The corresponding web change adds the retained movie/episode watch browser.
Native layouts remain behind the separately reviewed ownership prerequisite
recorded in the [implementation ledger](../clients/WATCH-AND-BROWSE-IMPLEMENTATION.md).
iOS and tvOS simulator builds and the two focused Apple input tests passed
locally. No physical-device acceptance or upload is claimed.
