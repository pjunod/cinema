"use strict";

// Sign-ins that expire must say so.
//
// When "Sign-ins expire" is on, the server refuses a device that went unused
// for the whole idle window with 401 `session_expired` and a sentence naming
// the window. The browser has to land on its sign-in screen with that
// sentence rather than the generic "Your session ended on the server." — and
// keep it there even though boot paints the login screen twice for one dead
// credential. The Users section owns the switch and shows, per device, when
// it will be signed out.

const assert = require("node:assert/strict");

const {shellSource} = require("./shell-source.js");
const SHIPPED_UI = shellSource().bodyScript;

const DECLARATIONS = ["\nfunction ", "\nasync function "];
const TERMINATORS = DECLARATIONS.concat([
  "\nconst ", "\nlet ", "\nwindow.", "\ndocument.", "\nsetInterval(", "\n// ",
]);
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) => SHIPPED_UI.indexOf(`${kind}${name}(`))
    .find((at) => at !== -1);
  assert.notEqual(start, undefined, `the shell no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = TERMINATORS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
  return (ends.length ? rest.slice(0, Math.min(...ends)) : rest).trimEnd();
}

function reply(status, body) {
  const text = JSON.stringify(body);
  return {
    status,
    ok: status >= 200 && status < 300,
    text: () => Promise.resolve(text),
    json: () => Promise.resolve(body),
  };
}

// The real shipped `api`, executed.
function apiHarness(answer) {
  const calls = { logout: [] };
  const api = new Function(
    "PlaybackPolicy", "answer", "calls",
    [
      "const API='/api/v1';let TOKEN='t';let AUTH_GENERATION=0;",
      "function logout(options){calls.logout.push(options);}",
      "function fetch(){return Promise.resolve(answer);}",
      shippedSource("api"),
      "return api;",
    ].join("\n"),
  )({ parseStreamFailure: () => null }, answer, calls);
  return { api, calls };
}

async function refusal(api) {
  try {
    await api("/me");
  } catch (error) {
    return error;
  }
  throw new Error("an expired credential resolved");
}

const EXPIRED = reply(401, {
  code: "session_expired",
  message: "Signed out after 90 days of inactivity. Sign in again to continue.",
  idle_days: 90,
});

async function main() {
  // 1. The server's reason reaches the sign-in screen.
  {
    const { api, calls } = apiHarness(EXPIRED);
    const error = await refusal(api);
    assert.equal(error.status, 401);
    assert.equal(error.code, "session_expired");
    assert.equal(calls.logout.length, 1, "an expired sign-in still ends the session");
    assert.equal(calls.logout[0].revoke, false);
    assert.equal(
      calls.logout[0].notice,
      "Signed out after 90 days of inactivity. Sign in again to continue.",
    );
  }

  // 2. Any other 401 keeps the generic sentence — including one whose body
  //    cannot be read at all.
  for (const answer of [
    reply(401, { error: "authentication required" }),
    reply(401, { code: "something_else", message: "not this one" }),
    { status: 401, ok: false, text: () => Promise.reject(new Error("aborted")) },
  ]) {
    const { api, calls } = apiHarness(answer);
    const error = await refusal(api);
    assert.equal(error.status, 401);
    assert.equal(error.code, undefined);
    assert.equal(calls.logout.length, 1);
    assert.equal(calls.logout[0].notice, "Your session ended on the server.");
  }

  // 3. Boot paints the login screen twice for one dead credential (the ended
  //    session re-renders, then boot's own catch does). The reason must
  //    survive both paints and go only when a sign-in succeeds.
  {
    const painted = [];
    let submit = null;
    const run = new Function(
      "painted", "setSubmit",
      [
        "let AUTH_NOTICE='Signed out after 90 days of inactivity. Sign in again to continue.';",
        "let TOKEN=null, AUTH_GENERATION=0, ME=null; const APP_NAME='Cinema';",
        "const location={origin:'http://x',hash:''};",
        "const localStorage={setItem(){}};",
        "const u={value:'paul'}, p={value:'pw'};",
        "function esc(s){return String(s);}",
        "function markBootReady(){}",
        "function refreshClientErrorReporterAuth(){}",
        "function render(){}",
        "async function api(){return {token:'new',user:{id:1}};}",
        "function authShell(title,sub,fields,btn,onsubmit){painted.push(sub);setSubmit(onsubmit);}",
        shippedSource("renderLogin"),
        "return {renderLogin, notice:()=>AUTH_NOTICE};",
      ].join("\n"),
    );
    const shell = run(painted, (fn) => { submit = fn; });
    shell.renderLogin();
    shell.renderLogin();
    assert.deepEqual(painted, [
      "Signed out after 90 days of inactivity. Sign in again to continue.",
      "Signed out after 90 days of inactivity. Sign in again to continue.",
    ]);
    await submit();
    assert.equal(shell.notice(), "", "a successful sign-in clears the reason");
  }

  // 4. Each device's expiry, as the Users section's devices drawer says it.
  {
    const label = new Function(`${shippedSource("deviceExpiryLabel")}; return deviceExpiryLabel;`)();
    const now = 1_000_000;
    assert.equal(label({ expires_at: null, expired: false }, now), "Doesn't expire");
    assert.equal(label({ expires_at: now - 1, expired: true }, now), "Signed out — unused too long");
    assert.match(label({ expires_at: now + 10 * 86400, expired: false }, now), /^Signs out in 10 days if unused/);
    assert.match(label({ expires_at: now + 3600, expired: false }, now), /^Signs out within a day if unused/);
  }

  // 5. The switch lives with the accounts it governs, on by default, and
  //    the window control keeps a custom stored value selectable.
  {
    const card = new Function(
      "setCard", "cardHead", "togRow", "togSelect", "setCardFoot",
      `const SIGN_IN_DAYS=[[7,"7 days"],[30,"30 days"],[90,"90 days"],[180,"180 days"],[365,"1 year"]];
       ${shippedSource("signInExpiryCard")}; return signInExpiryCard;`,
    )(
      (body) => body,
      (title) => `<h2>${title}</h2>`,
      (id, _label, _note, checked) => `<tog ${id} ${checked ? "on" : "off"}>`,
      (id, _label, _note, options, value) => `<sel ${id} ${value} ${options.map(([v]) => v).join(",")}>`,
      (fn) => `<save ${fn}>`,
    );
    const defaults = card({ auth_token_expiry: true, auth_token_idle_days: 90 });
    assert.match(defaults, /<h2>Sign-ins<\/h2>/);
    assert.match(defaults, /<tog signin-expire on>/);
    assert.match(defaults, /<sel signin-days 90 7,30,90,180,365>/);
    assert.match(defaults, /<save saveSignIns>/);
    assert.match(card({ auth_token_expiry: false, auth_token_idle_days: 45 }),
      /<tog signin-expire off>[\s\S]*<sel signin-days 45 7,30,45,90,180,365>/);
    assert.match(SHIPPED_UI, /const SIGN_IN_DAYS=\[\[7,"7 days"\],\[30,"30 days"\],\[90,"90 days"\],\[180,"180 days"\],\[365,"1 year"\]\];/,
      "the harness's option list is the shipped one");
    assert.ok(shippedSource("usersPanel").includes("signInExpiryCard(settings)"));
    assert.ok(shippedSource("saveSignIns").includes("auth_token_expiry:"));
    assert.ok(shippedSource("saveSignIns").includes("auth_token_idle_days:"));
    assert.ok(shippedSource("userRow").includes("openDeviceDrawer("));
    assert.ok(shippedSource("deviceDrawerHtml").includes("deviceExpiryLabel(row,now)"));
  }
}

main().then(() => console.log("session-expiry: ok"), (error) => {
  console.error(error);
  process.exit(1);
});
