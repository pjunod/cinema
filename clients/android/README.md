# plurx for Android

A native **Kotlin / Jetpack Compose** client for
[plurx](../../README.md) that runs from one APK across **phones, tablets,
foldables, and Android TV / Google TV** (Sony, Nvidia Shield, Android-based
Fire TV, Chromecast with Google TV, etc.).

Playback is **Media3 / ExoPlayer**, so this is the client that direct-plays what the
browser can't: Matroska (`.mkv`) containers, TS, and HEVC / AV1 / DTS / TrueHD wherever the
device's decoders support them. The app probes those decoders at runtime and hands the
result to the server's `/decision` endpoint, so the server only transcodes what *this*
hardware genuinely can't play — everything else streams the original file untouched.

The probe covers three things a device knows and a server can't guess: which
video and audio codecs decode here, which **Dolby Vision** profiles the decoder
delivers on a panel that shows them (4, 5, 7, and 8; Profile 7 only from an
exact `DvheDtb` decoder enumeration), and what
the **active audio route** takes as a bitstream. That last one is why a Shield
feeding an AVR keeps lossless TrueHD instead of a 256 kb/s AAC downmix: the box
has no TrueHD decoder, the receiver does, and the claim follows the route. It is
recomputed on every decision, because unplugging HDMI changes the answer.

