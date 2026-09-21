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

assert.strictEqual(status.PULL_PAGE_SIZE, 50);
assert.strictEqual(status.MAX_PULL_PAGES, 2);
assert.strictEqual(status.MAX_STATUS_FETCHES, 12);
assert.strictEqual(status.STATUS_CONCURRENCY, 4);
assert.strictEqual(status.REQUEST_TIMEOUT_MS, 10_000);
assert.deepStrictEqual(
  status.effectiveStatus("unclaimed", mapped.byPlan["P-01"]),
  { group: "active", text: "in-progress (live PR)" },
);
assert.deepStrictEqual(
  status.effectiveStatus("unclaimed", [{ ...mapped.byPlan["P-01"][0], draft: false }]),
  { group: "active", text: "in-review (live PR)" },
);
assert.deepStrictEqual(
  status.effectiveStatus("merged: M1-M3", mapped.byPlan["C-01"]),
  { group: "merged", text: "merged: M1-M3" },
  "an open evidence PR must not erase the canonical merged state",
);
assert.deepStrictEqual(
  status.effectiveStatus("unclaimed", []),
  { group: "unclaimed", text: "unclaimed" },
);

async function main() {
  const unavailable = status.overlayFallback(null, "HTTP 401");
  assert.strictEqual(unavailable.mode, "unavailable");
  assert.deepStrictEqual(unavailable.byPlan, {});
  assert.strictEqual(status.overlayAbsenceText(unavailable), "Overlay unavailable");

  const stale = status.overlayFallback({
    byPlan: mapped.byPlan,
    complete: true,
    hasSnapshot: true,
    mode: "available",
  }, "HTTP 429");
  assert.strictEqual(stale.mode, "stale");
  assert.strictEqual(stale.byPlan["P-01"][0].number, 401);
  assert.strictEqual(
    status.overlayAbsenceText(stale),
    "No PR in stale snapshot",
  );

  const emptyByPlan = Object.fromEntries(boardIds.map((id) => [id, []]));
  const staleEmpty = status.overlayFallback({
    byPlan: emptyByPlan,
    complete: true,
    hasSnapshot: true,
    mode: "available",
  }, "request deadline");
  assert.strictEqual(staleEmpty.mode, "stale");
  assert.strictEqual(
    status.overlayAbsenceText(staleEmpty),
    "No PR in stale snapshot",
    "an empty retained snapshot is still stale, not authoritative absence",
  );
  assert.strictEqual(
    status.overlayAbsenceText({ mode: "loading", complete: false }),
    "PR state loading",
  );
  assert.strictEqual(
    status.overlayAbsenceText({ mode: "truncated", complete: false }),
    "No PR in partial snapshot",
  );
  assert.strictEqual(
    status.overlayAbsenceText({ mode: "available", complete: true }),
    "No mapped open PR",
  );

  const fullPage = Array.from({ length: status.PULL_PAGE_SIZE }, (_, index) => ({ index }));
  const pages = [];
  const truncated = await status.collectPullPages(async (page) => {
    pages.push(page);
    return fullPage;
  });
  assert.deepStrictEqual(pages, [1, 2]);
  assert.strictEqual(truncated.pulls.length, 100);
  assert.strictEqual(truncated.complete, false);
  assert.strictEqual(truncated.pages, 2);

  const complete = await status.collectPullPages(async (page) => (
    page === 1 ? fullPage : [{ number: 101 }]
  ));
  assert.strictEqual(complete.pulls.length, 51);
  assert.strictEqual(complete.complete, true);

  let deadlineCallback;
  let clearedTimer = null;
  const fakeTimers = {
    setTimeout(callback, milliseconds) {
      assert.strictEqual(milliseconds, 25);
      deadlineCallback = callback;
      return 17;
    },
    clearTimeout(timer) {
      clearedTimer = timer;
    },
  };
  const stalled = status.runWithDeadline((signal) => new Promise((resolve, reject) => {
    signal.addEventListener("abort", () => reject(signal.reason), { once: true });
  }), null, 25, fakeTimers);
  deadlineCallback();
  await assert.rejects(stalled, /25 ms deadline/);
  assert.strictEqual(clearedTimer, 17);

  const fence = status.createRefreshFence();
  const committed = [];
  let finishOld;
  const oldResult = new Promise((resolve) => {
    finishOld = resolve;
  });
  const oldRefresh = fence.begin();
  const oldCompletion = oldResult.then((value) => {
    oldRefresh.commit(() => committed.push(value));
  });
  const newRefresh = fence.begin();
  assert.strictEqual(oldRefresh.signal.aborted, true);
  assert.strictEqual(oldRefresh.isCurrent(), false);
  assert.strictEqual(fence.hasActive(), true);
  newRefresh.commit(() => committed.push("newest"));
  finishOld("older late result");
  await oldCompletion;
  assert.deepStrictEqual(
    committed,
    ["newest"],
    "a superseded generation must not overwrite the newer snapshot",
  );
  assert.strictEqual(newRefresh.finish(), true);
  assert.strictEqual(fence.hasActive(), false);

  process.stdout.write(JSON.stringify({
    currentExamples: ["P-01", "C-02", "S-01"],
    customBranchFallback: "C-01",
    escaped: true,
    fallbackModes: [unavailable.mode, stale.mode],
    snapshotCompleteness: [truncated.complete, complete.complete],
    deadlineAborted: true,
    newestGenerationWon: committed[0] === "newest",
  }));
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
