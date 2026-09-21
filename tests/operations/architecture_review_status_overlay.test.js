"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");

const parserPath = process.argv[2];
const boardPath = process.argv[3];
assert(parserPath && boardPath, "usage: node overlay.test.js PARSER BOARD");

const status = require(path.resolve(parserPath));
const rows = status.parseBoard(fs.readFileSync(path.resolve(boardPath), "utf8"));
const boardIds = rows.map((row) => row.Id);

const fixtures = [
  {
    number: 401,
    title: "WIP: P-01 Rust test execution policy",
    draft: true,
    updated_at: "2026-09-21T02:11:51Z",
    head: {
      ref: "plan/P-01",
      sha: "bab55a7ee58faab85beee47d2de698ef0dfccebb",
    },
  },
  {
    number: 400,
    title: "WIP: C-02 Scan and enrichment hygiene",
    draft: true,
    updated_at: "2026-09-21T02:34:09Z",
    head: {
      ref: "plan/C-02",
      sha: "a49863c2d68e11ae5409e3b195636fc7f7ae3d47",
    },
  },
  {
    number: 396,
    title: "WIP: S-01 Process output capture and scan-probe bounds",
    draft: true,
    updated_at: "2026-09-21T02:14:27Z",
    head: {
      ref: "plan/S-01",
      sha: "53cd4aadbde580a927f4e23b98294b6562d04c5e",
    },
  },
  {
    number: 395,
    title: "C-01 HTTP listener timeouts and asset delivery",
    draft: false,
    updated_at: "2026-09-21T02:40:00Z",
    head: {
      ref: "server/http-listener-timeouts",
      sha: "7911325407b928e22a9fc731aeeb0b64a63da57d",
    },
  },
];

const mapped = status.mapPullsToPlans(fixtures, boardIds);
assert.deepStrictEqual(
  ["P-01", "C-02", "S-01", "C-01"].map((id) => mapped.byPlan[id][0].number),
  [401, 400, 396, 395],
  "current plan branches and the custom-branch title fallback must map",
);

const branchWins = {
  ...fixtures[0],
  title: "WIP: C-02 misleading title",
};
assert.strictEqual(status.planIdForPull(branchWins, boardIds), "P-01");

const hostile = {
  ...mapped.byPlan["C-01"][0],
  title: '<img src=x onerror="alert(1)">',
  headRef: "branch<&\"'name",
  updatedAt: '"><script>alert(1)</script>',
  ci: { state: "success", count: 9 },
};
const markup = status.pullOverlayMarkup(hostile);
assert(!markup.includes("<img"));
assert(!markup.includes("<script"));
assert(markup.includes("&lt;img"));
assert(markup.includes("branch&lt;&amp;&quot;&#39;name"));
assert(markup.includes("/noirr/plurx/pulls/395"));

const exactHead = fixtures[0].head.sha;
assert.deepStrictEqual(
  status.aggregateCommitStatus(
    { sha: exactHead, state: "skipped", total_count: 9, statuses: [] },
    exactHead,
  ),
  { state: "skipped", count: 9 },
);
assert.strictEqual(
  status.aggregateCommitStatus(
    { sha: fixtures[1].head.sha, state: "success", total_count: 9 },
    exactHead,
  ),
  null,
  "a status response for any commit except the PR head must be rejected",
);

const unavailable = status.overlayFallback(null, "HTTP 401");
assert.strictEqual(unavailable.mode, "unavailable");
assert.deepStrictEqual(unavailable.byPlan, {});
const stale = status.overlayFallback({ byPlan: mapped.byPlan }, "HTTP 429");
assert.strictEqual(stale.mode, "stale");
assert.strictEqual(stale.byPlan["P-01"][0].number, 401);

assert.strictEqual(status.PULL_PAGE_SIZE, 50);
assert.strictEqual(status.MAX_PULL_PAGES, 2);
assert.strictEqual(status.MAX_STATUS_FETCHES, 12);
assert.strictEqual(status.STATUS_CONCURRENCY, 4);

process.stdout.write(JSON.stringify({
  currentExamples: ["P-01", "C-02", "S-01"],
  customBranchFallback: "C-01",
  escaped: true,
  fallbackModes: [unavailable.mode, stale.mode],
}));