> Status: **v0.8.0** — native viewer for phones, tablets, foldables and
> Android TV.
>
> Play builds are numbered when they are released
> ([how](../../docs/ci/SHARED-BOOKKEEPING.md#store-build-numbers)); the tree
> carries no build number, and a change records itself as a
> [changelog fragment](../../changelog.d/README.md). What each build through
> 207 changed is in [BUILD-HISTORY.md](BUILD-HISTORY.md).

## What works

- **HDHomeRun Live TV** — channel selection, pause/resume, mute, fullscreen and
  stop on a dedicated H.264/AAC live player. No library watch-progress writes.
  *Enable Live TV* is the first card in Settings → Live TV (it left Developer
  on 2026-10-10); the screen documents the required private IPv4 tuner,
  committed owner node, compatible fleet, FFmpeg, scratch space and session
  budget as advisory readiness beside the switch. [The Live TV plan](../../docs/features/HDHOMERUN-LIVE-TV-PLAN.md)
  records limits and acceptance; source tests are not physical playback proof.
- **Connect & sign in** to any plurx server by address (`http://10.42.1.10:32400`); the
  session is remembered so the app reconnects silently on next launch.
- **Adaptive home** with Continue Watching, Next Up, and Recently Added hubs,
  category- or library-grouped rails, poster-size controls, and layouts tuned
  for portrait phones, open foldables/tablets, and 10-foot TV screens.
- **Browse and search** across libraries; sort and filter grids, open shows →
  seasons → episodes, and view home-video folders and photos. The first page of
  a large library paints after one round trip and the rest fills in behind it;
  sorting is client-side, so changing the sort never re-fetches. Watch filters
  classify a show by its episodes (`rollup`), not by a watch row containers do
  not have.
- **Detail** pages with backdrop, overview, tags, file/probe status, rich media
  metadata, Resume / Start over, and Mark watched / unwatched.
- **Three web-matched themes** — Classic, Terminal, and noirr — each with
  system, light, and dark appearance modes.
- **Player** built on ExoPlayer:
  - **Direct play** of the original file with native seeking (HTTP range).
  - **Remux** via `stream.mp4`, with seek handled by re-requesting the stream at the new
    offset; **transcode** via an HLS session, released with a `DELETE` when
    playback ends. A session the server reports as `vod` — the whole stream
    already cached on disk — resumes at the saved position and seeks in place
    instead of spawning a session per scrub. HLS sessions use the server's
    `media_origin_ms`, while progressive remuxes read
    `X-Plurx-Media-Origin-Ms`; both fall back to the requested start when an
    older server omits the additive origin.
  - Custom Compose controls, a scrubber, ±10s, immersive mode,
    **Skip Intro / Skip Credits** from server markers, and native/server
    **audio / subtitle** track selection, which survive a quality change.
  - **Badges that report, not guess.** The dynamic-range chip names what the
    file carries and, when the session is delivering something weaker, dims and
    names that instead — `DV → HDR10` where the server stripped Dolby Vision
    for this device, `DV → SDR` on a transcode or an SDR panel. It combines the
    server's `delivered_dynamic_range` with the display's own
    `HdrCapabilities` and the decoder's `ColorInfo`, and the decoder wins when
    the two disagree. The playback-info panel carries the same sentence with
    the server's reason.
  - **Subtitles cost what they should.** SRT and WebVTT arrive as native HLS
    text renditions — no encoder, no lost resolution, no lost HDR — and
    switching between two of them never restarts the stream. A recognized
    `pgs-v1` track uses authenticated PNG compositions above the unchanged
    video. VobSub, styled ASS/SSA, and unsupported PGS versions burn only on
    SDR, always at the source's own height; while HDR is playing, the app
    refuses those selections and leaves the stream untouched.
  - Quality is **Auto, Original, and the rungs the server advertises for this
    source**, so a 1080p file stops offering to upscale itself. Plus persistent
    A/V sync correction, auto-skip, autoplay-next, playback decision details,
    media sessions, and picture-in-picture on supported devices.
  - Before a frame renders, a stream the device refuses gets **one** automatic
    compatibility rescue. Once HDR has rendered, a later error reconnects the
    same delivery once; a repeat is visible instead of silently becoming SDR.
  - Progress is reported back to the server every 10s and on pause/exit, which drives your
    watch state and the server-side **Trakt** scrobble.
- **Viewer preferences** remember theme, appearance, poster size, home
  grouping, quality, audio/subtitle language, auto-skip, and autoplay-next.
  Settings → Playback also holds the device-local *Prepared replacement*
  switch for quality switching, and Settings → About shows the release startup
  profile status; both moved out of Developer on 2026-10-10.
- **App-managed offline video viewing on phones and tablets.** A detail-page action
  creates a durable server preparation request, then Media3's foreground
  `DownloadService` transfers the prepared HLS package into one process-owned,
  non-evicting private cache. Wi-Fi-only is the default. Downloads restores
  queued work after process death, opens before reconnection, and uses a
  cache-only media source whose upstream is an intentional failure source, so
  offline playback cannot silently reach the server. Progress is recorded
  locally and the newest timestamp per title syncs after reconnect. Android TV
  and Google TV hide the action and route.
- **Cinema-managed offline EPUB reading on phones and tablets.** Download first
  records a durable profile intent, saves the authenticated original, reopens
  the publication to verify its exact revision and manifest, materializes only
  bounded declared resources, and atomically publishes the app-private copy.
  The local reader serves the bundled shell and publication through an
  intercepted synthetic HTTPS origin; it has no account token and refuses
  redirects, publisher network loads, file/content access, popups, and mixed
  content. Process-death reconciliation, retry/remove, local locator updates,
  and newest-dated reconnect replay are independent of Media3 video downloads.
  Android TV and Google TV expose neither the download nor reader route.
- **Android TV** ships a `LEANBACK_LAUNCHER` entry and TV banner; all viewer
  controls are D-pad focusable, focused cards grow and outline, and playback
  keys seek or toggle playback without stealing visible-control navigation.

## Requirements

- **Android 9.0 (API 28)** or newer for all variants. The audited
  debug-to-release signing rotation cannot preserve installed data on API 23–27.
- A reachable plurx server (default port `32400`). Because home servers are usually plain
  `http` on the LAN, the app sets `usesCleartextTraffic="true"`.

## Build

Three ways, in order of least host setup — all produce the same
`app/build/outputs/apk/debug/app-debug.apk`.

### In Docker — recommended for a server (no host JDK or SDK)

A pinned image ([`Dockerfile`](Dockerfile): JDK 25 + the exact Android SDK) is
the whole toolchain, so a headless box needs only Docker. From the **repo
root**:

```bash
make android          # builds the image once, then the debug APK in it
```

The APK lands in `clients/android/app/build/outputs/apk/debug/`, owned by you —
the container runs as your UID (`-u $(id -u)`), so nothing is left root-owned.
First run pulls the base image + SDK and downloads the Gradle deps; later runs
reuse the cached image and a `clients/android/.gradle-docker` cache.
`make android-image` rebuilds just the image (e.g. after bumping the SDK). Needs
the Docker daemon running and outbound internet on the first build. The image
is deliberately `linux/amd64`: AGP's Linux AAPT2 is x86-64, so Docker Desktop
emulates the same image on Apple Silicon that GitHub's Linux runner uses
natively.

### Manually — host JDK 25 + Android SDK

On a headless Ubuntu/Debian server:

```bash
# 1. JDK 25 (Gradle 9.6 runs on it; Android still emits Java 17 bytecode)
sudo apt update && sudo apt install -y openjdk-25-jdk unzip
export JAVA_HOME=/usr/lib/jvm/java-25-openjdk-amd64

# 2. Android command-line tools — must end up at cmdline-tools/latest/
mkdir -p ~/android-sdk/cmdline-tools && cd ~/android-sdk/cmdline-tools
curl -fsSL https://dl.google.com/android/repository/commandlinetools-linux-15859902_latest.zip -o clt.zip
unzip -q clt.zip && mv cmdline-tools latest && rm clt.zip
export ANDROID_HOME=$HOME/android-sdk
export PATH=$ANDROID_HOME/cmdline-tools/latest/bin:$PATH

# 3. the exact packages this app pins (Android 17 platform 37.0)
yes | sdkmanager --licenses
sdkmanager --channel=3 "platform-tools" "platforms;android-37.0" \
  "build-tools;36.0.0" "build-tools;37.0.0"

# 4. build (from clients/android/)
cd ~/plurx/clients/android && ./gradlew :app:assembleDebug
# → app/build/outputs/apk/debug/app-debug.apk
```

Persist `JAVA_HOME`, `ANDROID_HOME`, and the `PATH` line in `~/.bashrc` so new
shells find them. If the `commandlinetools` URL 404s, grab the current link from
the "Command line tools only" box at <https://developer.android.com/studio>.

### In Android Studio

Open `clients/android/` in **Android Studio** (Quail 2 / 2026.1.2+) and Run —
it provisions the SDK for you.

**Toolchain** (pinned): AGP 9.3.2, Gradle 9.7.1, built-in Kotlin
2.3.10, JDK 25, Compose BOM 2026.06.01, Media3 1.10.1,
`compileSdk`/`targetSdk` 37, `minSdk` 28 in debug, release and tests. The Gradle daemon runs on
Java 25 while Android source and bytecode stay at Java 17 for device
compatibility. Outside Docker the SDK location comes from `local.properties`
(`sdk.dir=…`) or `ANDROID_HOME`.

The release build uses the durable app signing key: `make android-release`
bind-mounts the new keystore, the audited old debug keystore and the signing
lineage. Gradle fails if a new-key input is missing; the signing helper checks
both lineage certificates, installed-data capability and the APK's effective
release signer before publication. The old key is used only to migrate
existing debug-signed installs without clearing app data on API 28+. Keep all
signing inputs outside the repository; see [Publishing](../../docs/PUBLISHING.md#51-what-has-to-change-in-the-repo-first).
`make android` still builds a debug-signed APK for local use, and
`make android-publish` serves the verified release variant.

The release build is **minified and resource-shrunk** (R8 +
`shrinkResources`), which is what keeps the APK near 3 MB rather than 18: this
app draws about twenty of `material-icons-extended`'s several thousand vectors,
and R8 removes the rest along with every library path the viewer never reaches.
`app/proguard-rules.pro` keeps what is reached reflectively — the generated
kotlinx.serialization serializers for the wire models and the generic
signatures Retrofit reads off `PlurxApi`.

## Install

For repeatable multi-device pushes, the Ansible controller builds, tests,
deduplicates USB/Wi-Fi endpoints, and refuses to call a partial deployment
successful. See
[`docs/PUBLISHING.md`](../../docs/PUBLISHING.md#ansible-owns-the-repeatable-mobile-deploy).

**Phone / tablet** — enable "Install unknown apps" for your file manager, copy the APK over,
and tap it. Or over USB with developer mode on:

```bash
adb install -r app-debug.apk
```

**Android TV / Google TV** — enable Developer options → USB/Network debugging, then push it
over the network:

```bash
adb connect 10.42.1.55        # your TV's IP
adb install -r app-debug.apk
```

The app then shows up in the TV launcher's Apps row.

## Serving it from the web UI

Instead of passing the APK around, the server can hand it out — and the **web
app shows a "Download APK" prompt automatically when opened on Android**.
Publish the APK into the server's `data_dir`:

```bash
# from the repo root, on any machine with Docker
make android-publish ANDROID_DATA_DIR=/path/to/plurx/data
# → copies the built APK to <data_dir>/plurx-android.apk
```

Or by hand: drop the file at `<data_dir>/plurx-android.apk`, or point
`PLURX_ANDROID_APK=/some/plurx.apk` at it. It's then served (unauthenticated —
it's the client app, not user data) at `GET /download/plurx-android.apk`, no
restart needed, and `/server` reports `android_app: true` so the web UI reveals
the link. The APK is deliberately kept out of git, so rebuild + re-publish when
the app changes.

**iOS** has no sideload equivalent, so the web app is an installable **PWA**:
open it in Safari and use **Share → Add to Home Screen** for a full-screen,
icon'd app. For a native iOS / Apple TV build, see
[../apple/README.md](../apple/README.md) (Xcode).

## First run

1. Launch **plurx**. On the connect screen enter your server address — host and port are
   enough (`10.42.1.10:32400`); `http://` is assumed if you leave the scheme off.
2. Sign in with your plurx username and password.
3. That's it — the token is stored (DataStore) and reused until it stops working or you sign
   out from Settings. Sign Out sends the captured bearer to the captured server
   for at most five seconds while offline cleanup proceeds, then clears local
   state on every outcome; its late result cannot erase a newer login.

## How playback decides

The interesting part lives in `data/Caps.kt` and `player/`. On play, the client:

1. Enumerates the device's decoders (`MediaCodecList`), the display's HDR types, and what the
   active audio route takes as a bitstream, and builds a caps map: `vcodec`, `acodec`,
   `container`, `hdr`, and `dv` / `dvprofile` when a Dolby Vision decoder and a Dolby Vision
   panel are both present.
2. Calls `GET /api/v1/files/{id}/decision?<caps>`. The server replies with a **delivery
   plan** (`direct` / `remux` / `transcode`) that the player executes as given.
3. `direct` → ExoPlayer streams `/files/{id}/direct` (seekable via range). `remux` →
   `/files/{id}/stream.mp4?start=…`, a live fast-seek remux, and seeks re-request at the
   new position. `transcode` → `POST /files/{id}/hls/sessions` (no `height`: the Auto rung
   is the server's choice), played as HLS; a seek opens a session at the new offset and the
   old one is released with a `DELETE`, as is the session on exit. Either way the true
   timeline position is what gets scrobbled: HLS uses `media_origin_ms`, and progressive
   remux uses the response's `X-Plurx-Media-Origin-Ms` after each seek.

This is why the Android app transcodes so rarely compared to a browser: MKV/HEVC/etc. that a
`<video>` tag refuses, ExoPlayer just plays.

## Project layout

```
app/src/main/java/tv/plurx/app/
  data/        Session, wire Models, Retrofit PlurxApi, shared OkHttp (Net),
               runtime Caps prober, DataStore SettingsStore + ViewerPreferences
  ui/          AppViewModel (session + loaders), Compose screens
               (Auth, Home, Search, Library, Detail, Photo, Settings),
               adaptive Layout, theme/, components/
  player/      PlayerScreen (Compose controls) + Controller (direct vs
               remux vs transcode playback, track menus, A/V sync,
               MediaSession, ExoPlayer wiring)
  PlurxApp     Application — points Coil's image loader at the authed OkHttp client
  MainActivity NavHost tying the screens together
```

One authenticated `OkHttpClient` (`data/Net.kt`) backs **API calls, Coil image loading, and
Media3 playback**, so posters and video streams carry the same bearer token as the API.

## Verification

The local test gate is:

```bash
./gradlew testDebugUnitTest :app:assembleDebug :app:lintDebug
```

With an emulator or device running, the Compose theme/layout and D-pad focus
checks are:

```bash
./gradlew :app:connectedDebugAndroidTest
```

`ShelfFocusTest` is the one to run on a **TV** profile. Every move in it is a
real D-pad key event, not a `FocusRequester.requestFocus()` call: it scrolls a
shelf until its first card is disposed and then presses *towards* that shelf,
and it presses towards destinations — a shelf's own "View all", the "Group by"
picker — that Compose's spatial search would not choose on its own. Those are
the two failures the focus graph exists to prevent: a requester left on a
disposed card, and a `focusProperties` block declared where no card can read it.

The v0.1 viewer layout was exercised on API 36 AOSP profiles matching Pixel 10
Pro XL (`1344×2992`), Pixel 10 Pro Fold open (`2076×2152`), and Android TV
1080p (`1920×1080`). The exact AVD names used locally are
`plurx_pixel_10_pro_xl_api36`, `plurx_pixel_10_pro_fold_api36`, and
`plurx_android_tv_1080p_api36`.

## Background screen invitations — explicit choices and recovery

The unfinished invitation controls live in Settings → Developer, alongside
Cinema remotes. Register this phone, allow notifications explicitly, and save
ON/OFF separately for each paired screen. Each screen uses one selected
transport: Push (FCM) or Resident. Readiness is advisory; unavailable providers
and permissions preserve the saved choice. The controls graduate to Remotes &
devices after physical provider and background-execution qualification.

**Optional FCM configuration.** The APK includes the real Firebase Messaging
SDK. The default build has no configured Firebase app and does not claim push
readiness. Supply your Firebase Android application's public configuration as
Gradle properties; no Google-services JSON, broker private key, provider service
account, or Cinema proof belongs in source or these properties:

```bash
./gradlew assembleDebug \
  -PcinemaFirebaseProjectId=your-project \
  -PcinemaFirebaseApplicationId=1:123456789:android:your-app-id \
  -PcinemaFirebaseApiKey=your-public-api-key \
  -PcinemaFirebaseSenderId=123456789
```

Use the Firebase project that matches the configured broker FCM project. Auto
initialization is disabled; an explicit push enrollment requests the real SDK
token. Start obtains a short-lived home ticket, sends only that ticket and the
OS token directly to its issued HTTPS broker origin, checks the exact broker
generation, then confirms with the home. The home never receives the OS token.
An unknown enrollment is renewed explicitly after readiness refresh; it is
never replayed automatically. Token rotation retains work per screen, so one
confirmed screen cannot hide another screen's uncertain renewal.
[Firebase setup](https://firebase.google.com/docs/android/setup) explains the
application configuration, and
[message handling](https://firebase.google.com/docs/cloud-messaging/android/receive-messages)
explains why the broker uses data-only messages. Physical FCM delivery is
unverified.

**Resident alternative.** Select Resident for each intended screen, then enable
its separate local resident choice and press Start. The visible connected-device
service requests Wi-Fi and uses its network only for the invitation client's
sockets and DNS. Media, downloads, reminders, and process-wide networking keep
their existing owners. Stop in the app or persistent notification takes effect
locally while offline and queues home availability reconciliation. Wi-Fi loss,
logout, setup failure, or service termination releases the network callback;
Start must be pressed again. Saved choice alone never triggers a boot, alarm,
WorkManager, sticky-service, or fallback restart. The
[Android connected-device service requirements](https://developer.android.com/develop/background-work/services/fgs/service-types#connected-device)
still require physical permission and background-execution acceptance.

**Tap and recovery.** Notifications contain one opaque invitation ID. Local
admission checks the actual saved login, exact installation, permission, channel,
proved consent, current saved choice, and bounded durable dedupe before display.
Tap retains the ID across login and requires the matching selected profile,
explicit foreground remote switch, and fresh authenticated lookup. It opens the
ordinary remote; take control and playback remain separate actions. An OFF
choice also fences previously displayed notifications before lookup selection.
Remotes & devices keeps the bounded signed-in home installation list and explicit
selected-installation removal available without a local phone proof or pairing
grant. Local reset or forgetting a saved profile does not claim remote
revocation: remove any orphan home installation while signed in there.

Encrypted invitation records retain at most eight saved profiles, 160 screen
choices/consents per profile, and 256 unexpired notification IDs. Invalid clocks,
clock rollback, corrupted records, and saturated dedupe fail closed. Invitation
and remote-proof preferences are excluded from cloud backup and device transfer.
Android's older remote vault keys already included canonical origin; they did not
retain an unambiguous origin/instance/account tuple. The new namespace does not
import those records automatically. Pair this phone again if an old pairing is
unavailable. Software compilation and focused JVM checks do not qualify native
permission dialogs, Google TV hardware, FCM delivery, or resident persistence.

## Roadmap

The native app deliberately owns the viewer experience. Library/user/key
administration and first-server setup stay in the embedded web app so the
server has one administrative control plane. Remaining native-client work:

- Per-row scroll memory when returning to Home; focus reaches every shelf and
  the "Group by" picker, but a revisited shelf starts at its first card.
- In-stream seeking within an already-produced transcode range; today a large
  seek opens a session at the new offset, matching the web player.
- Resolution and audio badge states — the chip mechanics are generic, but only
  dynamic range is wired to a delivered fact today.
- Google Cast. Phone/tablet offline downloads are implemented and awaiting the
  physical-device acceptance recorded in the implementation plan.

## License

Same as the plurx workspace.
