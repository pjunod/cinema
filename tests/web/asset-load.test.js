"use strict";

// Load the whole web app the way a browser does, and see that it survives.
//
// `tests/web/asset-order.test.js` reads the code and refuses an order that
// cannot resolve. This does the other half: it evaluates the sidecars, the
// `<head>` script and all sixty body rows in served order in one `vm` context
// and checks that the app is actually there afterwards — `render` is callable
// and the three layouts registered themselves.
//
// The globals below are a browser stub, and deliberately NOT a Proxy: an
// identifier this context does not define throws a ReferenceError, which is the
// entire point. When one does, the failure says whether the missing name is a
// later row's binding (an ordering bug) or something the browser provides that
// this stub does not (add it here).

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const {shellSource, WEB} = require("./shell-source.js");
const {analyze} = require("./asset-graph.js");

// A CSSStyleDeclaration is a bag of string properties that also answers a few
// methods; anything unknown reads as the empty string, which is what an unset
// CSS property does.
const STYLE_METHODS = new Set([
  "setProperty", "removeProperty", "getPropertyValue", "getPropertyPriority", "item",
]);

function styleStub() {
  return new Proxy({}, {
    get: (target, prop) => {
      if (STYLE_METHODS.has(prop)) return () => "";
      return Reflect.has(target, prop) ? target[prop] : "";
    },
    set: (target, prop, value) => {
      target[prop] = value;
      return true;
    },
  });
}

function element(id) {
  const node = {
    id,
    tagName: "DIV",
    nodeName: "DIV",
    innerHTML: "",
    outerHTML: "",
    textContent: "",
    value: "",
    checked: false,
    disabled: false,
    hidden: false,
    isConnected: true,
    children: [],
    childNodes: [],
    parentNode: null,
    firstChild: null,
    scrollTop: 0,
    scrollHeight: 0,
    clientHeight: 0,
    offsetHeight: 0,
    offsetWidth: 0,
    currentTime: 0,
    duration: 0,
    paused: true,
    muted: false,
    volume: 1,
    playbackRate: 1,
    readyState: 0,
    buffered: {length: 0, start: () => 0, end: () => 0},
    textTracks: {length: 0, addEventListener() {}, removeEventListener() {}},
    style: styleStub(),
    dataset: {},
    classList: {
      add() {}, remove() {}, toggle() {}, contains: () => false, replace() {},
    },
    attributes: [],
    getAttribute: () => null,
    setAttribute() {},
    removeAttribute() {},
    hasAttribute: () => false,
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => true,
    appendChild(child) { return child; },
    removeChild(child) { return child; },
    insertBefore(child) { return child; },
    replaceChildren() {},
    remove() {},
    closest: () => null,
    contains: () => false,
    matches: () => false,
    focus() {},
    blur() {},
    click() {},
    scrollIntoView() {},
    getBoundingClientRect: () => ({
      top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0, x: 0, y: 0,
    }),
    querySelector: () => null,
    querySelectorAll: () => [],
    getElementsByTagName: () => [],
    play: () => Promise.resolve(),
    pause() {},
    load() {},
    canPlayType: () => "",
    addTextTrack: () => ({cues: [], addCue() {}, mode: "hidden"}),
    requestFullscreen: () => Promise.resolve(),
    animate: () => ({cancel() {}, finished: Promise.resolve()}),
    insertAdjacentHTML() {},
    cloneNode() { return element(id); },
    appendData() {},
    after() {},
    before() {},
  };
  return node;
}

