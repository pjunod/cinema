"use strict";
const test = require("node:test"), assert = require("node:assert/strict"), fs = require("node:fs"), path = require("node:path"), vm = require("node:vm"), {EventEmitter} = require("node:events");
const source = fs.readFileSync(path.join(__dirname, "cinema-remote-browser-support.cjs"), "utf8");
function launchFixture(platform, uid, failure) {
  let args, profile;
  const child = new EventEmitter();
  child.stderr = new EventEmitter(); child.exitCode = null; child.signalCode = null;
  child.kill = () => { child.signalCode = "SIGTERM"; child.emit("exit"); };
  const module = {exports: {}};
  vm.runInNewContext(source, {module, process: {platform, getuid: () => uid, env: {}}, setTimeout, clearTimeout,
    require(name) {
      if (name === "playwright") return {chromium: {executablePath: () => "/disposable/chromium"}};
      if (name === "node:child_process") return {spawn(_binary, actualArgs) {
        args = actualArgs; profile = args.find(arg => arg.startsWith("--user-data-dir=")).split("=")[1];
        setTimeout(() => {
          if (failure.spawn) child.emit("error", new Error(failure.spawn));
          else { child.stderr.emit("data", Buffer.from(failure.stderr)); child.exitCode = failure.code; child.emit("exit"); }
        }, 0);
        return child;
      }};
      return require(name);
    }});
  return {run: () => module.exports.cinemaRealFocusBrowser(), args: () => args, profile: () => profile};
}
for (const [platform, uid, sandboxDisabled] of [["linux", 0, true], ["linux", 1000, false], ["darwin", 0, false]]) {
  test(`disposable native Chromium launch policy ${platform} uid ${uid}`, async () => {
    const fixture = launchFixture(platform, uid, {stderr: "native launch refusal", code: 17});
    await assert.rejects(fixture.run(), error => error.message.includes("exit=17") && error.message.includes("native launch refusal"));
    assert.equal(fixture.args().includes("--no-sandbox"), sandboxDisabled);
    assert.equal(fixture.args().some(arg => arg.startsWith("--headless")), false);
    assert.equal(fs.existsSync(fixture.profile()), false);
  });
}
test("native Chromium launch failure retains bounded stderr", async () => {
  const fixture = launchFixture("linux", 1000, {stderr: "x".repeat(20000) + " diagnostic tail", code: 1});
  await assert.rejects(fixture.run(), error => error.message.endsWith("diagnostic tail") && error.message.length < 8400);
  assert.equal(fs.existsSync(fixture.profile()), false);
});
test("native Chromium spawn error is reported and the disposable profile is removed", async () => {
  const fixture = launchFixture("linux", 1000, {spawn: "ENOENT executable missing"});
  await assert.rejects(fixture.run(), error => error.message.includes("spawn=ENOENT executable missing"));
  assert.equal(fs.existsSync(fixture.profile()), false);
});

test("TERM-unresponsive Chromium is reaped after KILL before profile removal", async () => {
  const events = [], module = {exports: {}};
  const child = new EventEmitter();
  child.stderr = new EventEmitter(); child.exitCode = null; child.signalCode = null;
  let profile;
  child.kill = signal => {
    events.push(signal);
    assert.equal(fs.existsSync(profile), true);
    if (signal === "SIGKILL") setTimeout(() => {
      assert.equal(fs.existsSync(profile), true, "profile remains until the child actually exits");
      child.signalCode = "SIGKILL"; events.push("exit"); child.emit("exit");
    }, 25);
    return true;
  };
  vm.runInNewContext(source, {module, process: {platform: "linux", getuid: () => 1000, env: {}}, clearTimeout,
    setTimeout(callback, delay) { return setTimeout(callback, delay === 2000 ? 10 : delay); },
    require(name) {
      if (name === "playwright") return {chromium: {
        executablePath: () => "/disposable/chromium",
        connectOverCDP: async (_url, options) => {
          assert.equal(options.noDefaults, true);
          return {contexts: () => [{}], close: async () => {}};
        }
      }};
      if (name === "node:child_process") return {spawn(_binary, args) {
        profile = args.find(arg => arg.startsWith("--user-data-dir=")).split("=")[1];
        fs.writeFileSync(path.join(profile, "DevToolsActivePort"), "12345\n");
        return child;
      }};
      return require(name);
    }});
  const runtime = await module.exports.cinemaRealFocusBrowser();
  await runtime.close(); events.push("closed");
  assert.deepEqual(events, ["SIGTERM", "SIGKILL", "exit", "closed"]);
  assert.equal(fs.existsSync(profile), false);
});

test("native Browser.close exits gracefully before cleanup without TERM", async () => {
  const events = [], module = {exports: {}};
  const child = new EventEmitter();
  child.stderr = new EventEmitter(); child.exitCode = null; child.signalCode = null;
  let profile;
  child.kill = signal => { throw new Error("unexpected " + signal); };
  vm.runInNewContext(source, {module, process: {platform: "linux", getuid: () => 1000, env: {}}, setTimeout, clearTimeout,
    require(name) {
      if (name === "playwright") return {chromium: {
        executablePath: () => "/disposable/chromium",
        connectOverCDP: async () => ({contexts: () => [{}], close: async () => { events.push("disconnect"); },
          newBrowserCDPSession: async () => ({send: async command => {
            assert.equal(command, "Browser.close"); events.push(command);
            setTimeout(() => {
              assert.equal(fs.existsSync(profile), true);
              child.exitCode = 0; events.push("exit"); child.emit("exit");
            }, 25);
            throw new Error("expected connection closed during native shutdown");
          }})})
      }};
      if (name === "node:child_process") return {spawn(_binary, args) {
        profile = args.find(arg => arg.startsWith("--user-data-dir=")).split("=")[1];
        fs.writeFileSync(path.join(profile, "DevToolsActivePort"), "12345\n"); return child;
      }};
      return require(name);
    }});
  const runtime = await module.exports.cinemaRealFocusBrowser();
  await runtime.close(); events.push("closed");
  assert.deepEqual(events, ["Browser.close", "exit", "disconnect", "closed"]);
  assert.equal(fs.existsSync(profile), false);
});
