"use strict";

// This row intentionally has no dependencies: it runs before the sidecars so
// a load-time throw in the policy or player runtime is still observable.
const EARLY_ERROR_LIMIT = 20;
const EARLY_ERROR_INTERVAL_MS = 10_000;
const EARLY_ERROR_MESSAGE_MAX = 512;
const EARLY_ERROR_STACK_MAX = 2_048;

function createEarlyErrorReporter({
  send = fetch,
  now = () => performance.now(),
  schedule = setTimeout,
} = {}) {
  const queued = [];
  const seen = new Map();
  let token = null;
  let ua = "browser";
  let reporting = false;
  let sent = 0;
  let lastOverflowSend = -Infinity;
  let firstScriptError = null;
  let sentinelReported = false;

  function clip(value, limit) {
    return String(value == null ? "" : value).slice(0, limit);
  }

  function redact(value, limit) {
    return clip(value, limit)
      .replace(/Bearer\s+[^\s"'<>]+/gi, "Bearer [redacted]")
      .replace(/((?:[a-z][a-z0-9+.-]*:\/\/|\/)[^\s"'<>?]+)\?[^\s"'<>]*/gi, "$1");
  }

  function browserName() {
    const label = String(navigator.userAgent || "");
    if (/\bedg(a|ios|)\//i.test(label)) return "Edge";
    if (/(firefox|fxios)\//i.test(label)) return "Firefox";
    if (/(chrome|crios|chromium)\//i.test(label)) return "Chrome";
    if (/safari\//i.test(label) && /version\//i.test(label)) return "Safari";
    return "browser";
  }

  function maySend() {
    if (sent < EARLY_ERROR_LIMIT) return true;
    return now() - lastOverflowSend >= EARLY_ERROR_INTERVAL_MS;
  }

  function sendReport(body) {
    if (!token) {
      if (queued.length < EARLY_ERROR_LIMIT) queued.push(body);
      return false;
    }
    if (!maySend()) return false;
    sent += 1;
    if (sent >= EARLY_ERROR_LIMIT) lastOverflowSend = now();
    try {
      Promise.resolve(send("/api/v1/client-log", {
        method: "POST",
        keepalive: true,
        headers: {
          "content-type": "application/json",
          "authorization": `Bearer ${token}`,
        },
        body: JSON.stringify(Object.assign({ua}, body)),
      })).catch(() => {});
    } catch (error) {
      void error;
    }
    return true;
  }

  function capture(kind, event = {}) {
    if (reporting) return false;
    reporting = true;
    try {
      const reason = kind === "unhandledrejection" ? event.reason : event.error;
      const message = redact(
        event.message || (reason && reason.message) || reason || kind,
        EARLY_ERROR_MESSAGE_MAX,
      );
      const src = redact(event.filename || event.src || "", EARLY_ERROR_MESSAGE_MAX);
      const line = Number(event.lineno || event.line || 0) || null;
      const col = Number(event.colno || event.col || 0) || null;
      const stack = redact(reason && reason.stack || event.stack || "", EARLY_ERROR_STACK_MAX);
      const key = `${message}\n${src}\n${line || 0}`;
      const count = (seen.get(key) || 0) + 1;
      seen.set(key, count);
      if (count > 1) return false;
      const body = {event: "client_error", level: "error", detail: kind, message};
      if (src) body.src = src;
      if (line != null) body.line = line;
      if (col != null) body.col = col;
      if (stack) body.stack = stack;
      if (!firstScriptError) firstScriptError = body;
      return sendReport(body);
    } catch (error) {
      return false;
    } finally {
      reporting = false;
    }
  }

  function setAuth(nextToken, label = null) {
    token = typeof nextToken === "string" && nextToken ? nextToken : null;
    ua = label || browserName();
    if (!token || reporting) return;
    reporting = true;
    try {
      const pending = queued.splice(0);
      for (const body of pending) {
        if (!maySend()) break;
        sendReport(body);
      }
    } finally {
      reporting = false;
    }
  }

  function markReady() {
    document.documentElement.dataset.boot = "ready";
    const banner = document.getElementById("boot-sentinel");
    if (banner) banner.remove();
  }

  function showBanner(message, reload = false) {
    if (!document.body) return;
    let banner = document.getElementById("boot-sentinel");
    if (!banner) {
      banner = document.createElement("div");
      banner.id = "boot-sentinel";
      banner.className = "boot-sentinel";
      banner.setAttribute("role", "alert");
      document.body.appendChild(banner);
    }
    banner.textContent = message;
    if (reload) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = "Reload";
      button.addEventListener("click", () => location.reload());
      banner.appendChild(button);
    }
  }

  function checkBoot(elapsedMs) {
    if (document.documentElement.dataset.boot !== "loading") return "ready";
    if (firstScriptError) {
      const file = firstScriptError.src
        ? firstScriptError.src.replace(/^.*\/assets\//, "")
        : "an application script";
      showBanner(`Cinema crashed while loading ${file}. Reload to try again.`, true);
      if (!sentinelReported) {
        sentinelReported = true;
        reporting = true;
        try {
          sendReport({event: "boot_sentinel", level: "error", detail: "crashed",
            message: `bootstrap crashed in ${file}`, src: firstScriptError.src || undefined});
        } finally {
          reporting = false;
        }
      }
      return "crashed";
    }
    if (elapsedMs < 20_000) return document.readyState === "complete" ? "waiting" : "slow";
    if (document.readyState !== "complete") {
      showBanner(`Still loading (${Math.round(elapsedMs / 1_000)}s)…`);
      return "slow";
    }
    showBanner(`Cinema did not finish starting after ${Math.round(elapsedMs / 1_000)}s.`, true);
    return "timed_out";
  }

  document.documentElement.dataset.boot = "loading";
  window.addEventListener("error", (event) => capture("error", event));
  window.addEventListener("unhandledrejection", (event) => capture("unhandledrejection", event));
  schedule(() => checkBoot(5_000), 5_000);
  schedule(() => checkBoot(20_000), 20_000);

  return Object.freeze({capture, setAuth, markReady, checkBoot});
}

const PlurxErrorReporter = createEarlyErrorReporter();
window.PlurxErrorReporter = PlurxErrorReporter;
function markBootReady() { PlurxErrorReporter.markReady(); }
