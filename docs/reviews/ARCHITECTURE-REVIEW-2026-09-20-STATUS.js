(function exposeParser(root, factory) {
  "use strict";

  const parser = factory();
  if (typeof module === "object" && module.exports) module.exports = parser;
  root.ArchitectureReviewStatusParser = parser;
})(typeof globalThis === "undefined" ? this : globalThis, function buildParser() {
  "use strict";

  const EXPECTED_COLUMNS = Object.freeze([
    "Id", "Plan", "Executes", "Priority", "Status", "Model", "Session",
    "Branch / PR", "Last update", "Notes",
  ]);
  const LIVE_COLUMN = "Live Forgejo overlay";
  const PULL_PAGE_SIZE = 50;
  const MAX_PULL_PAGES = 2;
  const MAX_STATUS_FETCHES = 12;
  const STATUS_CONCURRENCY = 4;
  const REQUEST_TIMEOUT_MS = 10_000;
  const PLAN_BRANCH = /^plan\/([A-Z]-\d{2})$/;
  const PLAN_TITLE = /^(?:WIP:\s*)?([A-Z]-\d{2})(?=\s|:|[-\u2013\u2014]|$)/;
  const COMMIT_SHA = /^[0-9a-f]{40,64}$/;
  const CI_STATES = new Set([
    "error", "failure", "pending", "skipped", "success", "warning",
  ]);

  function splitMarkdownRow(line) {
    const cells = [];
    let cell = "";
    let escaped = false;
    for (const character of line.trim().slice(1, -1)) {
      if (escaped) {
        cell += character;
        escaped = false;
      } else if (character === "\\") {
        escaped = true;
      } else if (character === "|") {
        cells.push(cell.trim());
        cell = "";
      } else {
        cell += character;
      }
    }
    cells.push(cell.trim());
    return cells;
  }

  function parseBoard(markdown) {
    const lines = markdown.split(/\r?\n/);
    const boardHeading = lines.findIndex(
      (line) => line.trim() === "## The board",
    );
    if (boardHeading < 0) {
      throw new Error("The workboard has no ‘The board’ section.");
    }

    const headerIndex = lines.findIndex(
      (line, index) => index > boardHeading && line.startsWith("| Id | Plan |"),
    );
    if (headerIndex < 0) {
      throw new Error("The workboard table header was not found.");
    }

    const columns = splitMarkdownRow(lines[headerIndex]);
    if (columns.join("\u0000") !== EXPECTED_COLUMNS.join("\u0000")) {
      throw new Error(
        "The workboard columns changed; update this view before trusting it.",
      );
    }

    const rows = [];
    for (const line of lines.slice(headerIndex + 2)) {
      if (!line.startsWith("|")) break;
      const values = splitMarkdownRow(line);
      if (values.length !== EXPECTED_COLUMNS.length) {
        throw new Error(
          `Workboard row has ${values.length} cells instead of ` +
          `${EXPECTED_COLUMNS.length}.`,
        );
      }
      rows.push(Object.fromEntries(
        EXPECTED_COLUMNS.map((column, index) => [column, values[index]]),
      ));
    }
    if (!rows.length) throw new Error("The workboard table contains no plans.");
    return rows;
  }

  function escapeHtml(value) {
    return String(value).replace(/[&<>"']/g, (character) => ({
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      '"': "&quot;",
      "'": "&#39;",
    })[character]);
  }

  function planIdForPull(pull, boardIds) {
    const known = boardIds instanceof Set ? boardIds : new Set(boardIds);
    const branch = typeof pull?.head?.ref === "string" ? pull.head.ref : "";
    const branchMatch = PLAN_BRANCH.exec(branch);
    if (branchMatch && known.has(branchMatch[1])) return branchMatch[1];

    const title = typeof pull?.title === "string" ? pull.title.trim() : "";
    const titleMatch = PLAN_TITLE.exec(title);
    if (titleMatch && known.has(titleMatch[1])) return titleMatch[1];
    return null;
  }

  function normalizePull(pull, planId) {
    const number = Number(pull?.number);
    const headRef = typeof pull?.head?.ref === "string" ? pull.head.ref : "";
    const headSha = typeof pull?.head?.sha === "string" ? pull.head.sha : "";
    if (!Number.isSafeInteger(number) || number < 1 || !headRef || !COMMIT_SHA.test(headSha)) {
      return null;
    }
    return {
      planId,
      number,
      title: typeof pull.title === "string" ? pull.title : "",
      draft: pull.draft === true,
      headRef,
      headSha,
      updatedAt: typeof pull.updated_at === "string" ? pull.updated_at : "",
      ci: null,
    };
  }

  function mapPullsToPlans(pulls, boardIds) {
    const known = boardIds instanceof Set ? boardIds : new Set(boardIds);
    const byPlan = Object.fromEntries([...known].map((id) => [id, []]));
    let ignored = 0;
    for (const pull of Array.isArray(pulls) ? pulls : []) {
      const planId = planIdForPull(pull, known);
      const normalized = planId && normalizePull(pull, planId);
      if (!normalized) {
        ignored += 1;
        continue;
      }
      byPlan[planId].push(normalized);
    }
    for (const pullsForPlan of Object.values(byPlan)) {
      pullsForPlan.sort((left, right) => right.number - left.number);
    }
    return { byPlan, ignored };
  }

  function statusGroup(status) {
    const normalized = String(status || "").trim().toLowerCase();
    if (normalized === "unclaimed") return "unclaimed";
    if (["claimed", "in-progress", "in-review"].includes(normalized)) return "active";
    if (normalized.startsWith("blocked:")) return "blocked";
    if (normalized === "merged" || normalized.startsWith("merged:")) return "merged";
    if (normalized === "done") return "done";
    if (normalized.startsWith("abandoned:")) return "abandoned";
    return "active";
  }

  function effectiveStatus(status, pulls) {
    const group = statusGroup(status);
    if (group !== "unclaimed" || !Array.isArray(pulls) || pulls.length === 0) {
      return { group, text: status };
    }
    return pulls.some((pull) => pull.draft !== true)
      ? { group: "active", text: "in-review (live PR)" }
      : { group: "active", text: "in-progress (live PR)" };
  }

  function aggregateCommitStatus(payload, expectedSha) {
    if (!COMMIT_SHA.test(expectedSha) || payload?.sha !== expectedSha) return null;
    const state = typeof payload.state === "string" ? payload.state.toLowerCase() : "";
    const statuses = Array.isArray(payload.statuses) ? payload.statuses : [];
    const count = Number.isSafeInteger(payload.total_count)
      ? payload.total_count
      : statuses.length;
    return {
      state: CI_STATES.has(state) ? state : (count ? "unknown" : "none"),
      count,
    };
  }

  function createRefreshFence(createController = () => new AbortController()) {
    let active = null;
    let generation = 0;
    return Object.freeze({
      begin({ replace = true } = {}) {
        if (active && !replace) return null;
        if (active) {
          active.controller.abort(new Error("Refresh superseded by a newer generation."));
        }
        const controller = createController();
        const currentGeneration = ++generation;
        const ticket = {
          generation: currentGeneration,
          signal: controller.signal,
          isCurrent() {
            return active?.generation === currentGeneration && !controller.signal.aborted;
          },
          commit(operation) {
            if (!this.isCurrent()) return false;
            operation();
            return true;
          },
          finish() {
            if (active?.generation !== currentGeneration) return false;
            active = null;
            return true;
          },
        };
        active = { controller, generation: currentGeneration };
        return Object.freeze(ticket);
      },
      hasActive() {
        return active !== null;
      },
    });
  }

  async function runWithDeadline(
    operation,
    parentSignal,
    timeoutMs = REQUEST_TIMEOUT_MS,
    timers = globalThis,
  ) {
    const controller = new AbortController();
    const abort = (reason) => {
      if (!controller.signal.aborted) controller.abort(reason);
    };
    const relayAbort = () => abort(parentSignal.reason);
    if (parentSignal?.aborted) {
      relayAbort();
    } else {
      parentSignal?.addEventListener("abort", relayAbort, { once: true });
    }
    const timer = timers.setTimeout(
      () => abort(new Error(`Request exceeded its ${timeoutMs} ms deadline.`)),
      timeoutMs,
    );
    try {
      return await operation(controller.signal);
    } finally {
      timers.clearTimeout(timer);
      parentSignal?.removeEventListener("abort", relayAbort);
    }
  }

  async function collectPullPages(
    loadPage,
    pageSize = PULL_PAGE_SIZE,
    maxPages = MAX_PULL_PAGES,
  ) {
    const pulls = [];
    for (let page = 1; page <= maxPages; page += 1) {
      const batch = await loadPage(page);
      if (!Array.isArray(batch)) {
        throw new Error("Forgejo pull list was not an array.");
      }
      pulls.push(...batch);
      if (batch.length < pageSize) {
        return { pulls, complete: true, pages: page };
      }
    }
    return { pulls, complete: false, pages: maxPages };
  }

  function overlayFallback(previous, reason) {
    const byPlan = previous?.byPlan || {};
    const hasSnapshot = previous?.hasSnapshot === true;
    return {
      byPlan,
      mode: hasSnapshot ? "stale" : "unavailable",
      complete: false,
      hasSnapshot,
      reason: String(reason || "Forgejo API request failed."),
    };
  }

  function overlayAbsenceText(overlay) {
    if (overlay?.mode === "unavailable") return "Overlay unavailable";
    if (overlay?.mode === "available" && overlay.complete === true) {
      return "No mapped open PR";
    }
    if (overlay?.mode === "stale") return "No PR in stale snapshot";
    if (overlay?.mode === "loading") return "PR state loading";
    return "No PR in partial snapshot";
  }

  function pullOverlayMarkup(pull) {
    const title = escapeHtml(pull.title || `PR #${pull.number}`);
    const branch = escapeHtml(pull.headRef);
    const updated = escapeHtml(pull.updatedAt || "unknown");
    const sha = escapeHtml(pull.headSha.slice(0, 8));
    const readiness = pull.draft ? "draft" : "ready";
    const ciState = pull.ci?.state || "unavailable";
    const ciCount = Number.isSafeInteger(pull.ci?.count) ? pull.ci.count : 0;
    const ciText = ciState === "unavailable"
      ? "CI unavailable"
      : `${escapeHtml(ciState)} (${ciCount} checks)`;
    return `<div class="live-pr">` +
      `<a href="/noirr/plurx/pulls/${pull.number}" title="${title}">` +
      `PR #${pull.number}</a> ` +
      `<span class="pr-readiness pr-${readiness}">${readiness}</span>` +
      `<div class="overlay-title">${title}</div>` +
      `<div><code>${branch}</code></div>` +
      `<div>updated <time datetime="${updated}">${updated}</time></div>` +
      `<div>head <code>${sha}</code> · ${ciText}</div>` +
      `</div>`;
  }

  return Object.freeze({
    EXPECTED_COLUMNS,
    LIVE_COLUMN,
    MAX_PULL_PAGES,
    MAX_STATUS_FETCHES,
    PULL_PAGE_SIZE,
    REQUEST_TIMEOUT_MS,
    STATUS_CONCURRENCY,
    aggregateCommitStatus,
    collectPullPages,
    createRefreshFence,
    effectiveStatus,
    escapeHtml,
    mapPullsToPlans,
    overlayAbsenceText,
    overlayFallback,
    parseBoard,
    planIdForPull,
    pullOverlayMarkup,
    runWithDeadline,
    splitMarkdownRow,
    statusGroup,
  });
});
