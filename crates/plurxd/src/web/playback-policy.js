(function (root, factory) {
  const policy = factory();
  if (typeof module === "object" && module.exports) module.exports = policy;
  root.PlurxPlaybackPolicy = policy;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  const DEFAULTS = Object.freeze({
    lostPerMinute: 6,
    minimumSeconds: 150,
    minimumLost: 15,
  });

  const AUTO_DEFAULTS = Object.freeze({
    sampleMs: 5_000,
    // A 5s decision clock plus a prepared successor's first fragments missed
    // the 10s cliff budget even when the handoff itself was seamless.
    decisionMs: 1_000,
    safeEstimateFactor: 0.95,
    severeEstimateRatio: 0.7,
    // A transfer below three quarters of the encoded peak while runway
    // drains is an early cliff signal on a low rung whose nominal bitrate
    // can still look safe during a mixed progress window.
    severePeakRatio: 0.75,
    // An in-flight fragment's first progress window can span a link change.
    // Leave room for that mixed sample and the encoded segment's peak, not
    // merely its nominal bitrate, when choosing a cliff replacement.
    severePeakSafetyFactor: 0.84,
    mildHeadroom: 1.3,
    mildSamples: 2,
    cooldownMs: 20_000,
    upgradeHeadroom: 1.8,
    upgradeHoldMs: 45_000,
    // The estimate alone is not evidence that a higher rung is sustainable.
    // On a JIT server hls.js measures min(link, encode) of the CURRENT rung,
    // so a fast 720p encode reads as ~200 Mb/s and clears any bandwidth bar
    // the 1080p rung can set. What actually fails one rung up is ENCODE
    // headroom, and the server reports it: `recent_speed` is its pace as a
    // multiple of realtime. Predict the pace after the switch by the pixel
    // ratio and require this much margin over realtime.
    upgradeSpeedFloor: 1.15,
    stallWindowMs: 60_000,
    dwellMs: 60_000,
    nearEmptyRunwaySeconds: 1.5,
    restartCostSeconds: 2.5,
    // Cause evidence older than three controller samples cannot explain the
    // starvation in front of the viewer. Unknown is not bandwidth pressure.
    causeMaxAgeMs: 15_000,
    recentSampleMaxAgeMs: 15_000,
  });

  const DECODE_LIMIT_DEFAULTS = Object.freeze({
    schema: "v2",
    // Ignore immaterial metadata jitter without grouping the 40 and 90 Mb/s
    // 4K loads that exposed the old codec/height poisoning.
    bitrateBucketBps: 10_000_000,
    ttlMs: 30 * 24 * 60 * 60 * 1_000,
    retestMs: 7 * 24 * 60 * 60 * 1_000,
  });

  // ---- generated from tests/playback/player-input-contract.json by
  // scripts/player-contract-table --embed; do not edit by hand ----
  const INPUT_ROUTING = {"ten-foot":{"hidden":{"left":"reveal","right":"reveal","up":"reveal","down":"reveal","select":"reveal","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"reveal","idle":"ignore"},"transport":{"left":"focus_row","right":"focus_row","up":"focus_row","down":"focus_row","select":"activate","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"hide"},"timeline":{"left":"preview","right":"preview","up":"focus_marker_or_ignore","down":"focus_transport","select":"toggle_play","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"hide"},"scrub":{"left":"preview","right":"preview","up":"cancel_then_focus_marker_or_ignore","down":"cancel_then_focus_transport","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"focus_row","right":"focus_row","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"desktop":{"hidden":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"ignore","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"transport":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"hide"},"timeline":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"toggle_play","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"hide"},"scrub":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"touch":{"hidden":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"transport":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"hide"},"timeline":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"hide"},"scrub":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}}};
  const WATCH_INPUT_ROUTING = {"browser":{"overlay":{"ten-foot":{"hidden":{"left":"reveal","right":"reveal","up":"reveal","down":"reveal","select":"reveal","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"reveal","idle":"ignore"},"transport":{"left":"focus_row","right":"focus_row","up":"focus_row","down":"focus_row","select":"activate","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"hide"},"timeline":{"left":"preview","right":"preview","up":"focus_marker_or_ignore","down":"focus_transport","select":"toggle_play","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"hide"},"scrub":{"left":"preview","right":"preview","up":"cancel_then_focus_marker_or_ignore","down":"cancel_then_focus_transport","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"focus_row","right":"focus_row","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"desktop":{"hidden":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"ignore","back":"ignore","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"transport":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"activate","back":"ignore","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"hide"},"timeline":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"toggle_play","back":"ignore","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"hide"},"scrub":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"ignore","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"touch":{"hidden":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"transport":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"hide"},"timeline":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"hide"},"scrub":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}}},"inline":{"ten-foot":{"hidden":{"left":"reveal","right":"reveal","up":"reveal","down":"reveal","select":"reveal","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"reveal","idle":"ignore"},"transport":{"left":"focus_row","right":"focus_row","up":"focus_row","down":"focus_row","select":"activate","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"ignore"},"timeline":{"left":"preview","right":"preview","up":"focus_marker_or_ignore","down":"focus_transport","select":"toggle_play","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"ignore"},"scrub":{"left":"preview","right":"preview","up":"cancel_then_focus_marker_or_ignore","down":"cancel_then_focus_transport","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"focus_row","right":"focus_row","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"desktop":{"hidden":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"ignore","back":"ignore","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"transport":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"activate","back":"ignore","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"timeline":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"toggle_play","back":"ignore","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"scrub":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"ignore","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"touch":{"hidden":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"transport":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"timeline":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"exit","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"scrub":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}}}},"fullscreen":{"overlay":{"ten-foot":{"hidden":{"left":"reveal","right":"reveal","up":"reveal","down":"reveal","select":"reveal","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"reveal","idle":"ignore"},"transport":{"left":"focus_row","right":"focus_row","up":"focus_row","down":"focus_row","select":"activate","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"hide"},"timeline":{"left":"preview","right":"preview","up":"focus_marker_or_ignore","down":"focus_transport","select":"toggle_play","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"hide"},"scrub":{"left":"preview","right":"preview","up":"cancel_then_focus_marker_or_ignore","down":"cancel_then_focus_transport","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"focus_row","right":"focus_row","up":"ignore","down":"ignore","select":"activate","back":"return_browser","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"desktop":{"hidden":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"ignore","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"transport":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"activate","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"hide"},"timeline":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"toggle_play","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"hide"},"scrub":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"return_browser","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"touch":{"hidden":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"transport":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"hide"},"timeline":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"hide"},"scrub":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"return_browser","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}}},"inline":{"ten-foot":{"hidden":{"left":"reveal","right":"reveal","up":"reveal","down":"reveal","select":"reveal","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"reveal","idle":"ignore"},"transport":{"left":"focus_row","right":"focus_row","up":"focus_row","down":"focus_row","select":"activate","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"ignore"},"timeline":{"left":"preview","right":"preview","up":"focus_marker_or_ignore","down":"focus_transport","select":"toggle_play","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"ignore","idle":"ignore"},"scrub":{"left":"preview","right":"preview","up":"cancel_then_focus_marker_or_ignore","down":"cancel_then_focus_transport","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"focus_row","right":"focus_row","up":"ignore","down":"ignore","select":"activate","back":"return_browser","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"desktop":{"hidden":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"ignore","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"transport":{"left":"skip","right":"skip","up":"ignore","down":"ignore","select":"activate","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"timeline":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"toggle_play","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_play","idle":"ignore"},"scrub":{"left":"preview","right":"preview","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"menu_focus","right":"menu_focus","up":"menu_focus","down":"menu_focus","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"return_browser","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}},"touch":{"hidden":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"return_browser","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"transport":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"timeline":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"hide","play_pause":"toggle_play","skip_back":"skip","skip_forward":"skip","tap_surface":"toggle_chrome","idle":"ignore"},"scrub":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"commit","back":"cancel","play_pause":"commit_then_toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_menu","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_menu","idle":"ignore"},"info":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_info","play_pause":"toggle_play","skip_back":"ignore","skip_forward":"ignore","tap_surface":"close_info","idle":"ignore"},"failed":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"return_browser","play_pause":"ignore","skip_back":"ignore","skip_forward":"ignore","tap_surface":"ignore","idle":"ignore"}}}}};
  const CONTRACT_TIMINGS = {"hide_after_ms":4000,"hidden_only_while_playing":true,"preview_auto_commit_ms":null,"desktop_hotkey_coalesce_ms":350,"notes":["hide_after_ms: chrome hides this long after the last input while playing. Never while paused, failed, scrubbing, or with a menu or the info panel open.","preview_auto_commit_ms is null: a pending preview commits only on `select` (ten-foot) or pointer release (touch); it never commits on a timer.","desktop_hotkey_coalesce_ms: arrow hotkeys on the desktop body accumulate against one frozen base, but the physical key owns the gesture. The timer may commit only after keyup; blur commits the visible target and attachment replacement cancels it."]};
  const CONTRACT_STEPS = {"skip_seconds":10,"preview_step_seconds":10,"preview_acceleration":[{"from_repeat":0,"step_seconds":10},{"from_repeat":5,"step_seconds":30},{"from_repeat":10,"step_seconds":60}],"vertical_seek_seconds":null,"notes":["skip_seconds is the only immediate seek step: the two transport buttons and the FF/REW media keys.","preview_acceleration applies to a HELD direction while scrubbing: repeats 0–4 move 10 s, 5–9 move 30 s, 10+ move 60 s. A released key resets the ladder.","vertical_seek_seconds is null: Up/Down never seek. Vertical is navigation between rows on every surface."]};
  // ---- end generated ----

  // ---- generated from the `live` section of
  // tests/playback/player-input-contract.json by scripts/player-contract-table
  // --embed; do not edit by hand ----
  const LIVE_INPUT_ROUTING = {"ten-foot":{"fullscreen_hidden":{"left":"reveal","right":"reveal","up":"reveal","down":"reveal","select":"reveal","back":"return_browser","play_pause":"toggle_play","tap_surface":"reveal","idle":"ignore"},"fullscreen_controls":{"left":"focus_control","right":"focus_control","up":"focus_control","down":"focus_control","select":"activate","back":"hide","play_pause":"toggle_play","tap_surface":"ignore","idle":"hide"},"browser":{"left":"delegate","right":"delegate","up":"delegate","down":"delegate","select":"delegate","back":"exit","play_pause":"ignore","tap_surface":"delegate","idle":"ignore"},"temporary_guide":{"left":"focus_cell","right":"focus_cell","up":"focus_cell","down":"focus_cell","select":"activate","back":"close_panel","play_pause":"toggle_play","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"focus_panel","right":"focus_panel","up":"focus_panel","down":"focus_panel","select":"activate","back":"close_panel","play_pause":"toggle_play","tap_surface":"ignore","idle":"ignore"},"programme_details":{"left":"focus_panel","right":"focus_panel","up":"focus_panel","down":"focus_panel","select":"activate","back":"close_panel","play_pause":"toggle_play","tap_surface":"ignore","idle":"ignore"},"stream_info":{"left":"focus_panel","right":"focus_panel","up":"focus_panel","down":"focus_panel","select":"activate","back":"close_panel","play_pause":"toggle_play","tap_surface":"ignore","idle":"ignore"}},"desktop":{"fullscreen_hidden":{"left":"strip_prev","right":"strip_next","up":"channel_up","down":"channel_down","select":"ignore","back":"exit","play_pause":"toggle_play","tap_surface":"reveal","idle":"ignore"},"fullscreen_controls":{"left":"strip_prev","right":"strip_next","up":"channel_up","down":"channel_down","select":"tune","back":"exit","play_pause":"toggle_play","tap_surface":"ignore","idle":"hide"},"browser":{"left":"delegate","right":"delegate","up":"delegate","down":"delegate","select":"delegate","back":"ignore","play_pause":"ignore","tap_surface":"delegate","idle":"ignore"},"temporary_guide":{"left":"focus_cell","right":"focus_cell","up":"focus_cell","down":"focus_cell","select":"activate","back":"close_panel","play_pause":"toggle_play","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"delegate","right":"delegate","up":"delegate","down":"delegate","select":"delegate","back":"close_panel","play_pause":"toggle_play","tap_surface":"ignore","idle":"ignore"},"programme_details":{"left":"delegate","right":"delegate","up":"delegate","down":"delegate","select":"delegate","back":"close_panel","play_pause":"toggle_play","tap_surface":"ignore","idle":"ignore"},"stream_info":{"left":"delegate","right":"delegate","up":"delegate","down":"delegate","select":"delegate","back":"close_panel","play_pause":"toggle_play","tap_surface":"ignore","idle":"ignore"}},"touch":{"fullscreen_hidden":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"exit","play_pause":"ignore","tap_surface":"toggle_chrome","idle":"ignore"},"fullscreen_controls":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"exit","play_pause":"ignore","tap_surface":"toggle_chrome","idle":"hide"},"browser":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"ignore","back":"exit","play_pause":"ignore","tap_surface":"ignore","idle":"ignore"},"temporary_guide":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_panel","play_pause":"ignore","tap_surface":"ignore","idle":"ignore"},"menu":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_panel","play_pause":"ignore","tap_surface":"close_panel","idle":"ignore"},"programme_details":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_panel","play_pause":"ignore","tap_surface":"close_panel","idle":"ignore"},"stream_info":{"left":"ignore","right":"ignore","up":"ignore","down":"ignore","select":"activate","back":"close_panel","play_pause":"ignore","tap_surface":"close_panel","idle":"ignore"}}};
  const LIVE_CONTRACT_TIMINGS = {"hide_after_ms":4000,"hidden_only_while_playing":true,"channel_coalesce_ms":350,"preview_auto_commit_ms":null,"guide_poll_unavailable_s":30,"guide_poll_min_s":15,"guide_poll_after_next_refresh_s":5,"retire_liveness_probe_ms":250,"retire_orphan_after_keepalives":3,"start_replay_attempts":1,"guide_poll_ceiling_s":1200,"notes":["hide_after_ms and hidden_only_while_playing are the finite player's values unchanged: the overlay hides this long after the last input while playing, and never while paused or failed.","channel_coalesce_ms: a held channel key accumulates one preview gesture and starts ONE session only after keyup plus the remaining quiet interval. Blur commits; direct selection or owner replacement cancels. Ten repeats must not be ten tuner GETs.","preview_auto_commit_ms is null: the neighbour strip's preview commits on select or a click, never on a timer.","guide_poll_unavailable_s: when the guide answers unavailable and says nothing about when it comes back, ask again this often. Short, because an owner with nothing to serve is usually an owner about to have something.","guide_poll_min_s: never poll the guide faster than this, whatever next_refresh_at says. A floor on fan-out, not a cadence.","guide_poll_after_next_refresh_s: poll this long after the owner's next_refresh_at, so the client asks once the answer exists rather than just before it does.","retire_liveness_probe_ms: how long a pressing web document waits for a sibling tab to claim a hint before treating it as an orphan. It decides what to RETIRE, never whether to start.","retire_orphan_after_keepalives: a hint touched more recently than this many 5 s keepalives is held by something alive; leave it.","start_replay_attempts: how many times the lease re-sends a start that got no answer, with the same request id. One — the owner joins the replay to the same session.","guide_poll_ceiling_s: never wait longer than this between guide polls, whatever next_refresh_at says. A far-future answer must not park the grid for hours."]};
  const LIVE_HOTKEYS = {"f":"fullscreen","m":"mute","p":"picture_in_picture","g":"guide_sheet","escape":"exit"};
  // ---- end generated live table ----

  // ---- generated from the surface sections of
  // tests/playback/playback-surface-contract.json by scripts/player-contract-table
  // --embed; do not edit by hand ----
  const SURFACE_CLASSES = {"preparing":{"severity":"progress","blocking":"while_not_presenting","retired_by":["presenting","intent_settled","attached_retired"]},"buffering":{"severity":"progress","blocking":"while_not_presenting","min_ms":350,"retired_by":["presenting","playback_not_requested","attached_retired"]},"recovering":{"severity":"progress","blocking":"while_not_presenting","retired_by":["presenting_after_raise","owner_success","attached_retired"]},"hold":{"severity":"notice","timed_ms":30000,"retired_by":["timer","presenting_after_raise"]},"degraded":{"severity":"notice","timed_ms":5000,"retired_by":["timer","presenting_continuous_ms"],"timer_paused_while_actions":true},"refused":{"severity":"notice","timed_ms":null,"retired_by":["intent_superseded","presenting_continuous_ms"],"default_actions":["retry"]},"exhausted":{"severity":"prompt","blocking":true,"requires_player_stopped":true,"retired_by":["user"],"title":"Playback is stalled.","default_actions":["keep_waiting","retry","close"]},"stopped":{"severity":"terminal","blocking":true,"requires_player_stopped":true,"retired_by":["user"],"default_actions":["retry","close"]}};
  const SURFACE_SOURCES = [{"id":"owner_stopped","context":"any","class":"stopped","requires":{"player_stopped":true}},{"id":"owner_exhausted","context":"any","class":"exhausted","requires":{"player_stopped":true}},{"id":"startup_exhausted","context":"any","class":"exhausted","actions":["close","retry"],"requires":{"player_stopped":true}},{"id":"hls_init_invalid","context":"start","class":"stopped","requires":{"player_stopped":true}},{"id":"hls_init_unsupported","context":"start","class":"stopped","requires":{"player_stopped":true}},{"id":"auth_401_403","context":"any","class":"stopped","actions":["sign_in","close"],"requires":{"player_stopped":true}},{"id":"vod_source_rescan_required","context":"start","class":"stopped","requires":{"player_stopped":true}},{"id":"vod_source_unsupported","context":"start","class":"stopped","requires":{"player_stopped":true}},{"id":"vod_transcode_unavailable","context":"start","class":"stopped","requires":{"player_stopped":true}},{"id":"vod_subtitle_burn_unavailable","context":"start","class":"stopped","requires":{"player_stopped":true}},{"id":"vod_disabled","context":"start","class":"stopped","requires":{"player_stopped":true}},{"id":"create_503_not_yet","context":"start","class":"preparing","codes":["startup_timeout","media_owner_transition","vod_index_pending","vod_engine_unattested","transcode_capacity_pending"],"retryable":true},{"id":"client_preparing","context":"any","class":"preparing"},{"id":"change_failed","context":"change","class":"refused","actions":["retry"]},{"id":"segment_503_not_yet","context":"attached","class":"recovering","codes":["startup_timeout","playlist_state_changed","segment_pending","segment_wait_busy","node_wait_capacity","media_owner_transition","vod_resurrection_unavailable","response_owner_transition","response_state_changed","response_owner_reclassification_unavailable","response_publication_timeout","response_completion_capacity","response_snapshot_capacity","node_maintenance","node_removal_fenced","learner_route_ineligible"]},{"id":"media_owner_lost_410","context":"any","class":"recovering","then_when_stopped":"stopped","carries":["position_ms"]},{"id":"control_hold","context":"attached","class":"hold"},{"id":"system_interruption","context":"attached","class":"hold","retired_by":["system_resumed"]},{"id":"media_waiting","context":"attached","class":"buffering"},{"id":"owner_recovery_step","context":"any","class":"recovering"},{"id":"readiness_deadline_rungs_left","context":"any","class":"recovering"},{"id":"decoder_failed","context":"any","class":"stopped","requires":{"player_stopped":true}},{"id":"black_frame_ladder_spent","context":"start","class":"exhausted","requires":{"player_stopped":true},"actions":["close","retry"]},{"id":"repeated_early_end","context":"attached","class":"stopped","requires":{"player_stopped":true}},{"id":"degraded_notice","context":"any","class":"degraded"},{"id":"log_only","context":"any","class":null}];
  const SURFACE_TIMINGS = {"buffering_min_ms":350,"hold_notice_ms":30000,"degraded_notice_ms":5000,"refused_progress_ms":10000,"notes":["refused_progress_ms is CONTINUOUS presenting on the attached generation, not accumulated playback.","buffering_min_ms debounces the surface, not the fault: a `media_waiting` fault exists from the moment it is raised and is simply not drawn until it has lasted this long.","A hidden page freezes every timer as well as every evidence sample. The reducer does this by carrying the hidden interval forward: on becoming visible again every fault's raise time is shifted by however long the page was hidden, and the continuous-presenting clock is reset, because no sample bridged the gap.","disagreement_notice_ms retires the \"Playback recovered\" banner a demotion leaves behind (§3.2). Its actions stay valid while the picture could still fail again; this much CONTINUOUS presenting is the point at which the offer is stale. Ruled by the implementer 2026-09-13 in Paul's absence — §3.1 says such a banner does not expire \"while an action is still valid\" and does not say when that ends; a banner with no end is the Android `playFailure` defect wearing a different hat.","presenting_after_raise is the contract's \"presenting evidence on the NEW attached generation\": what makes the evidence count is that the run of presentation BEGAN at or after the fault was raised. An owner that reopens in place, on the same generation, produces exactly that, and a recovering indicator that only a new generation could retire would outlive every in-place recovery."],"disagreement_notice_ms":30000};
  const SURFACE_SEVERITY_RANK = {"notice":1,"progress":2,"prompt":3,"terminal":4};
  // ---- end generated surface table ----

  function qualityForce(quality) {
    if (quality === "auto") return "auto";
    return quality === "original" || quality === "nomse"
      ? "original"
      : "transcode";
  }

  function controlLeasePresentation(mode) {
    switch (mode) {
      case "explicit":
        return Object.freeze({ ownership: "demand-owned", label: "explicit" });
      case "vod":
        return Object.freeze({ ownership: "immutable VOD", label: "VOD" });
      default:
        return Object.freeze({ ownership: "passive", label: "legacy" });
    }
  }

  function controlLeaseMode(reportedMode, vod = false, acceptedSequence = 0) {
    if (["legacy", "explicit", "vod"].includes(reportedMode)) {
      return reportedMode;
    }
    if (vod) return "vod";
    return Number(acceptedSequence) > 0 ? "explicit" : "legacy";
  }

  function transcodeHeight(quality) {
    return /^\d+$/.test(String(quality || ""))
      ? Number.parseInt(quality, 10)
      : null;
  }

  function normalizedLadder(ladder, playerHeight = Infinity) {
    const limit = playerHeight > 0 ? playerHeight : Infinity;
    const byHeight = new Map();
    for (const rung of Array.isArray(ladder) ? ladder : []) {
      const height = Number(rung && rung.height);
      const totalKbps = Number(rung && rung.total_kbps);
      const peakKbps = Number(rung && rung.peak_kbps);
      if (!(height > 0) || !(totalKbps > 0) || height > limit) continue;
      byHeight.set(height, {
        height,
        total_kbps: totalKbps,
        peak_kbps: peakKbps > 0 ? peakKbps : totalKbps,
      });
    }
    return [...byHeight.values()].sort((a, b) => a.height - b.height);
  }

  function closestRungIndex(ladder, height) {
    if (!ladder.length) return -1;
    let best = 0;
    for (let i = 1; i < ladder.length; i += 1) {
      if (
        Math.abs(ladder[i].height - height) <
        Math.abs(ladder[best].height - height)
      ) {
        best = i;
      }
    }
    return best;
  }

  function highestSafeRung(ladder, estimateKbps, defaults = AUTO_DEFAULTS) {
    if (!ladder.length || !(estimateKbps > 0)) return null;
    const ceiling = estimateKbps * defaults.safeEstimateFactor;
    for (let i = ladder.length - 1; i >= 0; i -= 1) {
      if (ladder[i].total_kbps <= ceiling) return ladder[i];
    }
    return ladder[0];
  }

  function initialAutoRung({
    ladder,
    persistedHeight = null,
    priorKbps = null,
    playerHeight = Infinity,
    defaults = AUTO_DEFAULTS,
  }) {
    const fullLadder = normalizedLadder(ladder);
    if (!fullLadder.length) return null;
    const cappedLadder = normalizedLadder(fullLadder, playerHeight);
    // The cap is a ceiling, not permission to discard the ladder's floor. A
    // player shorter than the lowest rung still starts at that lowest rung
    // when a prior or persisted choice asks the client to pick explicitly.
    const available = cappedLadder.length ? cappedLadder : [fullLadder[0]];
    const prior = highestSafeRung(available, priorKbps, defaults);
    if (prior) return prior.height;
    if (persistedHeight > 0) {
      const atOrBelowPersisted = available.filter(
        (rung) => rung.height <= Number(persistedHeight),
      );
      return (atOrBelowPersisted.at(-1) || available[0]).height;
    }
    // No prior and no learned rung: omit height and preserve the server's
    // existing encoder-aware Auto choice byte-for-byte.
    return null;
  }

  function bandwidthSeedBps({
    outgoingEstimateBps = null,
    priorKbps = null,
  }) {
    if (Number.isFinite(outgoingEstimateBps) && outgoingEstimateBps > 0) {
      return outgoingEstimateBps;
    }
    return Number.isFinite(priorKbps) && priorKbps > 0
      ? priorKbps * 1_000
      : null;
  }

  function transferSampleKbps({
    loadedBytes = null,
    loadingStartMs = null,
    loadingEndMs = null,
  } = {}) {
    const bytes = Number(loadedBytes);
    const start = Number(loadingStartMs);
    const end = Number(loadingEndMs);
    const elapsedMs = end - start;
    if (!(bytes > 0) || !(elapsedMs > 0)) return null;
    // bytes * 8 / milliseconds is numerically kilobits per second.
    return (bytes * 8) / elapsedMs;
  }

  // One browser pause can be reported near its start by hls.js and again at
  // its end by the video element. Both reports carry the wait's start time as
  // their episode identity, so a long pause still contributes exactly one
  // event to the rolling rescue window.
  function recordStallEpisode({
    events = [],
    episodeAtMs = null,
    nowMs = 0,
    windowMs = AUTO_DEFAULTS.stallWindowMs,
  } = {}) {
    const now = Number(nowMs);
    const episodeAt = episodeAtMs == null ? NaN : Number(episodeAtMs);
    const window = Number(windowMs);
    if (!Number.isFinite(now) || !(window > 0)) return [];
    const retained = (Array.isArray(events) ? events : [])
      .map(Number)
      .filter(
        (at) =>
          Number.isFinite(at) &&
          at <= now &&
          now - at < window,
      );
    if (
      Number.isFinite(episodeAt) &&
      episodeAt <= now &&
      !retained.includes(episodeAt)
    ) {
      retained.push(episodeAt);
    }
    return retained;
  }

  function playerPixelHeight({
    layoutHeight = null,
    devicePixelRatio = 1,
    intrinsicHeight = null,
  } = {}) {
    const cssHeight = Number(layoutHeight);
    if (cssHeight > 0) {
      const ratio = Number(devicePixelRatio);
      // Extreme browser zoom or synthetic environments can report enormous
      // ratios. Understating them is safe for a resolution ceiling; overstating
      // the real backing pixels is not.
      const scale = Number.isFinite(ratio) && ratio > 0
        ? Math.min(ratio, 4)
        : 1;
      return Math.round(cssHeight * scale);
    }
    const decodedHeight = Number(intrinsicHeight);
    return decodedHeight > 0 ? Math.round(decodedHeight) : Infinity;
  }

  function voluntaryGainSeconds({
    current,
    target,
    estimateKbps,
    recentSpeed,
    defaults = AUTO_DEFAULTS,
  }) {
    if (!current || !target) return 0;
    if (target.height > current.height) {
      return (
        ((target.total_kbps - current.total_kbps) / current.total_kbps) *
        (defaults.dwellMs / 1_000)
      );
    }
    if (recentSpeed > 0 && recentSpeed < 1) {
      return (
        (1 / recentSpeed - 1) *
        (defaults.dwellMs / 1_000)
      );
    }
    if (!(estimateKbps > 0)) return 0;
    return Math.max(
      0,
      (current.total_kbps * defaults.mildHeadroom / estimateKbps - 1) *
        (defaults.dwellMs / 1_000),
    );
  }

  // Pure restart-aware Auto policy. The caller owns sampling and feeds the
  // returned counters into the next sample; this function never reads hls.js,
  // the DOM, a clock, or persistence.
  /// Predicted server encode pace after moving from `current` to `target`,
  /// as a multiple of realtime, from the measured pace on the current rung.
  ///
  /// Encode cost tracks output pixels, so the ratio is by height squared (the
  /// ladder keeps one aspect). Returns null when there is nothing measured —
  /// absence must not block an upgrade, or a session that never reported
  /// speed could never rise.
  function predictedSpeed(recentSpeed, current, target) {
    const measured = Number(recentSpeed);
    if (!(measured > 0)) return null;
    const from = Number(current && current.height);
    const to = Number(target && target.height);
    if (!(from > 0) || !(to > 0)) return null;
    return measured * ((from * from) / (to * to));
  }

  function decideRung({
    ladder,
    currentHeight,
    estimateKbps = null,
    recentEstimateKbps = null,
    recentEstimateAtMs = null,
    runwaySeconds = null,
    previousRunwaySeconds = null,
    recentSpeed = null,
    activeSupplyStall = false,
    supplyStalls = 0,
    lastStallAtMs = null,
    nowMs = 0,
    lastSwitchAtMs = null,
    mildSamples = 0,
    upgradeSinceMs = null,
    playerHeight = Infinity,
    blockedHeights = null,
    causeEvidence = null,
    defaults = AUTO_DEFAULTS,
  }) {
    // Pressure decisions need the complete ladder. Filtering it by the player
    // ceiling first can map a currently-running 720p session onto the sole
    // surviving 360p rung and make the emergency branch think it is already at
    // the floor. The ceiling only vetoes an upgrade; it must never strand a
    // downgrade from a rung that is already above it.
    const available = normalizedLadder(ladder);
    const currentIndex = closestRungIndex(available, Number(currentHeight));
    if (currentIndex < 0) {
      return {
        height: null,
        reason: null,
        emergency: false,
        mildSamples: 0,
        upgradeSinceMs: null,
      };
    }
    const current = available[currentIndex];
    const estimate = estimateKbps == null ? NaN : Number(estimateKbps);
    const recentEstimate = recentEstimateKbps == null
      ? NaN
      : Number(recentEstimateKbps);
    const recentEstimateAt = recentEstimateAtMs == null
      ? NaN
      : Number(recentEstimateAtMs);
    const freshRecentEstimate =
      recentEstimate > 0 &&
      Number.isFinite(recentEstimateAt) &&
      recentEstimateAt <= nowMs &&
      nowMs - recentEstimateAt <= defaults.recentSampleMaxAgeMs
        ? recentEstimate
        : NaN;
    const runway = runwaySeconds == null ? NaN : Number(runwaySeconds);
    const priorRunway = previousRunwaySeconds == null
      ? NaN
      : Number(previousRunwaySeconds);
    const draining =
      Number.isFinite(runway) &&
      Number.isFinite(priorRunway) &&
      runway < priorRunway - 0.25;
    const nearEmpty =
      Number.isFinite(runway) && runway <= defaults.nearEmptyRunwaySeconds;
    const freshBandwidthCliff =
      freshRecentEstimate > 0 &&
      (freshRecentEstimate < current.total_kbps * defaults.severeEstimateRatio ||
       (draining && freshRecentEstimate <
         (current.peak_kbps || current.total_kbps) * defaults.severePeakRatio));
    const supplyBurst = supplyStalls >= 3;
    const starvation = activeSupplyStall || nearEmpty || supplyBurst;
    const causeKind = causeEvidence && typeof causeEvidence.kind === "string"
      ? causeEvidence.kind
      : "unknown";
    const causeAgeMs = causeEvidence && Number.isFinite(Number(causeEvidence.ageMs))
      ? Math.max(0, Number(causeEvidence.ageMs))
      : null;
    const causeFresh = causeAgeMs == null || causeAgeMs <= defaults.causeMaxAgeMs;
    const namedSuppression = causeFresh && [
      "producer-failed",
      "authority-refused",
      "loader-suspended",
      "delivery-refused",
    ].includes(causeKind)
      ? causeKind
      : null;

    if (
      namedSuppression ||
      (starvation && !freshBandwidthCliff && causeKind !== "capacity-shortfall")
    ) {
      return {
        height: current.height,
        reason: namedSuppression || "insufficient-evidence",
        action: "suppressed",
        evidence: {
          kind: namedSuppression || (causeFresh ? causeKind : "stale"),
          age_ms: causeAgeMs,
          runway_seconds: Number.isFinite(runway) ? runway : null,
          throughput_kbps: freshRecentEstimate > 0 ? freshRecentEstimate : null,
        },
        emergency: false,
        mildSamples: 0,
        upgradeSinceMs: null,
      };
    }

    const severe = freshBandwidthCliff;

    if (severe && currentIndex > 0) {
      // hls.js's EWMA intentionally carries history. At a sharp cliff that
      // history can briefly make the next rung look safe, even though the
      // fragment transfer measured the lower link. During
      // severe pressure only, bound the stable EWMA by that fresh transfer so
      // one restart lands below the cliff instead of teaching a replacement
      // instance the same lesson and walking the ladder.
      const severeEstimate = freshRecentEstimate > 0
        ? (estimate > 0
          ? Math.min(estimate, freshRecentEstimate)
          : freshRecentEstimate)
        : estimate;
      const peakCeiling = severeEstimate * defaults.severePeakSafetyFactor;
      const safe = available.slice().reverse().find(rung =>
        (rung.peak_kbps || rung.total_kbps) <= peakCeiling) || available[0];
      const safeIndex = safe
        ? closestRungIndex(available, safe.height)
        : currentIndex - 1;
      // Empty runway establishes urgency, not cause. The target comes from a
      // fresh measured transfer, so a server refusal or stopped loader can
      // never be translated into the ladder floor.
      const target = available[Math.min(currentIndex - 1, safeIndex)];
      return {
        height: target.height,
        reason: "bandwidth cliff",
        action: "switch",
        evidence: {
          kind: "bandwidth-limited",
          age_ms: nowMs - recentEstimateAt,
          runway_seconds: Number.isFinite(runway) ? runway : null,
          throughput_kbps: freshRecentEstimate,
        },
        emergency: true,
        mildSamples: 0,
        upgradeSinceMs: null,
      };
    }
    if (severe) {
      return {
        height: current.height,
        reason: null,
        emergency: false,
        mildSamples: 0,
        upgradeSinceMs: null,
      };
    }

    const estimatePressure =
      estimate > 0 && estimate < current.total_kbps * defaults.mildHeadroom;
    const serverPressure = causeFresh && causeKind === "capacity-shortfall"
      && recentSpeed > 0 && recentSpeed < 1 && draining;
    const nextMildSamples = estimatePressure || serverPressure
      ? mildSamples + 1
      : 0;
    const sinceSwitch = lastSwitchAtMs == null
      ? Infinity
      : nowMs - lastSwitchAtMs;
    const voluntaryAllowed =
      sinceSwitch >= defaults.cooldownMs && sinceSwitch >= defaults.dwellMs;

    if (
      currentIndex > 0 &&
      nextMildSamples >= defaults.mildSamples &&
      voluntaryAllowed
    ) {
      const target = available[currentIndex - 1];
      const gain = voluntaryGainSeconds({
        current,
        target,
        estimateKbps: estimate,
        recentSpeed,
        defaults,
      });
      if (gain > defaults.restartCostSeconds) {
        return {
          height: target.height,
          reason: serverPressure ? "server supply" : "bandwidth pressure",
          emergency: false,
          mildSamples: 0,
          upgradeSinceMs: null,
        };
      }
    }

    const playerCeiling = Number(playerHeight) > 0
      ? Number(playerHeight)
      : Infinity;
    const nextCandidate = available[currentIndex + 1];
    // A rung that already failed this playback is not a candidate. The
    // dwell/hold timers alone cannot end a loop whose every cycle looks new:
    // a rung that fails and is re-entered on schedule oscillates forever.
    const blocked = blockedHeights instanceof Set
      ? blockedHeights
      : new Set(Array.isArray(blockedHeights) ? blockedHeights : []);
    const next = nextCandidate
      && nextCandidate.height <= playerCeiling
      && !blocked.has(nextCandidate.height)
      ? nextCandidate
      : null;
    // Encode headroom, not just bandwidth — see `upgradeSpeedFloor`.
    const predicted = next
      ? predictedSpeed(recentSpeed, current, next)
      : null;
    const encodeHeadroom =
      predicted == null || predicted >= defaults.upgradeSpeedFloor;
    const stallFree =
      lastStallAtMs == null || nowMs - lastStallAtMs >= defaults.stallWindowMs;
    const upgradeReady =
      next &&
      estimate > next.total_kbps * defaults.upgradeHeadroom &&
      encodeHeadroom &&
      stallFree &&
      !estimatePressure &&
      !serverPressure;
    const nextUpgradeSince = upgradeReady
      ? (upgradeSinceMs == null ? nowMs : upgradeSinceMs)
      : null;
    if (
      next &&
      nextUpgradeSince != null &&
      nowMs - nextUpgradeSince >= defaults.upgradeHoldMs &&
      voluntaryAllowed &&
      voluntaryGainSeconds({
        current,
        target: next,
        estimateKbps: estimate,
        recentSpeed,
        defaults,
      }) > defaults.restartCostSeconds
    ) {
      return {
        height: next.height,
        reason: "bandwidth recovered",
        emergency: false,
        mildSamples: 0,
        upgradeSinceMs: null,
      };
    }

    return {
      height: current.height,
      reason: null,
      emergency: false,
      mildSamples: nextMildSamples,
      upgradeSinceMs: nextUpgradeSince,
    };
  }

  function sessionHeight({
    quality,
    sourceHeight,
    decidedMethod,
    burnedSubtitle = false,
    refusedOriginal = false,
  }) {
    if (!(sourceHeight > 0)) return null;
    if (qualityForce(quality) === "original") return sourceHeight;
    if (
      qualityForce(quality) === "auto" &&
      decidedMethod !== "transcode" &&
      (burnedSubtitle || refusedOriginal)
    ) {
      return sourceHeight;
    }
    return null;
  }

  function nativeHlsAvailable({
    canPlayNativeHls,
    hasWebKitPlaybackTarget,
    hlsJsSupported,
  }) {
    if (!canPlayNativeHls) return false;
    if (hasWebKitPlaybackTarget) return true;
    return !hlsJsSupported;
  }

  function hlsTransport({ nativeHls, hevcCopy, hlsJsSupported = true }) {
    return nativeHls && (hevcCopy || !hlsJsSupported) ? "native" : "mse";
  }

  function copyAudioNeedsTranscode({
    codec,
    clientAudioCodecs = [],
    msePairSupported = false,
    nativeHls = false,
  }) {
    const normalized = String(codec || "").toLowerCase();
    if (!normalized) return true;
    return nativeHls
      ? !clientAudioCodecs.some(
          (candidate) => String(candidate).toLowerCase() === normalized,
        )
      : !msePairSupported;
  }

  function initialRoute({
    method,
    selectedAudioIndex = 0,
    nativeHls = false,
    segmentedRemux = false,
    requiresHls = false,
    hlsAvailable = false,
  }) {
    if (method === "transcode") return "transcode_hls";
    if (method === "remux" && requiresHls) {
      return hlsAvailable ? "copy_hls" : "unsupported_hevc_delivery";
    }
    if (method === "remux" && (nativeHls || segmentedRemux)) {
      return "copy_hls";
    }
    if (method === "remux" || (method === "direct_play" && selectedAudioIndex !== 0)) {
      return "progressive_remux";
    }
    return "direct";
  }

  // A node-local VOD index is an optimization prerequisite, not evidence that
  // this browser cannot play the source. MSE browsers already proved the same
  // remux viable before choosing copy-HLS, so a cold/missing index may fall
  // back to the progressive pipe. Native HLS cannot: Safari does not accept
  // the fragmented progressive response, which is why it selected HLS in the
  // first place.
  function indexPendingFallback({
    code = null,
    method = null,
    nativeHls = false,
    requiresHls = false,
  } = {}) {
    return code === "vod_index_pending" && method === "remux" && !nativeHls
      && !requiresHls
      ? "progressive_remux"
      : "fail";
  }

  function fallbackAction({
    method,
    alreadyTried = false,
    playbackIsReal = false,
    mediaFailure = true,
  }) {
    return mediaFailure &&
      !alreadyTried &&
      !playbackIsReal &&
      (method === "direct_play" || method === "remux")
      ? "transcode"
      : "fail";
  }

  // A wait that persists has already outlived hls.js/browser nudges, but time
  // plus buffered media does not identify a decoder fault. Only independently
  // attributed decoder evidence may trade an Auto remux for a compatible
  // transcode. A generic network disconnect says the transfer failed, not
  // that its recipe exceeds the link; measured capacity adaptation remains
  // with the existing Auto controller. An explicit quality choice is always
  // reconnected exactly. One automatic attempt is the hard bound that prevents
  // a bad file or dead network from restart-looping.
  function stallRecoveryAction({
    method,
    quality = "auto",
    cause = "unknown",
    alreadyRecovered = false,
  }) {
    if (alreadyRecovered) return "prompt";
    if (
      method === "remux" &&
      qualityForce(quality) === "auto" &&
      cause === "decoder"
    ) {
      return "transcode";
    }
    return ["direct_play", "remux", "transcode"].includes(method)
      ? "restart"
      : "prompt";
  }

  // A persistent supply stall has one restart budget, so spend it on the rung
  // the same Auto controller would choose from the link evidence already in
  // hand. Falling back to `null` preserves the server's bounded one-rung step
  // when the player has no trustworthy ladder or estimate yet.
  function stallRecoveryTargetHeight({
    method,
    quality = "auto",
    kind = null,
    ladder = [],
    currentHeight = null,
    estimateKbps = null,
    recentEstimateKbps = null,
    recentEstimateAtMs = null,
    nowMs = 0,
    defaults = AUTO_DEFAULTS,
  } = {}) {
    const current = Number(currentHeight);
    if (
      method !== "transcode" ||
      qualityForce(quality) !== "auto" ||
      kind !== "supply" ||
      !(current > 0)
    ) {
      return null;
    }
    const decision = decideRung({
      ladder,
      currentHeight: current,
      estimateKbps,
      recentEstimateKbps,
      recentEstimateAtMs,
      runwaySeconds: 0,
      activeSupplyStall: true,
      nowMs,
      defaults,
    });
    return decision.height > 0 && decision.height < current
      ? decision.height
      : null;
  }

  // Bind only an evidence-backed Auto adaptation to the exact session whose
  // rung is being changed. The legacy `stall` reason asks the server to lower
  // Auto one rung, so sending it for an unattributed presentation repair would
  // silently change the selected recipe. Same-recipe restarts retain the
  // stable playback supersession id carried by every create and omit this
  // adaptation-only pair.
  function stallReopenSessionOptions({
    options = {},
    forceReopen = false,
    method = null,
    sessionId = null,
    cause = "unknown",
  } = {}) {
    const result = { ...options };
    if (
      forceReopen &&
      method === "transcode" &&
      sessionId &&
      cause === "network"
    ) {
      result.previous_session_id = sessionId;
      result.reopen_reason = "stall";
    }
    return result;
  }

  function fallbackResetBeforeOpen({ reason }) {
    return ["stall-recovery", "stall-manual", "auto-supply"].includes(reason);
  }

  // Read a refused stream response into the cause the server named.
  //
  // hls.js's ERROR event carries only the response *line* — `{code: status,
  // text: statusText}` — and drops the body on the floor, so every refusal
  // reached the viewer as the same "Playback failed to start
  // (levelLoadError)". The playlist and session routes now answer with
  // `{code, message}` naming what actually happened; the player captures that
  // body from the XHR and hands it here.
  //
  // `null` for anything that is not a legible refusal. Guessing would explain
  // the wrong failure with complete confidence, which is worse than the
  // generic sentence this replaces.
  const STREAM_FAILURE_BODY_MAX_CHARS = 16_384;
  const STREAM_FAILURE_MESSAGE_MAX_CHARS = 512;

  function parseStreamFailure({ status, body }) {
    if (!(status >= 400)) return null;
    if (typeof body !== "string" || body.length > STREAM_FAILURE_BODY_MAX_CHARS) return null;
    let parsed = null;
    try {
      parsed = JSON.parse(body);
    } catch (e) {
      return null;
    }
    if (!parsed || typeof parsed !== "object") return null;
    // `{code, message}` is the typed shape; `{error}` is the legacy body
    // several routes still use, and it carries a readable sentence too.
    const message =
      typeof parsed.message === "string" && parsed.message.trim()
        ? parsed.message.trim()
        : typeof parsed.error === "string" && parsed.error.trim()
          ? parsed.error.trim()
          : null;
    if (!message) return null;
    const boundedMessage = message.length > STREAM_FAILURE_MESSAGE_MAX_CHARS
      ? `${message.slice(0, STREAM_FAILURE_MESSAGE_MAX_CHARS - 1)}\u2026`
      : message;
    return {
      status,
      code: typeof parsed.code === "string" ? parsed.code : null,
      message: boundedMessage,
      // Where the film was when the server lost it. `media_owner_lost` is the
      // body that carries it, and it is what a Try again has to reopen at:
      // without it the viewer is sent back to the start of a film they were
      // ninety minutes into. Absent, null or unparseable stays `null` — a
      // guessed position is worse than no position.
      position_ms:
        parsed.film_position_ms != null &&
        Number.isFinite(Number(parsed.film_position_ms))
          ? Number(parsed.film_position_ms)
          : null,
    };
  }

  // Which fault source a refused stream response IS (contract §3.3).
  //
  // Driven by the embedded table so the three clients cannot drift: rows are
  // scanned in order and the first whose id and context both match wins, which
  // is the table's own precedence rule. A row whose id is the server's code
  // matches on that code; the "not yet" row lists its codes; the three rows
  // below are the ones the server answers with a status and no code of its own.
  //
  // `null` means no row claims this response. The caller keeps whatever
  // handling it had rather than being handed a class the table never assigned.
  const SURFACE_REFUSAL_BY_STATUS = Object.freeze({
    auth_401_403: (status) => status === 401 || status === 403,
    media_owner_lost_410: (status) => status === 410,
  });

  // A row whose codes only mean what they mean on ONE status (ruling 4).
  // §3.3 row 8 is "playlist/segment 503 WITH a 'not yet' code", and neither
  // half is sufficient alone: the status alone made every attached 503 a
  // `recovering` — `vod_disabled` included — and the code alone would let
  // `media_owner_transition`, which is also a create code, match off a status
  // that never carried it. A 503 whose code is not in the row's list falls
  // through to whichever row that code actually names, and to the caller's own
  // handling when no row names it.
  const SURFACE_REFUSAL_STATUS_FOR = Object.freeze({
    segment_503_not_yet: 503,
  });

  function classifyStreamFailure({ status, code, context } = {}) {
    const where = context || "attached";
    const numeric = Number(status);
    const named = typeof code === "string" && code.trim() ? code.trim() : null;
    for (const row of SURFACE_SOURCES) {
      if (!(row.context === "any" || row.context === where)) continue;
      const byStatus = SURFACE_REFUSAL_BY_STATUS[row.id];
      if (byStatus) {
        if (byStatus(numeric)) return row.id;
        continue;
      }
      // A viewer-requested replacement that failed is a refusal about the
      // destination, whatever the server said about it: the predecessor keeps
      // playing and keeps its own faults.
      if (row.id === "change_failed") {
        if (where === "change") return row.id;
        continue;
      }
      if (!named) continue;
      const onlyOn = SURFACE_REFUSAL_STATUS_FOR[row.id];
      if (onlyOn != null && numeric !== onlyOn) continue;
      if (row.id === named) return row.id;
      if (Array.isArray(row.codes) && row.codes.includes(named)) return row.id;
    }
    return null;
  }

  // ---- M5: the three bounded recovery additions ------------------------------
  //
  // The numbers, and the two decisions that read them, live here because all
  // three clients have to agree on them and only one of the three can run this
  // file. Apple (`PlayerController.CreateRetry`) and Android
  // (`createRetryStep` in `PlaybackPolicy.kt`) restate them, and
  // `web-policy.test.js` reads both files back and fails if a number drifts.
  //
  // Nothing here touches a player: these are the owner's own arithmetic, and
  // the presenter never sees them (PLAYBACK-SURFACE-CONTRACT.md §3.0).

  const CREATE_RETRY = Object.freeze({
    // The review's ladder, as a closed list. A fourth rung is not "4 s again":
    // the ladder is spent after the third retry and the owner says so.
    backoff_ms: Object.freeze([1_000, 2_000, 4_000]),
    // ABSOLUTE, from the first attempt — not per attempt. A server that holds
    // each create for a minute cannot stretch the sequence past this, which is
    // the whole reason the review asked for a deadline rather than a count.
    deadline_ms: 60_000,
    // The fixture's `create_503_not_yet` row, and only it (§3.3 row 6). A
    // refusal the server did not explain, or explained with any other code, is
    // not a "still building" answer and is not retried.
    source: "create_503_not_yet",
  });

  // What the create owner does after a refusal it has already classified.
  //
  // `attempt` counts retries already made (0 before the first one), `elapsedMs`
  // is measured from the FIRST attempt, and `source` is what
  // `classifyStreamFailure` answered. Returns one of:
  //
  //   {action: "fail"}                — not a "not yet" answer; the caller's
  //                                     existing handling stands, unchanged.
  //   {action: "retry", delayMs}      — wait this long and re-post the SAME
  //                                     request identity.
  //   {action: "exhausted", reason}   — the owner has nothing left: stop the
  //                                     player and raise `exhausted`.
  //
  // The deadline is checked twice on purpose: once for time already spent, and
  // once for the retry that would START after it. Scheduling an attempt that
  // could only begin past the deadline is how an "absolute" bound turns back
  // into a per-attempt one.
  function createRetryStep({ attempt = 0, elapsedMs = 0, source = null } = {}) {
    if (source !== CREATE_RETRY.source) return { action: "fail" };
    const spent = Number.isFinite(Number(elapsedMs)) ? Math.max(0, Number(elapsedMs)) : 0;
    if (spent >= CREATE_RETRY.deadline_ms) return { action: "exhausted", reason: "deadline" };
    const index = Math.max(0, Math.trunc(Number(attempt) || 0));
    if (index >= CREATE_RETRY.backoff_ms.length) {
      return { action: "exhausted", reason: "ladder_spent" };
    }
    const delayMs = CREATE_RETRY.backoff_ms[index];
    if (spent + delayMs >= CREATE_RETRY.deadline_ms) {
      return { action: "exhausted", reason: "deadline" };
    }
    return { action: "retry", delayMs };
  }

  // The web's hls.js retry (M5 addition 2). One `startLoad(position)` after
  // this long, and ONE per attach shared between the network-class fatal and
  // the `segment_503_not_yet` row — whichever fires first spends it. After
  // that the existing reopen path is what recovers, exactly as it does today.
  const HLS_RETRY = Object.freeze({
    delay_ms: 2_000,
    per_attach: 1,
  });

  // Media recovery spends the same per-attach budget as network startLoad.
  // The item bound survives internal reopens, so a decoder that repeatedly
  // rejects the same title cannot acquire a fresh rescue on every attachment.
  const HLS_MEDIA_RECOVERY = Object.freeze({
    per_item: 2,
    settle_ms: 4_000,
  });

  // A local rolling/progressive seek gets one short chance to land. The
  // transport owns the timer; this policy owns the frozen bound and route.
  const SEEK_LOCAL_SETTLE_MS = 3_000;

  function seekRoute({
    method,
    copyHls = false,
    vod = false,
    forceReopen = false,
    changing = false,
    targetMs,
    bufferedMs = [],
    publishedMs = null,
    holdbackMs = 0,
  } = {}) {
    const target = Number(targetMs);
    if (!Number.isFinite(target) || target < 0 || forceReopen || changing) {
      return { route: "reopen" };
    }
    if (method === "direct_play") return { route: "local", atMs: target, basis: "direct" };
    if (vod) return { route: "local", atMs: target, basis: "vod" };
    const rolling = Boolean(copyHls) || method === "transcode";
    if (!rolling && method !== "remux") return { route: "reopen" };
    for (const range of Array.isArray(bufferedMs) ? bufferedMs : []) {
      const from = Number(range && range.from);
      const through = Number(range && range.through);
      if (Number.isFinite(from) && Number.isFinite(through)
          && target >= from && target <= through) {
        return { route: "local", atMs: target, basis: "buffered" };
      }
    }
    if (rolling && publishedMs) {
      const from = Number(publishedMs.from);
      const through = Number(publishedMs.through);
      const holdback = Math.max(0, Number(holdbackMs) || 0);
      if (Number.isFinite(from) && Number.isFinite(through)
          && target >= from && target <= through - holdback) {
        return { route: "local", atMs: target, basis: "published" };
      }
    }
    return { route: "reopen" };
  }

  // One absolute startup policy for the web HLS attachment. hls.js owns the
  // retry ladder inside a source-load cycle; the application owns one
  // corrective cycle and the ceiling across both. Keeping the numbers here
  // makes the executor and the stock-loader adapter read the same contract.
  const HLS_STARTUP = Object.freeze({
    cold_deadline_ms: 40_000,
    seek_deadline_ms: 20_000,
    manifest_dispatch_ceiling: 16,
    manifest_load_policy: Object.freeze({
      default: Object.freeze({
        maxTimeToFirstByteMs: 10_000,
        maxLoadTimeMs: 12_000,
        timeoutRetry: Object.freeze({
          maxNumRetry: 1,
          retryDelayMs: 1_000,
          maxRetryDelayMs: 1_000,
        }),
        errorRetry: Object.freeze({
          maxNumRetry: 7,
          retryDelayMs: 1_000,
          maxRetryDelayMs: 4_000,
        }),
      }),
    }),
  });

  // Typed answers that end this source-load cycle immediately. In particular,
  // a terminal JSON body carried on HTTP 503 must not be retried merely because
  // hls.js's stock status policy retries 5xx responses.
  const HLS_STARTUP_TERMINAL_CODES = Object.freeze([
    "hls_init_invalid",
    "hls_init_unsupported",
    "media_session_ended",
    "media_owner_lost",
    "media_owner_transition",
    "producer_failed",
    "producer_exited",
    "producer_ended",
    "session_gone",
    "session_failed",
    "vod_disabled",
    "vod_source_rescan_required",
    "vod_source_unsupported",
    "vod_subtitle_burn_unavailable",
    "vod_transcode_unavailable",
  ]);

  function hlsStartupResponseAction({ status = 0, code = null } = {}) {
    const numeric = Number(status) || 0;
    if (numeric === 401 || numeric === 403) return "terminal";
    if (typeof code === "string" && HLS_STARTUP_TERMINAL_CODES.includes(code)) {
      return "terminal";
    }
    return "retry";
  }

  function hlsStartupSendAction({
    state = "active",
    nowMs = 0,
    deadlineMs = 0,
    dispatches = 0,
    current = true,
  } = {}) {
    if (!current || state === "cancelled" || state === "presenting") return "cancel";
    if (state === "paused") return "pause";
    if (state === "exhausted") return "exhaust";
    if (Number(nowMs) >= Number(deadlineMs)) return "exhaust";
    if ((Number(dispatches) || 0) >= HLS_STARTUP.manifest_dispatch_ceiling) {
      return "exhaust";
    }
    return "send";
  }

  // `used` is the attach's spent budget. A pure predicate so the one place
  // that schedules the retry cannot disagree with the test that pins it.
  function hlsRetryAllowed({ used = 0 } = {}) {
    return (Number(used) || 0) < HLS_RETRY.per_attach;
  }

  function hlsMediaFatalAction({
    type,
    details,
    sourceBufferName = null,
    retryUsed = 0,
    itemRecoveries = 0,
    recoveredAtMs = null,
    nowMs = 0,
  } = {}) {
    if (type !== "mediaError") return "none";
    if (
      details === "bufferIncompatibleCodecsError" ||
      details === "bufferAddCodecError"
    ) {
      return "fallback";
    }
    if (!hlsRetryAllowed({ used: retryUsed })) return "fallback";
    if (itemRecoveries >= HLS_MEDIA_RECOVERY.per_item) return "fallback";
    if (
      recoveredAtMs != null &&
      Number(nowMs) - Number(recoveredAtMs) < HLS_MEDIA_RECOVERY.settle_ms
    ) {
      return "fallback";
    }
    if (itemRecoveries > 0) {
      return (details === "bufferAppendError" || details === "bufferAppendingError") &&
        sourceBufferName === "audio"
        ? "swap_audio"
        : "fallback";
    }
    return "recover";
  }

  // Android's `BEHIND_LIVE_WINDOW` recovery (M5 addition 3). Finite timelines
  // only: Media3's own answer is `seekToDefaultPosition()`, which is a
  // LIVE-EDGE policy — on a finite timeline it skips content — so the contract
  // forbids it and this says when the seek-to-last-position recovery applies.
  // Restated in `PlaybackPolicy.kt`; the parity test reads both.
  const BEHIND_LIVE_WINDOW_CODE = 1002;

  function behindLiveWindowRecovers({ errorCode = null, live = true, used = 0 } = {}) {
    if (Number(errorCode) !== BEHIND_LIVE_WINDOW_CODE) return false;
    // A live item keeps today's `Fail`: there is no last real position on a
    // window that has moved past the viewer.
    if (live !== false) return false;
    return (Number(used) || 0) < 1;
  }

  // The two overlay lines for a failure the server explained.
  //
  // A still-starting session is deliberately not called a failure: the server
  // holds a playlist request for its whole hardware→software recovery budget,
  // and what comes back after that is "not yet", not "never". Telling someone
  // their stream permanently failed while the server is still building it is
  // the defect this pair of functions exists to close.
  //
  // A refusal older than this explains some earlier request, not the failure
  // in front of the viewer. Generous, because the whole point is that a
  // startup can legitimately take the better part of a minute — but bounded,
  // because this file's other confident-and-wrong explanation (the
  // stale-reason trap, PLAYBACK.md) is exactly the mistake to avoid repeating.
  const FAILURE_FRESH_MS = 90_000;

  // `null` means nothing better than the caller's generic sentence is known.
  // `now` is optional: a caller that does not stamp its failures gets no
  // freshness check rather than a silently discarded reason.
  function streamFailureOverlay(failure, now = null) {
    if (!failure || !failure.message) return null;
    if (now != null && failure.at != null && now - failure.at > FAILURE_FRESH_MS) {
      return null;
    }
    // An owner transition remains hopeful viewer copy, but it is still an
    // authoritative transport verdict: hls.js and the application must not
    // retry that response behind the owner's handoff decision.
    const retryable = failure.code === "media_owner_transition" ||
      (hlsStartupResponseAction({ status: failure.status, code: failure.code }) === "retry" &&
        (failure.code === "startup_timeout" || failure.status === 503));
    // The node serving this session died and nothing can take it over. That
    // is neither "still preparing" nor a startup failure — it happens to a
    // viewer who was already watching, and the honest sentence says the
    // stream stopped and has to be reopened. `media_owner_lost` carries the
    // film position to reopen at; the player does not act on it yet, so the
    // overlay does not promise that it will.
    if (failure.code === "media_owner_lost") {
      return {
        title: "This stream stopped.",
        detail: "The server that was playing it is gone. Start it again to keep watching.",
        retryable: false,
      };
    }
    return {
      title: retryable
        ? "Still preparing this stream…"
        : "Playback failed to start.",
      detail: failure.message,
      retryable,
    };
  }

  // `promptUp` was `stallPrompt` until the surface contract deleted the flag:
  // what it always meant is "a prompt the viewer has to answer is on screen",
  // which is now a property of the presenter's surface rather than a second
  // copy of the same truth kept on PLAYER.
  function waitingOverlayAction({ started = false, promptUp = false }) {
    if (!started) return "ignore";
    return promptUp ? "preserve_prompt" : "buffer";
  }

  function subtitleBurnAction({ requiresBurn, deliveredRange = null }) {
    if (!requiresBurn) return "native";
    return ["dolby_vision", "hdr10", "hlg"].includes(
      String(deliveredRange || "").toLowerCase(),
    )
      ? "keep_hdr"
      : "burn";
  }

  // The contract's numbers, not a second copy of them: `hide_after_ms`,
  // `skip_seconds` and the coalesce window were prose in the fixture until
  // the shipped code started reading them.
  function contractTiming(name) {
    if (!(name in CONTRACT_TIMINGS)) throw new Error(`no contract timing ${name}`);
    return CONTRACT_TIMINGS[name];
  }

  function contractStep(name) {
    if (!(name in CONTRACT_STEPS)) throw new Error(`no contract step ${name}`);
    return CONTRACT_STEPS[name];
  }

  function seekDeltaSeconds(key) {
    switch (key) {
      case "ArrowLeft":
        return -contractStep("skip_seconds");
      case "ArrowRight":
        return contractStep("skip_seconds");
      default:
        return null;
    }
  }

  // A quiet timer is useful for batching repeats, but quiet is not release:
  // desktop platforms commonly wait longer than the batching window before
  // emitting the first repeated keydown. This owner keeps the timer and the
  // physical key lifecycle together so a held gesture commits exactly once.
  function createHeldKeyCommitter({ delayMs, commit, setTimer = setTimeout, clearTimer = clearTimeout }) {
    if (!(delayMs >= 0)) throw new Error("held-key delay must be non-negative");
    if (typeof commit !== "function") throw new Error("held-key commit callback is required");
    const keys = new Set();
    let owner = null;
    let timer = null;
    let quietElapsed = false;

    const clearScheduled = () => {
      if (timer != null) clearTimer(timer);
      timer = null;
    };
    const reset = () => {
      clearScheduled();
      keys.clear();
      owner = null;
      quietElapsed = false;
    };
    const finish = () => {
      const committedOwner = owner;
      reset();
      if (committedOwner != null) commit(committedOwner);
    };
    const arm = () => {
      clearScheduled();
      timer = setTimer(() => {
        timer = null;
        if (owner == null) return;
        if (keys.size > 0) {
          quietElapsed = true;
          return;
        }
        finish();
      }, delayMs);
    };

    return Object.freeze({
      press(key, nextOwner) {
        if (!key || nextOwner == null) return false;
        if (owner !== nextOwner) {
          reset();
          owner = nextOwner;
        }
        keys.add(key);
        quietElapsed = false;
        arm();
        return true;
      },
      release(key, nextOwner) {
        if (owner !== nextOwner || !keys.delete(key)) return false;
        if (keys.size === 0 && quietElapsed) finish();
        return true;
      },
      blur(nextOwner) {
        if (owner !== nextOwner) return false;
        finish();
        return true;
      },
      cancel() {
        const active = owner != null;
        reset();
        return active;
      },
      active(nextOwner) {
        return owner === nextOwner;
      },
    });
  }

  function routeWatchInput(surface, state, input, presentation, chrome = "overlay") {
    const row = WATCH_INPUT_ROUTING[presentation]?.[chrome]?.[surface]?.[state];
    if (!row || !(input in row)) throw new Error(`no watch route for ${presentation}/${chrome}/${surface}/${state}/${input}`);
    return row[input];
  }

  function resolveWatchOutcome(outcome, browserMounted) {
    return outcome === "return_browser" && !browserMounted ? "exit" : outcome;
  }

  function routeInput(surface, state, input) {
    const row = INPUT_ROUTING[surface] && INPUT_ROUTING[surface][state];
    if (!row || !(input in row)) {
      throw new Error(`no route for ${surface}/${state}/${input}`);
    }
    return row[input];
  }

  // Live TV is a surface without a timeline: the same rulings, minus every
  // seek. Routed from the same fixture as the finite player so the web, Apple
  // and Android reducers cannot drift from one another.
  function routeLiveInput(surface, state, input) {
    const row = LIVE_INPUT_ROUTING[surface] && LIVE_INPUT_ROUTING[surface][state];
    if (!row || !(input in row)) {
      throw new Error(`no live route for ${surface}/${state}/${input}`);
    }
    return row[input];
  }

  function liveContractTiming(name) {
    if (!(name in LIVE_CONTRACT_TIMINGS)) throw new Error(`unknown live timing ${name}`);
    return LIVE_CONTRACT_TIMINGS[name];
  }

  // A desktop hotkey is only ours while the live host owns the keyboard. The
  // caller decides that; this only answers what the key means.
  function liveHotkey(key) {
    const name = String(key || "").toLowerCase();
    return Object.prototype.hasOwnProperty.call(LIVE_HOTKEYS, name) ? LIVE_HOTKEYS[name] : null;
  }

  function previewStepSeconds(repeatCount) {
    let step = contractStep("preview_step_seconds");
    for (const rung of contractStep("preview_acceleration")) {
      if (repeatCount >= rung.from_repeat) step = rung.step_seconds;
    }
    return step;
  }

  function lostFrameRate(hitches, playedSeconds) {
    if (!hitches || !(playedSeconds > 0)) return null;
    const lost =
      (hitches.back | 0) + (hitches.drop | 0) + (hitches.gap | 0);
    return { lost, rate: +(lost / (playedSeconds / 60)).toFixed(1) };
  }

  function decodeMarginVerdict(hitches, playedSeconds, thresholds = DEFAULTS) {
    if (!(playedSeconds >= thresholds.minimumSeconds)) return null;
    const result = lostFrameRate(hitches, playedSeconds);
    if (
      !result ||
      result.lost < thresholds.minimumLost ||
      result.rate < thresholds.lostPerMinute
    ) {
      return null;
    }
    const budgetMs = hitches.fps ? +(1000 / hitches.fps).toFixed(1) : null;
    return {
      lost: result.lost,
      rate: result.rate,
      secs: Math.round(playedSeconds),
      decodeMs:
        hitches.decodeMs == null ? null : +hitches.decodeMs.toFixed(1),
      budgetMs,
    };
  }

  function normalizedText(value, fallback = "unknown") {
    const text = value == null
      ? ""
      : String(value).trim().toLowerCase().replace(/\s+/g, " ");
    return text || fallback;
  }

  function positiveInteger(value) {
    const number = Number(value);
    return Number.isFinite(number) && number > 0
      ? Math.trunc(Math.min(number, Number.MAX_SAFE_INTEGER))
      : 0;
  }

  function sourceDynamicRange(source = {}) {
    const declared = normalizedText(source && source.hdr, "");
    if (declared) return declared;
    const rich = normalizedText(source && source.hdr_format, "");
    if (/dolby vision|\bdv\b/.test(rich)) return "dolby_vision";
    if (/hdr10/.test(rich)) return "hdr10";
    if (/\bhlg\b/.test(rich)) return "hlg";
    return "sdr_or_unknown";
  }

  function bitrateBucket(
    bitrateBps,
    bucketBps = DECODE_LIMIT_DEFAULTS.bitrateBucketBps,
  ) {
    const bitrate = Number(bitrateBps);
    const width = Number(bucketBps);
    if (!(bitrate > 0) || !Number.isFinite(bitrate) || !(width > 0)) {
      return null;
    }
    const index = Math.floor(
      Math.min(bitrate, Number.MAX_SAFE_INTEGER) / width,
    );
    return {
      index,
      lower_bps: index * width,
      upper_bps: (index + 1) * width,
    };
  }

  // A learned verdict describes one decoder load, not every file sharing a
  // codec and height. The schema prefix deliberately makes the old
  // `codec@height` keys unreadable instead of pretending they were precise
  // enough to migrate. JSON over a fixed-order array is collision-free and
  // deterministic even when a future source string contains our separators.
  function decodeLimitIdentity(source = {}) {
    if (!source || typeof source !== "object" || Array.isArray(source)) {
      return null;
    }
    const bucket = bitrateBucket(source.bitrate);
    const fields = [
      normalizedText(source.video_codec),
      normalizedText(source.video_profile),
      positiveInteger(source.width),
      positiveInteger(source.height),
      positiveInteger(source.bit_depth),
      sourceDynamicRange(source),
      normalizedText(source.hdr_format, "none"),
      bucket ? bucket.index : "unknown",
    ];
    return `decode-${DECODE_LIMIT_DEFAULTS.schema}:${JSON.stringify(fields)}`;
  }

  function dynamicRangeLabel(value) {
    const normalized = normalizedText(value, "unknown");
    const labels = {
      dolby_vision: "Dolby Vision",
      hdr10: "HDR10",
      hlg: "HLG",
      sdr: "SDR",
      sdr_or_unknown: "SDR or unknown range",
      unknown: "unknown range",
    };
    return labels[normalized] || String(value || "unknown range").trim();
  }

  function mediaLoadLabel(source = {}) {
    const codec = normalizedText(source && source.video_codec).toUpperCase();
    const profile = normalizedText(source && source.video_profile, "");
    const width = positiveInteger(source && source.width);
    const height = positiveInteger(source && source.height);
    const depth = positiveInteger(source && source.bit_depth);
    const richRange = source && source.hdr_format != null
      ? String(source.hdr_format).trim()
      : "";
    const bucket = bitrateBucket(source && source.bitrate);
    const facts = [codec];
    if (profile) facts.push(profile);
    facts.push(width && height ? `${width}\u00d7${height}` : "unknown resolution");
    facts.push(depth ? `${depth}-bit` : "unknown bit depth");
    facts.push(richRange || dynamicRangeLabel(sourceDynamicRange(source)));
    facts.push(bucket
      ? `${bucket.lower_bps / 1_000_000}\u2013<${bucket.upper_bps / 1_000_000} Mb/s`
      : "unknown bitrate");
    return facts.join(" \u00b7 ");
  }

  function isDecodeLimitIdentity(key) {
    const prefix = `decode-${DECODE_LIMIT_DEFAULTS.schema}:`;
    if (typeof key !== "string" || !key.startsWith(prefix)) return false;
    try {
      const fields = JSON.parse(key.slice(prefix.length));
      return Array.isArray(fields) && fields.length === 8 &&
        key === `${prefix}${JSON.stringify(fields)}`;
    } catch (_) {
      return false;
    }
  }

  function pruneDecodeLimits(
    entries,
    {
      nowMs = 0,
      ttlMs = DECODE_LIMIT_DEFAULTS.ttlMs,
    } = {},
  ) {
    const source = entries && typeof entries === "object" && !Array.isArray(entries)
      ? entries
      : {};
    const limits = {};
    let changed = source !== entries;
    for (const [key, limit] of Object.entries(source)) {
      const at = Number(limit && limit.at);
      const rate = Number(limit && limit.rate);
      const age = Number(nowMs) - at;
      const valid =
        isDecodeLimitIdentity(key) &&
        at > 0 &&
        limit.rate != null &&
        Number.isFinite(rate) &&
        age >= 0 &&
        age <= ttlMs;
      if (valid) limits[key] = limit;
      else changed = true;
    }
    return { limits, changed };
  }

  // Pure lookup policy for the cached route. The caller owns localStorage and
  // the server request; this function says whether the exact media-load entry
  // may steer Auto, must be re-tested, or is bypassed by the viewer.
  function learnedDecodeLimitAction({
    quality = "auto",
    source = {},
    limits = {},
    nowMs = 0,
    ttlMs = DECODE_LIMIT_DEFAULTS.ttlMs,
    retestMs = DECODE_LIMIT_DEFAULTS.retestMs,
  } = {}) {
    const key = decodeLimitIdentity(source);
    if (!key) return { action: "none", key: null, limit: null };
    if (qualityForce(quality) !== "auto") {
      return { action: "bypass", key, limit: null };
    }
    const pruned = pruneDecodeLimits(limits, { nowMs, ttlMs }).limits;
    const limit = Object.prototype.hasOwnProperty.call(pruned, key)
      ? pruned[key]
      : null;
    if (!limit) return { action: "none", key, limit: null };
    return {
      action: Number(nowMs) - Number(limit.at) > retestMs ? "retest" : "apply",
      key,
      limit,
    };
  }

  function withoutLearnedDecodeLimit(entries, source) {
    const key = decodeLimitIdentity(source);
    const limits = entries && typeof entries === "object" && !Array.isArray(entries)
      ? { ...entries }
      : {};
    const removed = !!key && Object.prototype.hasOwnProperty.call(limits, key);
    if (removed) delete limits[key];
    return { limits, removed, key };
  }

  function learnedDecodeLimitView({
    source = {},
    limit = null,
    ordinaryRange = null,
    deliveredRange = null,
  } = {}) {
    const loadLabel = mediaLoadLabel(source);
    const measurement = limit && Number.isFinite(Number(limit.rate))
      ? `lost ${Number(limit.lost) || 0} frames in ${Number(limit.secs) || 0}s (${Number(limit.rate)}/min)`
      : "measured unstable original playback";
    const ordinary = normalizedText(ordinaryRange, "unknown");
    const delivered = normalizedText(deliveredRange, "unknown");
    const rangeConsequence =
      ["dolby_vision", "hdr10", "hlg"].includes(ordinary) && delivered === "sdr"
        ? `${dynamicRangeLabel(ordinary)} \u2192 SDR`
        : null;
    return {
      identity: decodeLimitIdentity(source),
      loadLabel,
      measurement,
      rangeConsequence,
      reason: `learned client-performance limit for ${loadLabel}: ${measurement}` +
        (rangeConsequence ? `; ${rangeConsequence}` : ""),
    };
  }

  // ---- The playback surface presenter -------------------------------------
  //
  // One pure reducer, fed the same ordered event sequences on every client
  // (tests/playback/playback-surface-contract.json). It has NO side effects:
  // it never pauses, resumes, reopens, cancels a timer or reports anything.
  // The recovery owner keeps every one of those powers, and gains exactly one
  // obligation — stop the player before raising a blocking fault, which the
  // fixture refuses to render without.
  //
  // docs/clients/PLAYBACK-SURFACE-CONTRACT.md §3.

  const SURFACE_BLOCKING_CLASSES = Object.freeze(
    Object.keys(SURFACE_CLASSES).filter((name) => SURFACE_CLASSES[name].blocking === true),
  );

  // Which timing bounds each class's "this much CONTINUOUS presentation" rule.
  const SURFACE_CONTINUOUS_TIMING = Object.freeze({
    refused: "refused_progress_ms",
    degraded: "disagreement_notice_ms",
  });

  // Only the owner's own stop promotes a fault that declared a successor class
  // (`media_owner_lost_410` → `stopped`). Any other blocking source is its own
  // fault with its own sentence and its own actions — a decoder failure
  // reported as a 410 is exactly the misattribution the ledger exists to
  // prevent, and an `auth_401_403` folded into a 410 loses its Sign in.
  const SURFACE_PROMOTING_SOURCES = Object.freeze(["owner_stopped", "owner_exhausted"]);

  // The input contract's `failed` state. A full-screen `preparing` covers the
  // picture but asks the viewer nothing, so it keeps today's routing; only a
  // prompt or a terminal — a blocking surface with an answer to give — enters
  // `failed` (PLAYBACK-SURFACE-CONTRACT.md §4).
  function surfaceEntersFailedRouting(surface) {
    if (!surface || surface.kind !== "blocking") return false;
    const cls = SURFACE_CLASSES[surface.class];
    if (!cls) return false;
    return cls.severity === "prompt" || cls.severity === "terminal";
  }

  function initialSurfaceState() {
    return {
      faults: [],
      attached: null,
      hidden: false,
      hiddenSince: null,
      presenting: false,
      presentingSince: null,
      now: 0,
      seq: 0,
    };
  }

  function surfaceStateCopy(state) {
    return {
      // `actions` is copied too: the render and the ledger both hold the
      // surface's fault, and one in-place sort or push would reach back into
      // every earlier state this reducer ever returned.
      faults: state.faults.map((fault) => ({ ...fault, actions: fault.actions.slice() })),
      attached: state.attached,
      hidden: state.hidden,
      hiddenSince: state.hiddenSince,
      presenting: state.presenting,
      presentingSince: state.presentingSince,
      now: state.now,
      seq: state.seq,
    };
  }

  function surfaceFaultLog(event, fault, extra) {
    return Object.assign(
      {
        event,
        class: fault.class,
        source: fault.source,
        attached: fault.attached,
        intent: fault.intent,
        position_ms: fault.positionMs,
        actions: fault.actions.slice(),
        player_stopped: fault.playerStopped,
      },
      extra || {},
    );
  }

  // Rows are scanned in order; the first whose id and context both match wins.
  function surfaceSourceRow(id, context) {
    let sawId = false;
    for (const row of SURFACE_SOURCES) {
      if (row.id !== id) continue;
      sawId = true;
      if (row.context === "any" || row.context === context) return { row };
    }
    return { error: sawId ? "source_context_mismatch" : "unknown_source" };
  }

  function surfaceDropFaults(state, log, predicate, by, extra) {
    const kept = [];
    for (const fault of state.faults) {
      if (predicate(fault)) {
        log.push(surfaceFaultLog("surface_cleared", fault, Object.assign({ by }, extra || {})));
      } else {
        kept.push(fault);
      }
    }
    state.faults = kept;
  }

  // Each retirement reason is a property of the CLASS, not of the event that
  // carries it: a prompt the viewer has to answer is not swept away because a
  // seek happened to land underneath it.
  function surfaceRetiredBy(fault, reason) {
    const cls = SURFACE_CLASSES[fault.class];
    const row = SURFACE_SOURCES.find((candidate) => candidate.id === fault.source);
    const retiredBy = row && Array.isArray(row.retired_by)
      ? row.retired_by
      : cls && cls.retired_by || [];
    return retiredBy.includes(reason);
  }

  // `presenting_after_raise` only. The run of presentation has to have BEGUN at
  // or after the fault was raised: the picture that was already on screen when
  // the server said "held" is not proof the hold is over, and an owner that
  // reopens in place produces exactly this, which is why `recovering` does not
  // need a new generation to be retired.
  //
  // Plain `presenting` is NOT gated by it, and gating it was a bug: a picture
  // that is presenting is not buffering and is not preparing, whenever its run
  // began. Safari fires `waiting` at every fMP4 boundary on healthy 4K, so a
  // `media_waiting` raised over a picture that never stopped had no evidence
  // that could ever postdate it and the spinner stayed up for the rest of the
  // film. The classes that need the stronger proof say so in `retired_by`.
  function surfaceEvidencePostdates(state, fault) {
    return state.presentingSince != null && state.presentingSince >= fault.raisedAt;
  }

  function surfaceContinuousElapsed(state, fault) {
    // Continuous means continuous: a picture that is not presenting right now
    // has a run length of nothing, whatever it did earlier.
    if (!state.presenting || state.presentingSince == null) return null;
    return state.now - Math.max(state.presentingSince, fault.raisedAt);
  }

  function surfaceSweep(state, log) {
    // A hidden page samples nothing and expires nothing. The clock it is
    // measured against is rewound when the page comes back (see `hidden`).
    if (state.hidden) return;
    const now = state.now;
    const drop = [];
    for (const fault of state.faults) {
      const cls = SURFACE_CLASSES[fault.class];
      if (!cls) continue;
      const row = SURFACE_SOURCES.find((candidate) => candidate.id === fault.source);
      const retiredBy = row && Array.isArray(row.retired_by)
        ? row.retired_by
        : cls.retired_by || [];
      if (
        cls.timed_ms != null &&
        retiredBy.includes("timer") &&
        !(cls.timer_paused_while_actions && fault.actions.length > 0) &&
        now - fault.raisedAt >= cls.timed_ms
      ) {
        drop.push([fault, "timer"]);
        continue;
      }
      if (retiredBy.includes("presenting_continuous_ms")) {
        const bound = SURFACE_TIMINGS[SURFACE_CONTINUOUS_TIMING[fault.class]];
        const elapsed = surfaceContinuousElapsed(state, fault);
        if (bound != null && elapsed != null && elapsed >= bound) {
          drop.push([fault, "presenting"]);
          continue;
        }
      }
      if (fault.intent != null) continue; // evidence never retires a pending destination
      if (!state.presenting) continue;
      if (retiredBy.includes("presenting")) {
        drop.push([fault, "presenting"]);
        continue;
      }
      if (retiredBy.includes("presenting_after_raise") && surfaceEvidencePostdates(state, fault)) {
        drop.push([fault, "presenting"]);
      }
    }
    if (drop.length === 0) return;
    const reason = new Map(drop);
    const kept = [];
    for (const fault of state.faults) {
      if (reason.has(fault)) {
        log.push(surfaceFaultLog("surface_cleared", fault, { by: reason.get(fault) }));
      } else {
        kept.push(fault);
      }
    }
    state.faults = kept;
  }

  // The agreement rule (§3.2): a blocking surface over a moving picture is a
  // disagreement, and the picture wins. The fault keeps its actions and its
  // data — a `media_owner_lost` still carries its position and its Try again —
  // so when the buffer drains the viewer gets the specific recovery.
  function surfaceResolveDisagreement(state, log, generation) {
    for (const fault of state.faults) {
      if (!SURFACE_BLOCKING_CLASSES.includes(fault.class)) continue;
      if (fault.attached !== generation) continue;
      log.push(surfaceFaultLog("surface_disagreement", fault, { by: "presenting" }));
      fault.class = "degraded";
      fault.demoted = true;
      fault.title = "Playback recovered";
      fault.raisedAt = state.now;
    }
  }

  function surfaceDrawable(state, fault) {
    const cls = SURFACE_CLASSES[fault.class];
    if (!cls) return false;
    if (cls.min_ms != null && state.now - fault.raisedAt < cls.min_ms) return false;
    return true;
  }

  function surfaceKindFor(state, fault) {
    const cls = SURFACE_CLASSES[fault.class];
    if (cls.blocking === true) return "blocking";
    if (cls.blocking === "while_not_presenting") return state.presenting ? "indicator" : "blocking";
    return "banner";
  }

  const SURFACE_NONE = Object.freeze({
    kind: "none",
    class: null,
    source: null,
    title: null,
    detail: null,
    actions: Object.freeze([]),
    attached: null,
    intent: null,
    position_ms: null,
    player_stopped: false,
    input_failed: false,
    fault: null,
  });

  function surfaceFrom(state) {
    let chosen = null;
    let chosenRank = -1;
    for (const fault of state.faults) {
      if (!surfaceDrawable(state, fault)) continue;
      const cls = SURFACE_CLASSES[fault.class];
      const rank = SURFACE_SEVERITY_RANK[cls.severity] || 0;
      // Highest severity owns the surface; equal severity breaks by recency.
      if (chosen == null || rank > chosenRank || (rank === chosenRank && fault.seq > chosen.seq)) {
        chosen = fault;
        chosenRank = rank;
      }
    }
    if (!chosen) return SURFACE_NONE;
    const surface = {
      kind: surfaceKindFor(state, chosen),
      class: chosen.class,
      source: chosen.source,
      title: chosen.title,
      detail: chosen.detail,
      actions: Object.freeze(chosen.actions.slice()),
      attached: chosen.attached,
      intent: chosen.intent,
      position_ms: chosen.positionMs,
      player_stopped: chosen.playerStopped,
      // A frozen copy: the render and the ledger read this, and neither may
      // reach into the reducer's own state through it.
      fault: Object.freeze({ ...chosen, actions: Object.freeze(chosen.actions.slice()) }),
    };
    surface.input_failed = surfaceEntersFailedRouting(surface);
    return Object.freeze(surface);
  }

  function presentSurface(state, event) {
    const next = surfaceStateCopy(state || initialSurfaceState());
    const log = [];
    if (event && typeof event.t === "number") next.now = event.t;
    const now = next.now;

    if (event && Object.prototype.hasOwnProperty.call(event, "event")) {
      // Inert by contract: canplay, playing, isPlayingChanged, timeControlStatus.
      // They prompt a look; they are not evidence, and they never move a
      // surface — not even by letting a timer that is due run.
      return { state: next, surface: surfaceFrom(next), log };
    }

    if (event && Object.prototype.hasOwnProperty.call(event, "attach")) {
      const generation = event.attach;
      surfaceDropFaults(next, log, (fault) => fault.attached !== generation, "attached_retired");
      next.attached = generation;
      next.presenting = false;
      next.presentingSince = null;
    } else if (event && Object.prototype.hasOwnProperty.call(event, "retire")) {
      const generation = event.retire;
      surfaceDropFaults(next, log, (fault) => fault.attached === generation, "attached_retired");
      if (next.attached === generation) {
        next.attached = null;
        next.presenting = false;
        next.presentingSince = null;
      }
    } else if (event && Object.prototype.hasOwnProperty.call(event, "hidden")) {
      const hidden = !!event.hidden;
      if (hidden && !next.hidden) {
        next.hiddenSince = now;
      } else if (!hidden && next.hidden) {
        // Carry the hidden interval forward rather than letting wall time run
        // under a frozen surface: a 30 s hold the viewer backgrounded for a
        // minute has not been on screen for 30 s. Nothing sampled the picture
        // while the page was away, so the continuous-presenting clock restarts.
        const away = next.hiddenSince == null ? 0 : now - next.hiddenSince;
        if (away > 0) for (const fault of next.faults) fault.raisedAt += away;
        next.hiddenSince = null;
        next.presenting = false;
        next.presentingSince = null;
      }
      next.hidden = hidden;
    } else if (event && Object.prototype.hasOwnProperty.call(event, "presenting")) {
      if (!next.hidden && event.attached === next.attached) {
        if (event.presenting) {
          if (!next.presenting) {
            next.presenting = true;
            next.presentingSince = now;
          }
          surfaceResolveDisagreement(next, log, event.attached);
        } else {
          next.presenting = false;
          next.presentingSince = null;
        }
      }
    } else if (event && Object.prototype.hasOwnProperty.call(event, "system_paused")) {
      if (event.system_paused) {
        surfaceDropFaults(next, log, (fault) => fault.source === "system_interruption", "system_resumed");
        const found = surfaceSourceRow("system_interruption", "attached");
        if (!found.error && next.attached != null) {
          next.seq += 1;
          const fault = {
            class: found.row.class,
            source: found.row.id,
            attached: next.attached,
            intent: null,
            raisedAt: now,
            positionMs: null,
            title: "Paused — call in progress",
            detail: "Paused — audio interrupted",
            actions: [],
            playerStopped: false,
            thenWhenStopped: null,
            demoted: false,
            seq: next.seq,
          };
          next.faults.push(fault);
          log.push(surfaceFaultLog("surface_raised", fault));
        }
      } else {
        surfaceDropFaults(
          next,
          log,
          (fault) => fault.source === "system_interruption" && surfaceRetiredBy(fault, "system_resumed"),
          "system_resumed",
        );
      }
    } else if (event && Object.prototype.hasOwnProperty.call(event, "raise")) {
      const found = surfaceSourceRow(event.raise, event.context);
      if (found.error) {
        log.push({ event: "surface_error", error: found.error, source: event.raise, context: event.context || null });
      } else if (found.row.class == null) {
        log.push({ event: "surface_log_only", source: found.row.id, attached: event.attached ?? null });
      } else {
        const cls = SURFACE_CLASSES[found.row.class];
        const playerStopped = !!event.player_stopped;
        if (cls.blocking === true && !playerStopped) {
          log.push({
            event: "surface_error",
            error: "blocking_without_stop",
            source: found.row.id,
            class: found.row.class,
            attached: event.attached ?? null,
          });
        } else {
          const attached = event.attached ?? null;
          const rowActions = Array.isArray(event.actions)
            ? event.actions.slice()
            : Array.isArray(found.row.actions)
              ? found.row.actions.slice()
              : Array.isArray(cls.default_actions)
                ? cls.default_actions.slice()
                : [];
          const promoted = SURFACE_PROMOTING_SOURCES.includes(found.row.id)
            ? next.faults.find(
                (fault) =>
                  fault.attached === attached &&
                  fault.thenWhenStopped === found.row.class &&
                  // Defence for a future row: no blocking source declares a
                  // successor today, so a demoted fault cannot reach here, and
                  // a fault that already promoted has had its successor cleared.
                  !fault.demoted,
              )
            : null;
          if (promoted) {
            promoted.class = found.row.class;
            promoted.playerStopped = playerStopped;
            promoted.raisedAt = now;
            promoted.thenWhenStopped = null;
            if (event.title != null) promoted.title = event.title;
            if (event.detail != null) promoted.detail = event.detail;
            if (Array.isArray(event.actions)) {
              promoted.actions = event.actions.slice();
            } else if (promoted.actions.length === 0) {
              promoted.actions = rowActions;
            }
            log.push(surfaceFaultLog("surface_raised", promoted, { by: "owner_stopped" }));
          } else {
            next.seq += 1;
            const fault = {
              class: found.row.class,
              source: found.row.id,
              attached,
              intent: event.intent ?? null,
              raisedAt: now,
              positionMs: event.position_ms ?? null,
              title: event.title ?? cls.title ?? null,
              detail: event.detail ?? null,
              actions: rowActions,
              playerStopped,
              thenWhenStopped: found.row.then_when_stopped || null,
              demoted: false,
              seq: next.seq,
            };
            next.faults.push(fault);
            log.push(surfaceFaultLog("surface_raised", fault));
          }
          // A blocking fault raised over a picture that is presenting is a
          // disagreement the moment it is raised, not whenever the next
          // evidence sample happens to arrive.
          if (cls.blocking === true && next.presenting && next.attached === attached) {
            surfaceResolveDisagreement(next, log, attached);
          }
        }
      }
    } else if (event && Object.prototype.hasOwnProperty.call(event, "intent_settled")) {
      surfaceDropFaults(
        next,
        log,
        (fault) => fault.intent === event.intent_settled && surfaceRetiredBy(fault, "intent_settled"),
        "intent_settled",
      );
    } else if (event && Object.prototype.hasOwnProperty.call(event, "intent_superseded")) {
      surfaceDropFaults(
        next,
        log,
        (fault) => fault.intent === event.intent_superseded && surfaceRetiredBy(fault, "intent_superseded"),
        "intent_superseded",
      );
    } else if (event && Object.prototype.hasOwnProperty.call(event, "playback_requested")) {
      // A `buffering` fault is about a player that WANTS media: the wait is
      // only a wait while something is trying to play. A viewer who pauses no
      // longer wants it, so the fault is about nothing and is retired — and it
      // has to be retired by THIS, because the only other thing that retires it
      // is presentation evidence, and a paused picture never produces another
      // sample. That is how a spinner came to sit over a still frame until the
      // generation changed: the overlay outliving the thing it described, which
      // is the defect this whole contract exists to kill.
      //
      // Only classes that NAME the reason are retired, which today is
      // `buffering` alone: a `preparing` start has not been paused by a viewer
      // who has not seen it yet, and a blocking prompt is answered by the
      // viewer, not by a transport change. Resuming raises nothing back — the
      // raise sites decide what comes back, exactly as they do after every
      // other retirement.
      if (event.playback_requested === false) {
        surfaceDropFaults(
          next,
          log,
          (fault) => surfaceRetiredBy(fault, "playback_not_requested"),
          "playback_not_requested",
        );
      }
    } else if (event && Object.prototype.hasOwnProperty.call(event, "owner_success")) {
      // One recovery owner per player: its success retires every recovering
      // fault, not only the ones about the generation it replaced.
      surfaceDropFaults(next, log, (fault) => surfaceRetiredBy(fault, "owner_success"), "owner_success");
    } else if (event && Object.prototype.hasOwnProperty.call(event, "user_action")) {
      const action = event.user_action;
      const current = surfaceFrom(next).fault;
      const target =
        current && current.actions.includes(action)
          ? next.faults.find((fault) => fault.seq === current.seq)
          : next.faults.find((fault) => fault.actions.includes(action));
      if (target) {
        surfaceDropFaults(next, log, (fault) => fault === target, "user", { action });
      }
    }

    surfaceSweep(next, log);
    return { state: next, surface: surfaceFrom(next), log };
  }

  return Object.freeze({
    DEFAULTS,
    AUTO_DEFAULTS,
    DECODE_LIMIT_DEFAULTS,
    qualityForce,
    controlLeaseMode,
    controlLeasePresentation,
    transcodeHeight,
    normalizedLadder,
    initialAutoRung,
    bandwidthSeedBps,
    transferSampleKbps,
    recordStallEpisode,
    playerPixelHeight,
    decideRung,
    sessionHeight,
    nativeHlsAvailable,
    hlsTransport,
    copyAudioNeedsTranscode,
    initialRoute,
    indexPendingFallback,
    fallbackAction,
    stallRecoveryAction,
    stallRecoveryTargetHeight,
    stallReopenSessionOptions,
    fallbackResetBeforeOpen,
    parseStreamFailure,
    classifyStreamFailure,
    CREATE_RETRY,
    createRetryStep,
    HLS_RETRY,
    hlsRetryAllowed,
    HLS_MEDIA_RECOVERY,
    hlsMediaFatalAction,
    SEEK_LOCAL_SETTLE_MS,
    seekRoute,
    HLS_STARTUP,
    HLS_STARTUP_TERMINAL_CODES,
    STREAM_FAILURE_BODY_MAX_CHARS,
    STREAM_FAILURE_MESSAGE_MAX_CHARS,
    hlsStartupResponseAction,
    hlsStartupSendAction,
    BEHIND_LIVE_WINDOW_CODE,
    behindLiveWindowRecovers,
    SURFACE_CLASSES,
    SURFACE_SOURCES,
    SURFACE_TIMINGS,
    SURFACE_SEVERITY_RANK,
    initialSurfaceState,
    presentSurface,
    surfaceEntersFailedRouting,
    streamFailureOverlay,
    waitingOverlayAction,
    subtitleBurnAction,
    INPUT_ROUTING,
    WATCH_INPUT_ROUTING,
    routeWatchInput,
    resolveWatchOutcome,
    contractTiming,
    contractStep,
    routeInput,
    LIVE_INPUT_ROUTING,
    routeLiveInput,
    liveContractTiming,
    liveHotkey,
    createHeldKeyCommitter,
    seekDeltaSeconds,
    previewStepSeconds,
    lostFrameRate,
    decodeMarginVerdict,
    bitrateBucket,
    decodeLimitIdentity,
    mediaLoadLabel,
    pruneDecodeLimits,
    learnedDecodeLimitAction,
    withoutLearnedDecodeLimit,
    learnedDecodeLimitView,
  });
});
