'use strict';
// This fixture has no uploader, shell, network client, or credential handling.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
function requireFact(value) { if (!value) throw new Error('Invalid capability fixture identity'); }
function codepointCompare(a, b) {
  const x = Array.from(a, c => c.codePointAt(0)), y = Array.from(b, c => c.codePointAt(0));
  for (let i = 0; i < Math.min(x.length, y.length); i++) if (x[i] !== y[i]) return x[i] - y[i];
  return x.length - y.length;
}
function canonical(value) {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value === 'number') { requireFact(Number.isSafeInteger(value)); return JSON.stringify(value); }
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
  requireFact(typeof value === 'object');
  return '{' + Object.keys(value).sort(codepointCompare).map(k => JSON.stringify(k) + ':' + canonical(value[k])).join(',') + '}';
}
try {
  requireFact(process.execArgv.length === 0 && ['NODE_OPTIONS','NODE_PATH','LD_PRELOAD','DYLD_INSERT_LIBRARIES'].every(k => !process.env[k]));
  const raw = fs.readFileSync(process.env.GITHUB_EVENT_PATH);
  requireFact(raw.length <= 8 * 1024 * 1024);
  const event = JSON.parse(raw.toString('utf8'));
  requireFact(event.inputs.operation === 'probe' && event.inputs.mode === 'unit');
  const control = JSON.parse(event.inputs.control), registration = control.registration;
  requireFact(['success', 'failure', 'post_failure'].includes(control.probe_case));
  requireFact(/^[0-9a-f]{64}$/.test(control.probe_nonce));
  requireFact(/^[0-9a-f-]{36}$/.test(control.session_id));
  requireFact(/^[1-9][0-9]*$/.test(process.env.GITHUB_RUN_ID));
  const registrationSha = sha(Buffer.from(canonical(registration), 'utf8'));
  requireFact(registrationSha === event.inputs.registration_sha256);
  const executable = fs.realpathSync(process.execPath);
  const runtime = {path: executable, sha256: sha(fs.readFileSync(executable)), argv: ['--version'], version: process.version};
  // An unmatched proposed pin remains unadmitted; this observation grants no capability.
  fs.writeSync(1, 'ASYNC-PROOF-CAPABILITY-NODE-RUNTIME ' + canonical({registration_sha256: registrationSha,
    nonce: control.probe_nonce, case: control.probe_case, runtime, acceptance: false}) + '\n');
  requireFact(canonical(runtime) === canonical(registration.runtime.action_node));
  requireFact(path.isAbsolute(registration.storage.root) && !registration.storage.root.split('/').includes('..'));
  const binding = path.join(registration.storage.root, control.session_id, 'run-' + process.env.GITHUB_RUN_ID,
    'capability-frame-binding.json');
  const state = {variant: control.probe_case, nonce: control.probe_nonce, registration: registrationSha,
    archive: registration.source_archive_sha256, run: process.env.GITHUB_RUN_ID,
    binding: Buffer.from(binding).toString('base64'), node: Buffer.from(canonical(runtime)).toString('base64')};
  fs.appendFileSync(process.env.GITHUB_STATE, Object.entries(state).map(([k,v]) => k + '=' + v + '\n').join(''));
} catch (_) {
  fs.writeSync(2, 'Async capability fixture identity unavailable\n');
  process.exitCode = 1;
}
