package org.plurx.qualification.video;

import android.app.Activity;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.graphics.Bitmap;
import android.view.PixelCopy;
import android.view.SurfaceView;
import android.view.WindowManager;
import androidx.media3.common.C;
import androidx.media3.common.MediaItem;
import androidx.media3.common.PlaybackException;
import androidx.media3.common.Player;
import androidx.media3.common.Tracks;
import androidx.media3.exoplayer.ExoPlayer;
import org.json.JSONArray;
import org.json.JSONObject;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.HashSet;
import java.util.Set;

/** Finite isolated Media3 probe; never installs over the shipping application. */
public final class ProbeActivity extends Activity {
    final Handler main = new Handler(Looper.getMainLooper());
    final String[] names = {"start", "forward", "backward", "restart"};
    final long[] targets = {0, 12000, 3000, 8000};
    final JSONArray frames = new JSONArray(), phases = new JSONArray();
    final Set<String> fingerprints = new HashSet<>();
    ExoPlayer player;
    SurfaceView surface;
    String url, caseName;
    int phase = -1, observed = 0, successfulCopies = 0;
    volatile int epoch = 0;
    int priorEpochCallbacks = 0;
    long started, phaseStarted, firstPts = -1, lastPts = -1;
    double maximumClockDelta = 0;
    boolean done = false, copyPending = false, audioSelected = false;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        surface = new SurfaceView(this);
        setContentView(surface);
        started = SystemClock.elapsedRealtime();
        url = getIntent().getStringExtra("url");
        caseName = getIntent().getStringExtra("case");
        // adb reverse exposes only the owned loopback qualification server.
        if (url == null || !url.startsWith("http://127.0.0.1:")) {
            finishProbe("URL must be an owned loopback HTTP asset"); return;
        }
        advance();
        main.postDelayed(new Runnable() {
            @Override public void run() {
                if (done) return;
                long now = SystemClock.elapsedRealtime();
                if (now - started > 100000 || now - phaseStarted > 20000) {
                    finishProbe("bounded phase deadline exceeded"); return;
                }
                main.postDelayed(this, 100);
            }
        }, 100);
    }

    void attach() {
        audioSelected = false;
        if (player != null) player.release();
        player = new ExoPlayer.Builder(this).build();
        player.setVideoSurfaceView(surface);
        player.addListener(new Player.Listener() {
            @Override public void onPlayerError(PlaybackException error) {
                finishProbe(error.getErrorCodeName() + ": " + error.getMessage());
            }
            @Override public void onTracksChanged(Tracks tracks) {
                audioSelected = tracks.isTypeSelected(C.TRACK_TYPE_AUDIO);
            }
        });
        player.setVideoFrameMetadataListener((ptsUs, releaseNs, format, mediaFormat) -> {
            final int ownEpoch = epoch;
            // Metadata is scheduled before release. Observe on the main thread
            // at release time, then copy actual Surface pixels independently.
            long delay = Math.max(0, (releaseNs - System.nanoTime()) / 1000000);
            main.postDelayed(() -> observe(ownEpoch, ptsUs, format.width, format.height), delay);
        });
        player.setMediaItem(MediaItem.fromUri(url));
        player.prepare();
    }

    void advance() {
        epoch++;
        phase++;
        if (phase == names.length) { finishProbe(null); return; }
        if (player != null) player.pause();
        phaseStarted = SystemClock.elapsedRealtime();
        firstPts = lastPts = -1;
        observed = successfulCopies = priorEpochCallbacks = 0;
        fingerprints.clear(); maximumClockDelta = 0;
        if (phase == 0 || names[phase].equals("restart")) attach();
        player.seekTo(targets[phase]);
        player.play();
    }

    void observe(int ownEpoch, long ptsUs, int width, int height) {
        if (done || ownEpoch != epoch || ptsUs < targets[phase] * 1000 - 100000) return;
        // The decoder may finish an outgoing frame after seekTo has already
        // changed the application clock. Establish the new epoch with a frame
        // corresponding to the current player clock before checking monotonicity.
        // Once established, later reversals still fail; no moving window filter.
        if (firstPts < 0 && Math.abs(ptsUs / 1000.0 - player.getCurrentPosition()) >= 250) {
            priorEpochCallbacks++;
            return;
        }
        if (lastPts >= 0 && ptsUs < lastPts) { finishProbe("presentation timestamp reversed"); return; }
        if (lastPts == ptsUs) return;
        lastPts = ptsUs;
        if (firstPts < 0) firstPts = ptsUs;
        observed++;
        double delta = Math.abs(ptsUs / 1000000.0 - player.getCurrentPosition() / 1000.0);
        maximumClockDelta = Math.max(maximumClockDelta, delta);
        try {
            frames.put(new JSONObject().put("phase", names[phase]).put("media_seconds", ptsUs / 1000000.0)
                .put("player_clock_seconds", player.getCurrentPosition() / 1000.0)
                .put("wall_since_phase_seconds", (SystemClock.elapsedRealtime() - phaseStarted) / 1000.0)
                .put("audio_selected", audioSelected).put("width", width).put("height", height));
        } catch (Exception error) { finishProbe(error.toString()); return; }
        if (!copyPending && surface.getHolder().getSurface().isValid()) {
            copyPending = true;
            Bitmap bitmap = Bitmap.createBitmap(64, 36, Bitmap.Config.ARGB_8888);
            PixelCopy.request(surface, bitmap, result -> {
                copyPending = false;
                if (done || ownEpoch != epoch) { bitmap.recycle(); return; }
                if (result == PixelCopy.SUCCESS) {
                    successfulCopies++;
                    long hash = 0xcbf29ce484222325L;
                    for (int y = 0; y < 36; y++) for (int x = 0; x < 64; x++) {
                        hash = (hash ^ bitmap.getPixel(x, y)) * 0x100000001b3L;
                    }
                    fingerprints.add(Long.toUnsignedString(hash, 16));
                }
                bitmap.recycle();
                if (lastPts - firstPts >= 2500000 && observed >= 30 && successfulCopies >= 5) {
                    if (!audioSelected || fingerprints.size() < 2 || maximumClockDelta >= .25) {
                        finishProbe("audio/clock/visible-moving-surface contract failed"); return;
                    }
                    try {
                        phases.put(new JSONObject().put("phase", names[phase]).put("result", "pass")
                            .put("prior_epoch_callbacks", priorEpochCallbacks).put("frames_observed", observed).put("successful_surface_copies", successfulCopies)
                            .put("distinct_pixel_fingerprints", fingerprints.size())
                            .put("maximum_video_to_player_clock_seconds", maximumClockDelta));
                    } catch (Exception error) { finishProbe(error.toString()); return; }
                    advance();
                }
            }, main);
        }
    }

    void finishProbe(String failure) {
        if (done) return;
        done = true;
        main.removeCallbacksAndMessages(null);
        if (player != null) { player.release(); player = null; }
        try {
            JSONObject report = new JSONObject().put("schema", 1).put("case", caseName)
                .put("runtime", "Media3 1.10.1 on isolated Android emulator")
                .put("os", android.os.Build.VERSION.RELEASE).put("result", failure == null ? "pass" : "fail")
                .put("failure", failure == null ? JSONObject.NULL : failure).put("phases", phases).put("observations", frames)
                .put("scope", "Native codec and actual Surface pixels, timestamp progression, seeks and fresh-player restart. Audio selection and player clock measured; physical output A/V sync is not measured.");
            try (FileOutputStream out = new FileOutputStream(new File(getFilesDir(), "qualification.json"))) {
                out.write(report.toString(2).getBytes(StandardCharsets.UTF_8));
            }
        } catch (Exception error) { android.util.Log.e("VideoQualification", "receipt failure", error); }
    }
    @Override public void onDestroy() { if (!done) finishProbe("activity destroyed before completion"); super.onDestroy(); }
}
