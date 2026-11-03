# WebKit report draft — native HLS frame callback can strand across seek

**Status:** prepared, not filed · **Scope:** Safari native-src HLS ·
**Prepared:** 2026-11-02

Companion to the [native callback RCA](NATIVE-CALLBACK-DELIVERY-RCA.md) and
[Fable disposition](NATIVE-CALLBACK-FABLE-REVIEW.md). This sanitized draft has
no private media URL, capture, account, host or device identity. External filing
requires separate authorization; the app repair proceeds independently.

## Proposed title

A `requestVideoFrameCallback` registered during a native HLS seek may never
deliver after `seeked`, even while playback advances

## Report text

On Safari's native HLS path, a one-shot
`HTMLVideoElement.requestVideoFrameCallback()` request registered while
`video.seeking` is true can remain pending after `seeked` and fail to deliver
while the video continues advancing. Canceling that pending request and
registering a new one after `seeked`, once `readyState >= 3`, restores callback
delivery in the observed case.

The behavior was observed with Safari 27.2 / WebKit
`22625.2.8.11.9` on macOS 27.2. The corresponding public WebKit source tag is
`WebKit-7625.2.8.11.9`. The report is limited to native-src HLS; it makes no
claim about MSE.

Expected behavior: a pending callback should eventually deliver a frame after
the seek completes and video frames continue to be presented.

Actual behavior: the request registered during seeking stays pending without
delivery. Canceling it and registering a fresh request after readiness produces
a callback. The callback is one-shot; a request-list transition from empty to
nonempty appears relevant in the WebKit source.

## Minimal reproduction

Use a Safari page with a playable native HLS manifest accessible to the test
machine. Select a seek target several seconds from the current position, then
run this script. It records only local media state and callback timing.

```html
<video id="v" controls playsinline></video>
<label>Native HLS manifest <input id="src" type="url"></label>
<button id="load">Load</button>
<button id="seek">Seek forward</button>
<pre id="log"></pre>
<script>
const video = document.querySelector('#v');
const output = document.querySelector('#log');
const log = (name, extra = {}) => output.textContent +=
  `${name} ${JSON.stringify({at: performance.now(), seeking: video.seeking,
    readyState: video.readyState, currentTime: video.currentTime, ...extra})}\n`;
let pendingId = null;
let delivered = false;

document.querySelector('#load').onclick = async () => {
  video.src = document.querySelector('#src').value;
  await video.play();
  log('loaded');
};
document.querySelector('#seek').onclick = () => {
  delivered = false;
  video.addEventListener('seeking', () => {
    pendingId = video.requestVideoFrameCallback((now, metadata) => {
      delivered = true;
      log('during-seek-request-delivered', {now, mediaTime: metadata.mediaTime});
    });
    log('request-registered-during-seeking', {pendingId});
  }, {once: true});
  video.addEventListener('seeked', () => {
    log('seeked');
    setTimeout(() => {
      if (delivered) return;
      log('still-pending-after-continued-playback');
      video.cancelVideoFrameCallback(pendingId);
      video.requestVideoFrameCallback((now, metadata) =>
        log('fresh-request-delivered', {now, mediaTime: metadata.mediaTime}));
    }, 1000);
  }, {once: true});
  video.currentTime += 5;
};
</script>
```

Reproduction requires the callback registration to occur with `seeking ===
true`. Keep the video playing after `seeked`; a paused frame path is a separate
case. The one-second observation delay only distinguishes a pending request in
this diagnostic page; it is not part of the application's repair contract.

## Evidence and limits

The application trace recorded an undelivered native request across `seeked`
and readiness, followed by callback delivery after cancel-to-empty and a fresh
request. An independent rearm-only control restored callbacks without a source
restart, new seek or CSS change. These are private application captures and
should not be attached to an upstream report without reviewing and sanitizing
them separately.

WebKit source at the cited tag starts metadata gathering when the native
request list moves from empty to nonempty, stops it when the final pending
request is canceled, and uses a one-shot AVFoundation image-change observer.
The exact internal missing-frame branch was not observed directly. This draft
asks WebKit to investigate the request's liveness after a native seek; it does
not assert a specific internal callback failure path.

**Filing:** not authorized or performed. Keep this draft unfiled until the user
authorizes external submission.
