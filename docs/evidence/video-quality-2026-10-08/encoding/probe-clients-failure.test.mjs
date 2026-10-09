import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import vm from 'node:vm';
import { chrome, cdpTransport, pageHtml } from './probe-clients.mjs';

class FakeSocket extends EventTarget {
  send() {}
  close() { this.dispatchEvent(new Event('close')); }
}

function child({ endpoint = false, reap = true } = {}) {
  const value = new EventEmitter();
  value.stderr = new EventEmitter();
  value.signals = [];
  value.kill = signal => {
    value.signals.push(signal);
    if (reap && signal === 'SIGKILL') setImmediate(() => value.emit('close', null, signal));
    return true;
  };
  if (endpoint) setImmediate(() => value.stderr.emit('data', 'DevTools listening on ws://127.0.0.1:1234/devtools/browser/owned\n'));
  return value;
}

function dependencies(ownedChild, removed, overrides = {}) {
  return {
    spawn: () => ownedChild,
    remove: async profile => removed.push(profile),
    startupMs: 25, requestMs: 25, terminateMs: 10, killMs: 25,
    ...overrides,
  };
}

test('fatal manifest rejects the page startup promise before its deadline', async () => {
  const context = pageContext('fatal');
  await assert.rejects(context.start(), /fatal manifest\/media error: manifestLoadError/);
  assert.equal(context.snapshot().events.filter(event => event.event === 'fatal').length, 1);
});

test('a manifest that never responds has a hard page startup deadline', async () => {
  await assert.rejects(pageContext('silent').start(), /media startup deadline exceeded/);
});

test('a play promise that never settles has a hard page startup deadline', async () => {
  await assert.rejects(pageContext('parsed').start(), /media startup deadline exceeded/);
});

function pageContext(mode) {
  class Hls extends EventEmitter {
    static Events = { ERROR: 'hls-error', MANIFEST_PARSED: 'parsed' };
    attachMedia() {}
    loadSource() {
      if (mode === 'fatal') this.emit(Hls.Events.ERROR, {}, { fatal: true, details: 'manifestLoadError' });
      if (mode === 'parsed') this.emit(Hls.Events.MANIFEST_PARSED);
    }
  }
  const video = new EventTarget();
  video.requestVideoFrameCallback = () => {};
  video.play = () => new Promise(() => {});
  const context = {
    Hls, URLSearchParams, performance, setTimeout, clearTimeout,
    location: { search: '?asset=missing' }, navigator: { userAgent: 'fake' },
    document: { querySelector: () => video, createElement: () => ({ getContext: () => ({}) }) },
  };
  context.window = context;
  vm.runInNewContext(pageHtml(25).split('<script>').at(-1).split('</script>')[0], context);
  return context;
}

test('CDP request timeout rejects an unresponsive awaitPromise command', async () => {
  const transport = cdpTransport(new FakeSocket(), 25);
  await assert.rejects(transport.send('Runtime.evaluate', { awaitPromise: true }), /CDP Runtime.evaluate deadline exceeded/);
  transport.close();
});

for (const event of ['close', 'error']) {
  test(`CDP socket ${event} rejects every pending request and future sends`, async () => {
    const socket = new FakeSocket();
    const transport = cdpTransport(socket, 1000);
    const first = assert.rejects(transport.send('First'), /CDP socket/);
    const second = assert.rejects(transport.send('Second'), /CDP socket/);
    socket.dispatchEvent(new Event(event));
    await Promise.all([first, second]);
    await assert.rejects(transport.send('Later'), /CDP socket/);
  });
}

test('DevTools startup timeout reaps its partial child with kill fallback before profile removal', async () => {
  const owned = child(), removed = [];
  owned.once('close', () => assert.equal(removed.length, 0));
  await assert.rejects(chrome('http://127.0.0.1/', dependencies(owned, removed)), /wait deadline exceeded/);
  assert.deepEqual(owned.signals, ['SIGTERM', 'SIGKILL']);
  assert.equal(removed.length, 1);
});

test('target-discovery failure cleans a launched child before returning a driver', async () => {
  const owned = child({ endpoint: true }), removed = [];
  await assert.rejects(chrome('http://127.0.0.1/', dependencies(owned, removed, {
    startupMs: 100, fetch: async () => ({ ok: false, status: 404 }),
  })), /Chrome target discovery failed: 404/);
  assert.deepEqual(owned.signals, ['SIGTERM', 'SIGKILL']);
  assert.equal(removed.length, 1);
});

test('WebSocket-open timeout cleans the partial socket, child and profile', async () => {
  const owned = child({ endpoint: true }), removed = [];
  let socketClosed = false;
  class NeverOpens extends FakeSocket { close() { socketClosed = true; super.close(); } }
  await assert.rejects(chrome('http://127.0.0.1/', dependencies(owned, removed, {
    startupMs: 100, fetch: async () => ({ ok: true, json: async () => [{ type: 'page', webSocketDebuggerUrl: 'ws://127.0.0.1/' }] }),
    WebSocket: NeverOpens,
  })), /CDP open deadline exceeded/);
  assert.equal(socketClosed, true);
  assert.deepEqual(owned.signals, ['SIGTERM', 'SIGKILL']);
  assert.equal(removed.length, 1);
});

test('profile is retained and cleanup fails explicitly if even SIGKILL does not reap the child', async () => {
  const owned = child({ reap: false }), removed = [];
  await assert.rejects(chrome('http://127.0.0.1/', dependencies(owned, removed)), error =>
    error instanceof AggregateError && error.errors.some(value => /Chrome reap deadline exceeded/.test(value.message)));
  assert.deepEqual(owned.signals, ['SIGTERM', 'SIGKILL']);
  assert.equal(removed.length, 0);
});
