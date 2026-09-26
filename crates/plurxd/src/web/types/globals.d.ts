// Globals the web shell's plain script rows name but do not declare.
//
// Read by `tsc` only (scripts/web-types, docs/clients/WEB-TYPE-CHECKING-AND-
// PLAYER-DECOMPOSITION.md §3.2). Never served, never bundled.
//
// The sidecars and hls.min.js are UMD modules: TypeScript would read each as a
// CommonJS file and hide the global it assigns, so they stay out of
// jsconfig.json's `files` and their globals are declared here instead, typed
// `any` on purpose — typing them is a later ratchet, not this file's job.
// A `declare var` in a global script is also a property of `window`, which is
// how the body rows reach most of them (`window.PlurxPlaybackPolicy`).

/** hls.js, vendored at /assets/hls.min.js. */
declare var Hls: any;

/** The UMD sidecars (`root.Plurx… = api`). */
declare var PlurxClusterPanel: any;
declare var PlurxPlaybackPolicy: any;
declare var PlurxPlaybackControl: any;
declare var PlurxReaderCore: any;
declare var PlurxLiveTv: any;
declare var PlurxLibraryChannels: any;

/** Native bridges the Apple and Android shells inject into the page. */
declare var webkit: any;
declare var CinemaNative: any;

/** Assigned by pages/reader.js for the native shells to call. */
interface Window {
  startNativeReader?: any;
}
