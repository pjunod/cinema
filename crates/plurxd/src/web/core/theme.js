// Theme engine. Runs in <head> before first paint so there's no flash of the
// wrong theme. A theme is a palette of CSS variables with a dark and light
// variant; appearance (auto/dark/light) picks the variant, auto following the
// OS and falling back to dark. Persisted in localStorage.
"use strict";
// Display name only — the on-screen product brand. Internal names (crates,
// binary, PLURX_* env, data paths) are unchanged. Change this one line to
// rebrand the whole UI.
const APP_NAME="cinema";
const FONT_UI="system-ui,-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif";
// JetBrains Mono (500/700) + Inter are embedded above as data-URL @font-face,
// so every client gets the real brand type; the rest of each stack is fallback.
const FONT_MONO="'JetBrains Mono',ui-monospace,SFMono-Regular,Menlo,Consolas,'Liberation Mono',monospace";
const FONT_SYS_MONO="ui-monospace,SFMono-Regular,Menlo,Consolas,'Liberation Mono',monospace";
const FONT_INTER="'Inter',system-ui,-apple-system,'Helvetica Neue',Arial,sans-serif";
const THEMES={
  classic:{name:"Classic",
    dark:{"--bg":"#0e0f13","--panel":"#171922","--panel2":"#1e2230","--line":"#2a2f3e","--text":"#e8eaf0","--muted":"#9aa2b4","--accent":"#6ea8fe","--accent2":"#8b7cf6","--good":"#4ade80","--bad":"#f87171","--warn":"#fbbf24","--radius":"12px","--btn-ink":"#07101f","--prose":"#cdd2df","--font":FONT_UI,"--font-mono":FONT_SYS_MONO},
    light:{"--bg":"#f6f7f9","--panel":"#ffffff","--panel2":"#eef1f6","--line":"#dde2ea","--text":"#1a1d24","--muted":"#5b6472","--accent":"#2f6fe0","--accent2":"#6d5bd0","--good":"#147d47","--bad":"#c23e48","--warn":"#b45309","--radius":"12px","--btn-ink":"#ffffff","--prose":"#333a45","--font":FONT_UI,"--font-mono":FONT_SYS_MONO}},
  // Terminal — a unix box. dark: green phosphor on a truer console black,
  // ANSI-leaning status colors. light: Solarized (the classic editor/tty scheme).
  terminal:{name:"Terminal",
    dark:{"--bg":"#050705","--panel":"#0b100b","--panel2":"#111a12","--line":"#1e3020","--text":"#c9e8c0","--muted":"#7a9670","--accent":"#3fe170","--accent2":"#2aa858","--good":"#3fe170","--bad":"#ff7066","--warn":"#d9c25a","--radius":"0px","--btn-ink":"#041508","--prose":"#a9cba2","--font":FONT_MONO,"--font-mono":FONT_MONO},
    light:{"--bg":"#eee8d5","--panel":"#fdf6e3","--panel2":"#e6dfc8","--line":"#d5cdb4","--text":"#073642","--muted":"#657b83","--accent":"#6e7f00","--accent2":"#268bd2","--good":"#5c6900","--bad":"#ba2a27","--warn":"#7d5e00","--radius":"0px","--btn-ink":"#fdf6e3","--prose":"#586e75","--font":FONT_MONO,"--font-mono":FONT_MONO}},
  // noirr — the brand theme, exact kit tokens (brand/tokens.css): midnight +
  // matinee. Crimson cursor accent; warm paper by day; glow only at midnight.
  noirr:{name:"noirr",
    dark:{"--bg":"#0a0a0c","--panel":"#101014","--panel2":"#16161b","--line":"rgba(255,255,255,.08)","--text":"#ededef","--muted":"#9a9aa3","--accent":"#e5484d","--accent2":"#8e1c22","--good":"#5fb582","--bad":"#ff7a66","--warn":"#d9a05b","--radius":"10px","--btn-ink":"#ffffff","--prose":"#cdced2","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--glow":"rgba(229,72,77,.35)","--shadow":"0 4px 14px rgba(0,0,0,.5)","--grain":".045"},
    light:{"--bg":"#f2efe8","--panel":"#faf8f2","--panel2":"#ffffff","--line":"rgba(24,22,20,.12)","--text":"#1a1a1e","--muted":"#5d5c63","--accent":"#c2343a","--accent2":"#8e1c22","--good":"#307a54","--bad":"#aa5438","--warn":"#8d6425","--radius":"10px","--btn-ink":"#ffffff","--prose":"#33312e","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--glow":"rgba(194,52,58,.16)","--shadow":"0 2px 10px rgba(25,20,15,.12)","--grain":".03"}},
  // ---- the catalogue (UI-LAYOUTS-IMPLEMENTATION.md T1) --------------------
  // Every value below is a contract value: machine-checked at or above 4.5:1
  // for text/muted/prose/accent/good/warn/bad against --bg, --panel AND
  // --panel2, and for --btn-ink against --accent. `scripts/contrast-check`
  // re-runs the whole matrix; run it after touching any line in this object.
  //
  // --accent2 is deliberately outside that check. It is not a text token in
  // this app: it appears exactly twice, both times as the far end of a
  // gradient (the wordmark at .logo, and the badge below it), so measuring it
  // as ink on a page is the wrong test. What does matter for it is the white
  // label on that badge — noted per theme where it is tight.

  // Amber — charcoal + gold, the signature of the catalog layout. The light
  // accent/good/bad are NOT the mockup's values: those measured 2.73 / 3.92 /
  // 4.49 and are replaced here by the review's corrected set. Dark --bad is
  // ours: #e5534b passes on --bg but lands at 4.25 / 3.89 on the panels, and
  // status text mostly lives on cards.
  amber:{name:"Amber",
    dark:{"--bg":"#191a1d","--panel":"#212327","--panel2":"#2a2d32","--line":"#383c43","--text":"#eceef0","--muted":"#9aa0a7","--accent":"#e5a00d","--accent2":"#cc7b19","--good":"#52b788","--bad":"#ee7168","--warn":"#f2c14e","--radius":"8px","--btn-ink":"#1c1303","--prose":"#c9cdd2","--font":FONT_UI,"--font-mono":FONT_SYS_MONO},
    light:{"--bg":"#f3f4f6","--panel":"#ffffff","--panel2":"#e9ebee","--line":"#d5d9de","--text":"#1e2124","--muted":"#5f666d","--accent":"#8b5e00","--accent2":"#a95f16","--good":"#246b49","--bad":"#bd332d","--warn":"#8a6116","--radius":"8px","--btn-ink":"#ffffff","--prose":"#3a4046","--font":FONT_UI,"--font-mono":FONT_SYS_MONO}},
  // Giallo — the kit's reserved amber showtime. noirr's structure, amber heat,
  // grain a notch louder. Crimson is *correct* as the error colour here:
  // giallo posters are yellow and red.
  giallo:{name:"Giallo",
    dark:{"--bg":"#0c0a06","--panel":"#14100a","--panel2":"#1c160d","--line":"rgba(240,200,120,.10)","--text":"#f2e9d8","--muted":"#a89c85","--accent":"#e8a33d","--accent2":"#8a5a14","--good":"#5fb582","--bad":"#e5484d","--warn":"#c9723a","--radius":"10px","--btn-ink":"#1a1002","--prose":"#d6ccb8","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--glow":"rgba(232,163,61,.30)","--shadow":"0 4px 14px rgba(0,0,0,.55)","--grain":".055"},
    light:{"--bg":"#f5eed9","--panel":"#fbf7ea","--panel2":"#ffffff","--line":"rgba(90,70,30,.14)","--text":"#241d10","--muted":"#6e6350","--accent":"#7f4e00","--accent2":"#7c4a05","--good":"#2c734d","--bad":"#b23a35","--warn":"#8a6116","--radius":"10px","--btn-ink":"#ffffff","--prose":"#4a4233","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--glow":"rgba(168,114,12,.14)","--shadow":"0 2px 10px rgba(60,45,15,.14)","--grain":".035"}},
  // Silver — the kit's reserved B&W showtime. White is the accent at night,
  // ink is the accent by day. The statuses stay quietly tinted rather than
  // grey: a fully monochrome "failed" is an accessibility lie, because colour
  // is then the only channel that ever carried the meaning and it is gone.
  // Light --good is #467353, not the proposal's #4a7a58 — that one measured
  // 4.13 on --panel2, which is where pills and chips actually sit.
  silver:{name:"Silver",
    dark:{"--bg":"#0a0a0b","--panel":"#131315","--panel2":"#1b1b1e","--line":"rgba(255,255,255,.10)","--text":"#f2f2f2","--muted":"#9a9a9e","--accent":"#e8e8ea","--accent2":"#8a8a8f","--good":"#9fbfa8","--bad":"#d09088","--warn":"#c9b48c","--radius":"6px","--btn-ink":"#0a0a0b","--prose":"#cfcfd3","--font":FONT_INTER,"--font-mono":FONT_MONO},
    light:{"--bg":"#f4f4f2","--panel":"#ffffff","--panel2":"#eaeae7","--line":"rgba(20,20,20,.14)","--text":"#141416","--muted":"#66666a","--accent":"#1a1a1c","--accent2":"#5a5a5f","--good":"#467353","--bad":"#a05248","--warn":"#765f28","--radius":"6px","--btn-ink":"#ffffff","--prose":"#3a3a3e","--font":FONT_INTER,"--font-mono":FONT_MONO}},
  // Void — true black for OLED, and dark-only: an OLED theme that turns itself
  // into a white page on a light toggle is not the same product. Borders do
  // the structural work (--shadow:none — a shadow on #000 is invisible), and
  // the accent is electric so one colour still reads at minimum brightness.
  void:{name:"Void", darkOnly:true,
    dark:{"--bg":"#000000","--panel":"#0a0a0a","--panel2":"#131313","--line":"rgba(255,255,255,.13)","--text":"#e8e8e8","--muted":"#8a8a8a","--accent":"#4cc2ff","--accent2":"#2f6fe0","--good":"#34d399","--bad":"#f87171","--warn":"#fbbf24","--radius":"12px","--btn-ink":"#001018","--prose":"#c6c6c6","--font":FONT_UI,"--font-mono":FONT_MONO,
      "--shadow":"none"}},
  // VHS — tracking-line synthwave, dark-only for the same reason as Void.
  // Magenta accent with a cyan accent2: the one theme whose wordmark gradient
  // runs magenta to cyan. Its loading language is chunky tracking bars rather
  // than a shimmer — see [data-theme=vhs] in the stylesheet, and note that the
  // bars sit behind prefers-reduced-motion with a static bar as the fallback,
  // because "loading" is information and must not depend on movement.
  vhs:{name:"VHS", darkOnly:true,
    dark:{"--bg":"#140d22","--panel":"#1d1430","--panel2":"#291c42","--line":"rgba(255,120,220,.16)","--text":"#f4e9ff","--muted":"#a78fc7","--accent":"#ff4fd8","--accent2":"#26e2ff","--good":"#3ddc97","--bad":"#ff5c7a","--warn":"#ffb454","--radius":"14px","--btn-ink":"#22041c","--prose":"#d9c9ee","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--glow":"rgba(255,79,216,.35)","--shadow":"0 4px 18px rgba(0,0,0,.55)"}},
  // Paper — light-first editorial: warm stock, graphite text, ink-blue accent.
  // Its dark mode is a reading room, not an inverted white page. No grain, no
  // glow, no animated brand treatment, on purpose. Light --good is #26724c
  // rather than the review's #287a51 (4.09 on --panel2).
  paper:{name:"Paper",
    dark:{"--bg":"#171715","--panel":"#201f1c","--panel2":"#2a2823","--line":"#454139","--text":"#f2eee6","--muted":"#aaa49a","--accent":"#9db2ff","--accent2":"#e58b84","--good":"#6fc49a","--bad":"#ff8a80","--warn":"#e2c36f","--radius":"6px","--btn-ink":"#101322","--prose":"#d6d0c6","--font":FONT_INTER,"--font-mono":FONT_MONO},
    light:{"--bg":"#f4f0e8","--panel":"#fffdf8","--panel2":"#e9e2d7","--line":"#c8bfb2","--text":"#1f2328","--muted":"#5f625f","--accent":"#3451b2","--accent2":"#a33a3a","--good":"#26724c","--bad":"#b3261e","--warn":"#765c00","--radius":"6px","--btn-ink":"#ffffff","--prose":"#3d4248","--font":FONT_INTER,"--font-mono":FONT_MONO}},
  // Tide — sea-glass green on deep blue-green with a sand secondary. It exists
  // because the catalogue was otherwise red, gold, blue and magenta, with no
  // calm colour family anywhere in it, and it works in both appearances.
  tide:{name:"Tide",
    dark:{"--bg":"#071412","--panel":"#0d1e1b","--panel2":"#142a25","--line":"#2a4841","--text":"#e4f2ed","--muted":"#93aaa2","--accent":"#73d6b1","--accent2":"#d6b56d","--good":"#6fd39f","--bad":"#ff8a80","--warn":"#e5c875","--radius":"10px","--btn-ink":"#052019","--prose":"#c8dcd5","--font":FONT_INTER,"--font-mono":FONT_MONO},
    light:{"--bg":"#edf4f0","--panel":"#fbfdfa","--panel2":"#dfeae4","--line":"#bccfc5","--text":"#14201c","--muted":"#586a62","--accent":"#176b54","--accent2":"#8a631a","--good":"#196b48","--bad":"#b33a32","--warn":"#735b0b","--radius":"10px","--btn-ink":"#ffffff","--prose":"#34473f","--font":FONT_INTER,"--font-mono":FONT_MONO}},
  // Panoptic — a dark-only intelligence cockpit: cyan telemetry on smoked
  // glass, hairline instrument borders and quiet signal glow. Its visual
  // reference is geospatial, but this remains a media UI: theme paint only,
  // never decorative fake coordinates or controls that imply nonexistent data.
  panoptic:{name:"Panoptic", darkOnly:true,
    dark:{"--bg":"#0a0a0f","--panel":"#10131b","--panel2":"#171c25","--line":"rgba(138,232,255,.16)","--text":"#e8eaed","--muted":"#9eaab2","--accent":"#00d4ff","--accent2":"#5cffc6","--good":"#5ce1b4","--bad":"#ff8191","--warn":"#ffd178","--radius":"12px","--btn-ink":"#001014","--prose":"#c6d1d8","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--accent-dim":"rgba(0,212,255,.12)","--glow":"rgba(0,212,255,.34)","--glass":"rgba(12,14,22,.78)","--glass-strong":"rgba(9,12,18,.90)","--hairline":"rgba(138,232,255,.16)","--shadow":"0 12px 34px rgba(0,0,0,.58)"}},
  // Redline — Panoptic's graphite instrument panel in restrained crimson.
  // Interaction red remains separate from success, warning and failure colors,
  // so operational meaning never relies on interpreting one ambiguous hue.
  redline:{name:"Redline", darkOnly:true,
    dark:{"--bg":"#070708","--panel":"#111214","--panel2":"#191a1d","--line":"rgba(255,122,128,.17)","--text":"#f0eded","--muted":"#aaa1a3","--accent":"#ff5964","--accent2":"#d62e3d","--good":"#6ccf9a","--bad":"#ff9f70","--warn":"#f6c760","--radius":"12px","--btn-ink":"#170204","--prose":"#d4ccce","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--accent-dim":"rgba(255,89,100,.13)","--glow":"rgba(255,55,70,.32)","--glass":"rgba(16,13,15,.80)","--glass-strong":"rgba(10,9,11,.92)","--hairline":"rgba(255,122,128,.17)","--shadow":"0 12px 34px rgba(0,0,0,.62)"}},
  // Burnt Pumpkin — the original Panovic true-black cockpit, warmed from
  // tangerine into a deeper harvest signal. Existing `panovic` preferences
  // intentionally migrate in place to this refined palette.
  panovic:{name:"Burnt Pumpkin", darkOnly:true,
    dark:{"--bg":"#000000","--panel":"#181818","--panel2":"#242424","--line":"rgba(255,255,255,.06)","--text":"#e8e6e1","--muted":"#9a9aa0","--accent":"#e8871e","--accent2":"#81420b","--good":"#5fb582","--bad":"#ff7a66","--warn":"#d9a05b","--radius":"10px","--btn-ink":"#150b00","--prose":"#c8c6c1","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--accent-dim":"rgba(232,135,30,.12)","--glow":"rgba(232,135,30,.30)","--glass":"rgba(24,24,24,.88)","--glass-strong":"rgba(0,0,0,.94)","--hairline":"rgba(255,255,255,.06)","--shadow":"0 4px 14px rgba(0,0,0,.8)"}},
  // Copper — the same neutral true-black cockpit with a quieter, earthier
  // signal. Status colors stay identical so operational meaning is stable.
  copper:{name:"Copper", darkOnly:true,
    dark:{"--bg":"#000000","--panel":"#181818","--panel2":"#242424","--line":"rgba(255,255,255,.06)","--text":"#e8e6e1","--muted":"#9a9aa0","--accent":"#cf7643","--accent2":"#70402b","--good":"#5fb582","--bad":"#ff7a66","--warn":"#d9a05b","--radius":"10px","--btn-ink":"#160b06","--prose":"#c8c6c1","--font":FONT_INTER,"--font-mono":FONT_MONO,
      "--accent-dim":"rgba(207,118,67,.12)","--glow":"rgba(207,118,67,.28)","--glass":"rgba(24,24,24,.88)","--glass-strong":"rgba(0,0,0,.94)","--hairline":"rgba(255,255,255,.06)","--shadow":"0 4px 14px rgba(0,0,0,.8)"}},
};
// Favicons: the noirr theme wears the kit icon (icon-a — n + cursor, midnight
// in both showtimes); everything else gets a neutral play mark.
const FAVICONS={
  noirr:"data:image/svg+xml,%3Csvg%20xmlns=%22http://www.w3.org/2000/svg%22%20viewBox=%220%200%2064%2064%22%20width=%2264%22%20height=%2264%22%3E%3Crect%20width=%2264%22%20height=%2264%22%20rx=%2214%22%20fill=%22%230a0a0c%22/%3E%3Cpath%20d=%22M16%2024%20V46%20M16%2031.5%20Q16%2024%2023.5%2024%20H28.5%20Q36%2024%2036%2031.5%20V46%22%20fill=%22none%22%20stroke=%22%23ededef%22%20stroke-width=%226.5%22%20stroke-linecap=%22round%22/%3E%3Crect%20x=%2243%22%20y=%2240.5%22%20width=%2210%22%20height=%226%22%20rx=%221.5%22%20fill=%22%23e5484d%22/%3E%3C/svg%3E",
  default:"data:image/svg+xml,%3Csvg%20xmlns=%27http://www.w3.org/2000/svg%27%20viewBox=%270%200%2064%2064%27%3E%3Cdefs%3E%3ClinearGradient%20id=%27g%27%20x1=%270%27%20y1=%270%27%20x2=%271%27%20y2=%271%27%3E%3Cstop%20offset=%270%27%20stop-color=%27%236ea8fe%27/%3E%3Cstop%20offset=%271%27%20stop-color=%27%238b7cf6%27/%3E%3C/linearGradient%3E%3C/defs%3E%3Crect%20width=%2764%27%20height=%2764%27%20rx=%2714%27%20fill=%27%230e0f13%27/%3E%3Cpath%20d=%27M25%2020%20L47%2032%20L25%2044%20Z%27%20fill=%27url(%23g)%27/%3E%3C/svg%3E"
};
function resolveMode(app){
  if(app==="light"||app==="dark") return app;
  return (window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches)?"light":"dark";
}
let APPLIED_VARS=[];
function applyTheme(){
  let id=null, app=null;
  try{ id=localStorage.getItem("plurx_theme"); app=localStorage.getItem("plurx_appearance"); }catch(e){}
  if(!THEMES[id]) id="classic";
  const theme=THEMES[id];
  // A dark-only theme keeps its midnight palette whatever the
  // appearance toggle says, and the theme menu says so inline rather than
  // leaving the toggle looking broken. The rejected alternative — falling back
  // to a different theme in light mode — swaps the whole personality of the UI
  // on a control that claims to change brightness.
  const mode=theme.darkOnly?"dark":resolveMode(app||"auto");
  const vars=theme[mode]||theme.dark;
  const root=document.documentElement;
  // drop vars the previous theme set that this one doesn't (e.g. noirr's --glow)
  for(const k of APPLIED_VARS) if(!(k in vars)) root.style.removeProperty(k);
  for(const k in vars) root.style.setProperty(k, vars[k]);
  APPLIED_VARS=Object.keys(vars);
  root.dataset.theme=id; root.dataset.mode=mode;
  root.style.colorScheme=mode;   // native controls & scrollbars follow the room
  const fav=document.getElementById("favicon");
  if(fav) fav.href=FAVICONS[id]||FAVICONS.default;
}
applyTheme();
// Poster/icon size for the browse grids (per browser). Applied as a CSS var so
// every `.grid` reflows live — no re-render. Defined here, in the <head>, so the
// size is on the root element before first paint: a definition in the body
// script would not exist yet when this line runs, and the ReferenceError would
// take the rest of this script (title, theme-follows-system) down with it.
const POSTER_SIZES=[["s","Small","120px"],["m","Medium","150px"],["l","Large","186px"],["xl","Extra large","226px"]];
function posterSize(){ let v="s"; try{ v=localStorage.getItem("plurx_iconsize")||"s"; }catch(e){} return POSTER_SIZES.some(s=>s[0]===v)?v:"s"; }
function applyPosterSize(){ const s=POSTER_SIZES.find(x=>x[0]===posterSize()); document.documentElement.style.setProperty("--poster", s?s[2]:"150px"); }
applyPosterSize();
// Layout engine. A layout is structure — where navigation lives, how a page is
// composed, how dense it is. A theme is paint. The two stay orthogonal because
// components only ever read tokens, so every theme works on every layout.
//
// Declared here, in the <head>, for the same reason applyTheme() runs here: a
// layout applied after first paint flashes the classic chrome and then jumps.
// This object carries only what the pre-paint pass needs — a display name and
// the surfaces the layout can actually run on. The renderers are attached to
// these same objects by the body script (see "layout renderers" there), which
// is why nothing here dereferences .views.
const LAYOUTS={
  classic:{name:"Classic", surfaces:["desktop","mobile","tv"]},
  catalog:{name:"Catalog", surfaces:["desktop","mobile","tv"]},
  theater:{name:"Theater", surfaces:["desktop","mobile","tv"]},
};
// Which kind of surface this browser is, from viewport and input capability —
// never a user-agent string. UA sniffing is wrong the moment someone resizes a
// window, and TV browsers misreport their UA as a matter of routine.
function surfaceClass(){
  const mq=q=>{ try{ return !!(window.matchMedia&&window.matchMedia(q).matches); }catch(e){ return false; } };
  // No pointing device at all means a D-pad: a television or a set-top box.
  if(mq("(pointer: none)")) return "tv";
  const coarse=mq("(pointer: coarse)"), noHover=mq("(hover: none)");
  const w=window.innerWidth||1280;
  // A coarse pointer with no hover on a very wide viewport is a remote driving
  // a cursor, not a phone — nobody holds a 1280px-wide touchscreen.
  if(coarse&&noHover&&w>=1280) return "tv";
  if(coarse&&w<900) return "mobile";
  return "desktop";
}
// The layout in force right now. Two independent fallbacks, both silent and
// both to classic: an id we don't ship (a preference written by a newer build,
// or a hand-edited localStorage), and an id this surface can't run (deck on a
// phone). A stale preference must open a familiar app, never a blank shell.
function layoutId(){
  let id=null; try{ id=localStorage.getItem("plurx_layout"); }catch(e){}
  // Earlier builds saved the retired public name as the layout id. Migrate it
  // once so the rename does not reset an existing browser to Classic.
  if(id==="plex"){
    id="catalog";
    try{ localStorage.setItem("plurx_layout",id); }catch(e){}
  }
  const L=LAYOUTS[id];
  if(!L) return "classic";
  if(L.surfaces&&L.surfaces.indexOf(surfaceClass())<0) return "classic";
  return id;
}
// Layouts scope their structural CSS under [data-layout=…] — the same shape the
// themes use for [data-theme=…]. Setting it before first paint is the point.
function applyLayout(){ document.documentElement.dataset.layout=layoutId(); }
applyLayout();
try{ document.title=`Noirr ${APP_NAME[0].toUpperCase()}${APP_NAME.slice(1)}`; }catch(e){}
if(window.matchMedia){ try{ window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change",()=>{ if((localStorage.getItem("plurx_appearance")||"auto")==="auto") applyTheme(); }); }catch(e){} }
