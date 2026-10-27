"use strict";
// Start a disposable native Chromium, then attach with noDefaults. Playwright's
// normal launcher forces every document focused; a second CDP session cannot
// undo that first session's override. Production focus eligibility is unchanged.
const fs = require("node:fs"), os = require("node:os"), path = require("node:path"), {spawn} = require("node:child_process");
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || "playwright");
async function cinemaRealFocusBrowser() {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "cinema-remote-profile-"));
  const args = ["--user-data-dir=" + profile, "--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check", "about:blank"];
  // Root inside the disposable Linux CI container cannot use Chromium's
  // sandbox. Keep the normal sandbox on every other host/user.
  if (process.platform === "linux" && process.getuid?.() === 0) args.unshift("--no-sandbox");
  const child = spawn(process.env.CHROMIUM_EXECUTABLE || chromium.executablePath(), args, {stdio: ["ignore", "ignore", "pipe"]});
  let browser, stderr = "", spawnError;
  child.stderr.on("data", chunk => { stderr = (stderr + chunk.toString()).slice(-8192); });
  child.once("error", error => { spawnError = error; });
  async function stop(graceful = false) {
    if (child.exitCode === null && child.signalCode === null && !spawnError) {
      await new Promise(resolve => {
        let timer;
        const exited = () => child.exitCode !== null || child.signalCode !== null;
        const done = () => { clearTimeout(timer); child.removeListener("exit", done); resolve(); };
        child.once("exit", done);
        const terminate = () => {
          timer = setTimeout(() => {
            child.kill("SIGKILL");
            if (exited()) done();
          }, 2000);
          child.kill("SIGTERM");
          // A failed kill can race an already observed exit. Otherwise retain
          // the exit listener, including after escalation, before deleting data.
          if (exited()) done();
        };
        if (graceful) timer = setTimeout(terminate, 2000);
        else terminate();
        if (exited()) done();
      });
    }
    // Exited Chromium helpers can briefly finish profile writes. Retry only
    // transient filesystem cleanup errors, then retain any exhausted failure.
    fs.rmSync(profile, {recursive: true, force: true, maxRetries: 3, retryDelay: 100});
  }
  async function bounded(operation) {
    let timer;
    try {
      await Promise.race([
        Promise.resolve().then(operation).catch(() => {}),
        new Promise(resolve => { timer = setTimeout(resolve, 2000); }),
      ]);
    } finally { clearTimeout(timer); }
  }
  async function close() {
    let requested = false;
    if (browser) await bounded(async () => {
      const session = await browser.newBrowserCDPSession();
      requested = true;
      // A CDP-attached Playwright browser.close only disconnects transport.
      // Ask this owned native browser to flush and shut down its helpers first.
      await session.send("Browser.close");
    });
    try { await stop(requested); }
    finally { if (browser) await bounded(() => browser.close()); }
  }
  try {
    const deadline = Date.now() + 15000, file = path.join(profile, "DevToolsActivePort");
    while (!fs.existsSync(file)) {
      if (Date.now() > deadline || spawnError || child.exitCode !== null || child.signalCode !== null) {
        throw new Error("Disposable Chromium did not start" +
          ` (exit=${child.exitCode}, signal=${child.signalCode}` + (spawnError ? `, spawn=${spawnError.message}` : "") + ")" +
          (stderr.trim() ? "\n" + stderr.trim() : ""));
      }
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    const port = Number(fs.readFileSync(file, "utf8").split("\n")[0]);
    if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid Chromium debugging port");
    browser = await chromium.connectOverCDP("http://127.0.0.1:" + port, {noDefaults: true});
    const context = browser.contexts()[0];
    return {browser, context, close};
  } catch (error) {
    await close();
    throw error;
  }
}
module.exports = {cinemaRealFocusBrowser};