function browser() {
  const store = () => {
    const map = new Map();
    return {
      get length() { return map.size; },
      key: (i) => [...map.keys()][i] ?? null,
      getItem: (k) => (map.has(String(k)) ? map.get(String(k)) : null),
      setItem: (k, v) => map.set(String(k), String(v)),
      removeItem: (k) => map.delete(String(k)),
      clear: () => map.clear(),
    };
  };
  const nodes = new Map();
  const documentElement = element("html");
  const body = element("body");
  const head = element("head");
  const doc = {
    documentElement,
    body,
    head,
    title: "",
    cookie: "",
    hidden: false,
    visibilityState: "visible",
    readyState: "complete",
    activeElement: body,
    fullscreenElement: null,
    pictureInPictureElement: null,
    getElementById(id) {
      if (!nodes.has(id)) nodes.set(id, element(id));
      return nodes.get(id);
    },
    createElement: (tag) => element(tag),
    createTextNode: (text) => ({textContent: text, nodeType: 3}),
    createDocumentFragment: () => element("#fragment"),
    querySelector: () => null,
    querySelectorAll: () => [],
    getElementsByTagName: () => [],
    getElementsByClassName: () => [],
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => true,
    execCommand: () => false,
    exitFullscreen: () => Promise.resolve(),
    exitPictureInPicture: () => Promise.resolve(),
  };

  const never = () => new Promise(() => {});
  const listener = class {
    constructor() {}
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() { return []; }
  };

  const globals = {
    document: doc,
    location: {
      href: "https://plurx.test/",
      origin: "https://plurx.test",
      protocol: "https:",
      host: "plurx.test",
      hostname: "plurx.test",
      port: "",
      pathname: "/",
      search: "",
      hash: "",
      reload() {},
      assign() {},
      replace() {},
      toString() { return this.href; },
    },
    history: {state: null, pushState() {}, replaceState() {}, back() {}, go() {}},
    navigator: {
      userAgent: "Mozilla/5.0 (asset-load stub)",
      platform: "Linux",
      language: "en-US",
      languages: ["en-US"],
      maxTouchPoints: 0,
      onLine: true,
      clipboard: {writeText: () => Promise.resolve()},
      mediaCapabilities: {decodingInfo: never},
      serviceWorker: {register: never, addEventListener() {}},
      sendBeacon: () => true,
      share: never,
    },
    localStorage: store(),
    sessionStorage: store(),
    screen: {width: 1920, height: 1080, orientation: {type: "landscape-primary"}},
    visualViewport: {width: 1920, height: 1080, addEventListener() {}},
    innerWidth: 1920,
    innerHeight: 1080,
    devicePixelRatio: 2,
    scrollX: 0,
    scrollY: 0,
    // A pending promise, never a rejection: the app's boot() runs at load and
    // a rejected fetch here would surface as an unhandled rejection that has
    // nothing to do with whether the files loaded.
    fetch: never,
    matchMedia: () => ({
      matches: false, media: "", addEventListener() {}, removeEventListener() {},
      addListener() {}, removeListener() {},
    }),
    getComputedStyle: () => styleStub(),
    requestAnimationFrame: () => 0,
    cancelAnimationFrame() {},
    requestIdleCallback: () => 0,
    cancelIdleCallback() {},
    setTimeout: () => 0,
    clearTimeout() {},
    setInterval: () => 0,
    clearInterval() {},
    queueMicrotask() {},
    scrollTo() {},
    scrollBy() {},
    open: () => null,
    close() {},
    alert() {},
    confirm: () => false,
    prompt: () => null,
    print() {},
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => true,
    atob: (s) => Buffer.from(String(s), "base64").toString("binary"),
    btoa: (s) => Buffer.from(String(s), "binary").toString("base64"),
    structuredClone: (v) => JSON.parse(JSON.stringify(v ?? null)),
    performance: {now: () => 0, timeOrigin: 0, getEntriesByType: () => []},
    crypto: {randomUUID: () => "00000000-0000-4000-8000-000000000000",
      getRandomValues: (a) => a},
    console,
    URL, URLSearchParams, TextDecoder, TextEncoder, AbortController, Intl,
    Event: class { constructor(type) { this.type = type; } preventDefault() {} stopPropagation() {} },
    CustomEvent: class { constructor(type, init) { this.type = type; this.detail = (init || {}).detail; } },
    IntersectionObserver: listener,
    ResizeObserver: listener,
    MutationObserver: listener,
    MediaSource: {isTypeSupported: () => false},
    ManagedMediaSource: undefined,
    WebSocket: class { constructor() {} close() {} addEventListener() {} send() {} },
    Image: class { constructor() { return element("img"); } },
    Hls: undefined,
    module: undefined,
    exports: undefined,
  };
  return globals;
}

