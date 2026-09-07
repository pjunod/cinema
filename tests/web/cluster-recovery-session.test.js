"use strict";

// A cluster fault must not end an operator's session.
//
// `/cluster/status` and `/cluster/support-bundle` are guarded on the server by
// a process-local proof cache. When the cluster has not finished activating
// its credential-revocation protocol that cache is fenced closed on every
// node, and the guard used to answer 401 — which this client answered by
// calling `logout()`. Settings remembers its last section, so an operator
// whose last section was Cluster was thrown at the login page on every visit,
// holding a valid credential, over the very fault they were opening the page
// to read. The server fix makes the answer honest; this one makes the client
// stop treating a recovery-read refusal as a verdict on the session, because
// the two failures cost very differently: a refusal painted in the panel is
// recoverable and a destroyed session is not.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const SHIPPED_UI = fs.readFileSync(
  path.join(__dirname, "../../crates/plurxd/src/web/index.html"),
  "utf8",
);

const DECLARATIONS = ["\nfunction ", "\nasync function "];
const TERMINATORS = DECLARATIONS.concat([
  "\nconst ", "\nlet ", "\nwindow.", "\ndocument.", "\nsetInterval(",
]);
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) => SHIPPED_UI.indexOf(`${kind}${name}(`))
    .find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = TERMINATORS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
  return (ends.length ? rest.slice(0, Math.min(...ends)) : rest).trimEnd();
}

// The real shipped `api`, executed — not read. A harness that only matched the
// source text would pass against a function whose branch had been rewired.
function apiHarness(response) {
  const calls = { logout: 0, fetched: [] };
  const run = new Function(
    "PlaybackPolicy", "response", "calls",
    [
      "const API='/api/v1';let TOKEN='t';let AUTH_GENERATION=0;",
      "function logout(){calls.logout++;}",
      "function fetch(url,init){calls.fetched.push({url,init});return Promise.resolve(response);}",
      shippedSource("api"),
      "return api;",
    ].join("\n"),
  );
  const api = run({ parseStreamFailure: () => null }, response, calls);
  return { api, calls };
}

function refusal(status, body) {
  const text = JSON.stringify(body);
  return {
    status,
    ok: status >= 200 && status < 300,
    text: () => Promise.resolve(text),
    json: () => Promise.resolve(body),
  };
}

async function refusalFrom(api, path, options) {
  try {
    await api(path, options);
  } catch (error) {
    return error;
  }
  throw new Error(`${path} resolved where it had to refuse`);
}

async function main() {
// 1. An ordinary route's 401 still ends the session. That is the credential
//    contract and this change must not weaken it.
{
  const { api, calls } = apiHarness(refusal(401, { error: "authentication required" }));
  const error = await refusalFrom(api, "/libraries");
  assert.equal(error.status, 401);
  assert.equal(calls.logout, 1, "an ordinary 401 must still end the session");
}

// 2. The recovery reads refuse without ending it, and the caller still learns
//    the status and the server's stable code so the panel can say why.
{
  const body = {
    code: "cluster_recovery_authorization_unavailable",
    message: "this node could not authorize a cluster recovery read",
  };
  const { api, calls } = apiHarness(refusal(401, body));
  const error = await refusalFrom(api, "/cluster/status", { keepSessionOn401: true });
  assert.equal(calls.logout, 0, "a cluster-recovery refusal must not end the session");
  assert.equal(error.status, 401);
  assert.equal(error.code, body.code, "the panel needs the stable code, not a sentence");
  assert.equal(error.message, body.message);
}

// 3. A 403 there is a verdict on the user and never ended the session anyway;
//    pin it so the new branch cannot swallow it.
{
  const { api, calls } = apiHarness(refusal(403, { error: "admin privileges required" }));
  const error = await refusalFrom(api, "/cluster/status", { keepSessionOn401: true });
  assert.equal(calls.logout, 0);
  assert.equal(error.status, 403);
}

}

// 4. The option reaches the two call sites that need it, and only those. A
//    working `api` with the flag never passed is the same bug again.
assert.equal(
  (SHIPPED_UI.match(/api\("\/cluster\/status",\{keepSessionOn401:true\}\)/g) || []).length,
  3,
  "every /cluster/status read — first load, tick, and restart poll — carries the option",
);
assert.ok(
  !/api\("\/cluster\/status"\)/.test(SHIPPED_UI),
  "a bare /cluster/status read would end the session on a cluster fault",
);
assert.ok(
  SHIPPED_UI.includes('api("/cluster/support-bundle",{raw:true,keepSessionOn401:true})'),
  "the support bundle carries the same server guard and needs the same option",
);
// The async cases are awaited before anything reports success: a runner that
// prints "ok" while its promises are still pending has proved nothing.
main().then(() => console.log("cluster-recovery-session: ok"));
