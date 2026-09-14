"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const {test} = require("node:test");
const LibraryChannelCore = require("../../crates/plurxd/src/web/library-channels.js");
const shell = fs.readFileSync(require("node:path").join(__dirname,
  "../../crates/plurxd/src/web/index.html"), "utf8");
const source = shell.slice(shell.indexOf("function libraryChannelDraftKey("),
  shell.indexOf("async function libraryChannelTune("));

function editor() {
  const storage = new Map(), nodes = new Map(), calls = [];
  const context = vm.createContext({
    LibraryChannelCore, SERVER: {instance_id: "test-server"}, ME: {id: 1, is_admin: true},
    location: {origin: "https://test", hash: "#/library-channels"},
    sessionStorage: {get length() {return storage.size;}, key: index => [...storage.keys()][index],
      getItem: key => storage.get(key) || null,
      setItem: (key, value) => storage.set(key, value), removeItem: key => storage.delete(key)},
    document: {getElementById: id => nodes.get(id), querySelectorAll: () => []},
    LIBRARY_CHANNELS: {draft: LibraryChannelCore.emptyDraft({server: "test-server", user: 1}),
      libs: [], channels: [], editor: true},
    api: async (url, options) => {
      calls.push({url, options});
      if (url === "/libraries") return [];
      if (url.startsWith("/search")) return {results: []};
      return {eligible_count: 1, matches: [], first_ten: [], preview_seed: "abc"};
    },
    confirm: () => true, toast: () => {}, PAGE_RENDER_GENERATION: 1,
    newRequestId: () => "request", URLSearchParams, setTimeout: () => 0,
    esc: value => String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;")
      .replaceAll('"', "&quot;"),
    setHead: title => `<h1>${title}</h1>`,
  });
  vm.runInContext(source, context);
  // Emulate the controls replaced when the actual editor renders.
  context.libraryChannelsPaint = () => {
    nodes.clear();
    const draft = context.LIBRARY_CHANNELS.draft;
    if (!draft || draft.step !== "content") return;
    for (const [id, key] of [["lc-genres", "genres_any"], ["lc-tags", "tags_any"],
      ["lc-keywords", "keywords_any"]]) nodes.set(id, {value: draft.recipe[key].join(", ")});
    nodes.set("lc-all", {checked: draft.recipe.match_all_in_scope});
  };
  return {context, storage, nodes, calls};
}

test("Create never resumes an old edit ID, including legacy stored drafts", async () => {
  const {context: c, storage} = editor();
  const stale = {...c.LIBRARY_CHANNELS.draft, id: "deleted", name: "old", expected_revision: 3};
  storage.set(c.libraryChannelDraftKey(), JSON.stringify(stale));
  await c.openLibraryChannelEditor();
  assert.equal(c.LIBRARY_CHANNELS.draft.id, null);
  c.LIBRARY_CHANNELS.draft.name = "new";
  c.viewLibraryChannels = async () => {};
  const requests = [];
  c.api = async (url, options) => requests.push({url, options});
  await c.saveLibraryChannel();
  assert.equal(requests[0].url, "/library-channels");
  assert.equal(requests[0].options.method, "POST");
});

test("create and edit drafts survive independently; discard clears its draft", () => {
  const {context: c} = editor();
  c.LIBRARY_CHANNELS.draft.name = "new draft";
  c.saveLibraryChannelDraft();
  c.LIBRARY_CHANNELS.draft = {...c.LIBRARY_CHANNELS.draft, id: "existing", name: "edited", dirty: true};
  c.saveLibraryChannelDraft();
  assert.equal(c.loadLibraryChannelDraft().name, "new draft");
  assert.equal(c.loadLibraryChannelDraft("existing").name, "edited");
  c.closeLibraryChannelEditor();
  assert.equal(c.loadLibraryChannelDraft("existing").name, "");
  assert.equal(c.loadLibraryChannelDraft().name, "new draft");
});

test("sign-out clears all this account's drafts and fences a pending preview", async () => {
  const {context: c, storage} = editor();
  c.saveLibraryChannelDraft();
  c.LIBRARY_CHANNELS.draft.id = "existing";
  c.saveLibraryChannelDraft();
  const other = "plurx_library_channel_draft_v1:test-server:10";
  storage.set(other, "other account");
  let resolve;
  c.api = () => new Promise(done => {resolve = done;});
  const pending = c.previewLibraryChannel();
  c.clearLibraryChannelDraft(true);
  resolve({eligible_count: 5786});
  await pending;
  assert.equal(c.LIBRARY_CHANNELS.draft, null);
  assert.deepEqual([...storage.keys()], [other]);
});

test("subject preset replaces old all-library controls before preview reads them", async () => {
  const {context: c, calls} = editor();
  c.LIBRARY_CHANNELS.draft.recipe.match_all_in_scope = true;
  c.libraryChannelsPaint();
  await c.lcPreset("space");
  const recipe = calls.find(call => call.options?.body?.recipe)?.options.body.recipe;
  assert.equal(recipe.match_all_in_scope, false);
  assert.deepEqual(Array.from(recipe.genres_any), []);
  assert.deepEqual(Array.from(recipe.keywords_any), []);
  assert.match(recipe.subject, /space exploration/);
});