const shell = shellSource();

const declaredIn = new Map();
for (const row of analyze().rows) {
  for (const name of row.names) if (!declaredIn.has(name)) declaredIn.set(name, row.path);
}

/** Evaluate the sidecars, then `order`, in one fresh context. */
function load(order) {
  const context = vm.createContext(browser());
  vm.runInContext("globalThis.window = globalThis; globalThis.self = globalThis;", context);
  const run = (file, source) => {
    try {
      vm.runInContext(source, context, {filename: file});
    } catch (error) {
      const missing = /^(\w+) is not defined$/.exec(error.message)?.[1];
      const owner = missing && declaredIn.get(missing);
      const why = owner
        ? `\n  ${missing} is declared in ${owner}, which is served AFTER ${file}.`
          + "\n  That is an asset-order bug: move the row, or move what uses it."
        : "\n  No web asset declares that name, so this is most likely a browser"
          + "\n  global the stub in this file does not provide yet. Add it.";
      throw new Error(`loading ${file} threw ${error.name}: ${error.message}${why}`);
    }
  };
  // The sidecars load first in the shell, and the body rows read their globals.
  for (const file of shell.rows.sidecars) {
    if (file.endsWith(".css")) continue;
    run(file, fs.readFileSync(path.join(WEB, file), "utf8"));
  }
  run(shell.rows.head[0], shell.headScript);
  for (const file of order) {
    run(file, shell.files.find((f) => f.path === file).source);
  }
  return context;
}

const context = load(shell.rows.body);

// `const`/`let` at script top level are lexical bindings, not properties of the
// global object, so the context object does not carry them — ask the context.
const read = (expression) => vm.runInContext(expression, context);

assert.equal(read("typeof render"), "function", "the router never defined render()");
assert.equal(read("typeof boot"), "function", "the app never defined boot()");
assert.equal(read("typeof applyLayout"), "function",
  "the theme engine never defined applyLayout()");
assert.equal(read("typeof PLAY_CAPS"), "object",
  "player/decode-tiers.js builds PLAY_CAPS at load and it did not survive");

const layouts = read("LAYOUTS");
assert.ok(layouts, "no LAYOUTS registry");
for (const name of ["classic", "catalog", "theater"]) {
  assert.ok(layouts[name], `the ${name} layout never registered`);
}
for (const view of ["home", "item", "library"]) {
  assert.ok(layouts.classic.views[view],
    `classic lost its ${view} view — the relocated registration in `
    + "layouts/register-classic.js is the one block the split moves, and this "
    + "is what proves it still lands");
}
assert.equal(typeof layouts.classic.chrome, "function", "classic has no chrome()");

console.log(
  `PASS ${shell.rows.sidecars.length - 1} sidecars + ${shell.rows.body.length + 1} `
  + "web assets load in served order; render, boot and all three layouts are there",
);

// The relocated registration is the one thing this split moves, so prove this
// check would have seen it go wrong. Put it back at its banner and the load has
// to die — and say why.
{
  const probe = shell.rows.body.filter((f) => f !== "layouts/register-classic.js");
  probe.splice(probe.indexOf("layouts/renderers.js"), 0, "layouts/register-classic.js");
  assert.throws(() => load(probe), (error) => {
    assert.match(error.message, /layouts\/register-classic\.js threw ReferenceError/);
    assert.match(error.message, /classic(ItemBody|HomeBody)/);
    assert.match(error.message, /asset-order bug/);
    return true;
  }, "register-classic served at its banner must fail to load");
  console.log("PASS register-classic served before its functions fails the load");
}