test("stand-up preset uses performance phrases rather than broad comedy", async () => {
  const {context: c, calls} = editor();
  await c.lcPreset("standup");
  const recipe = calls.find(call => call.options?.body?.recipe)?.options.body.recipe;
  assert.equal(recipe.match_all_in_scope, false);
  assert.match(recipe.subject, /Stand-up comedy performances/);
  assert.match(recipe.subject, /Exclude sitcoms, comedy movies, talk shows/);
  assert.deepEqual(Array.from(recipe.keywords_any), []);
  assert.deepEqual(Array.from(recipe.genres_any), []);
});

test("title search preserves typed subject filters before repainting", async () => {
  const {context: c, nodes} = editor();
  nodes.set("lc-keywords", {value: "stand-up"});
  nodes.set("lc-search", {value: "Bill Maher"});
  await c.lcSearchTitles();
  assert.deepEqual(Array.from(c.LIBRARY_CHANNELS.draft.recipe.keywords_any), ["stand-up"]);
});

test("failed preview invalidates old eligibility evidence", async () => {
  const {context: c} = editor();
  c.LIBRARY_CHANNELS.draft.preview = {eligible_count: 5786};
  c.api = async () => {throw new Error("preview failed");};
  await c.previewLibraryChannel();
  assert.equal(c.LIBRARY_CHANNELS.draft.preview, null);
  assert.equal(c.LIBRARY_CHANNELS.draft.previewError, "preview failed");
});

test("older preview response cannot replace the current subject preview", async () => {
  const {context: c} = editor();
  const resolves = [];
  c.api = () => new Promise(resolve => resolves.push(resolve));
  const first = c.previewLibraryChannel(false);
  c.LIBRARY_CHANNELS.draft.recipe.keywords_any = ["stand-up"];
  const second = c.previewLibraryChannel(false);
  resolves[1]({eligible_count: 12});
  await second;
  resolves[0]({eligible_count: 5786});
  await first;
  assert.equal(c.LIBRARY_CHANNELS.draft.preview.eligible_count, 12);
});

test("editing filters while preview is pending preserves the edits and discards that response", async () => {
  const {context: c, nodes} = editor();
  c.libraryChannelsPaint();
  let resolve;
  c.api = () => new Promise(done => {resolve = done;});
  const pending = c.previewLibraryChannel();
  nodes.get("lc-keywords").value = "stand-up";
  resolve({eligible_count: 5786});
  await pending;
  assert.equal(c.LIBRARY_CHANNELS.draft.preview, null);
  assert.deepEqual(Array.from(c.LIBRARY_CHANNELS.draft.recipe.keywords_any), ["stand-up"]);
});

test("opening another channel cannot copy the previous editor's controls into it", async () => {
  const {context: c} = editor();
  c.LIBRARY_CHANNELS.draft.recipe.keywords_any = ["old subject"];
  c.libraryChannelsPaint();
  const recipe = {...LibraryChannelCore.defaultRecipe(), keywords_any: ["space"]};
  c.api = async url => url === "/library-channels/new-id"
    ? {id: "new-id", name: "Space", revision: 1, recipe}
    : url === "/libraries" ? [] : {eligible_count: 10};
  await c.openLibraryChannelEditor("new-id");
  assert.deepEqual(Array.from(c.LIBRARY_CHANNELS.draft.recipe.keywords_any), ["space"]);
});

test("preview names actual matching titles even when scheduled entries are on later pages", () => {
  const {context: c} = editor();
  c.LIBRARY_CHANNELS.draft.preview = {eligible_count: 100, repeat_description: "one rotation",
    first_ten: [{item_id: 99}], matches: [{candidate: {item_id: 1, title: "A stand-up special"},
      reasons: ["keyword: stand-up"]}]};
  const html = c.libraryChannelEditorHtml();
  assert.match(html, /A stand-up special/);
  assert.match(html, /keyword: stand-up/);
  assert.doesNotMatch(html, /Title 99/);
});

test("shared wire fixture and subject saving need no preview", () => {
  const fixture = JSON.parse(fs.readFileSync(require("node:path").join(__dirname,"../contracts/channel-subject-wire.json"),"utf8"));
  for (const row of fixture.cases) {
    const draft = LibraryChannelCore.emptyDraft({server:"a",user:1});
    draft.recipe = row.recipe; draft.name = "Subject channel"; draft.enabled = true;
    assert.equal(LibraryChannelCore.validateDraft(draft).enabled, undefined);
    draft.recipe.subject = null;
    assert.ok(JSON.stringify(draft.recipe).includes('"subject":null'));
  }
});

test("lost save response retries the same request ID", async () => {
  const {context:c,calls}=editor(); c.LIBRARY_CHANNELS.draft.name="Offline";c.LIBRARY_CHANNELS.draft.enabled=true;
  let serial=0;c.newRequestId=()=>`save-${++serial}`;
  const sent=[];c.api=async (url,options)=>{sent.push(options.body);throw new Error("lost response");};
  await c.saveLibraryChannel();await c.saveLibraryChannel();assert.equal(sent[0].request_id,sent[1].request_id);
  c.LIBRARY_CHANNELS.draft.name="Changed";await c.saveLibraryChannel();assert.notEqual(sent[1].request_id,sent[2].request_id);
});
