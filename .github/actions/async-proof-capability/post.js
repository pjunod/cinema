'use strict';
const fs = require('node:fs');
const crypto = require('node:crypto');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
function requireFact(value) { if (!value) throw new Error('Invalid capability post identity'); }
try {
  requireFact(process.execArgv.length === 0 && ['NODE_OPTIONS','NODE_PATH','LD_PRELOAD','DYLD_INSERT_LIBRARIES'].every(k => !process.env[k]));
  const variant = process.env.STATE_variant;
  requireFact(['success', 'failure', 'post_failure'].includes(variant));
  const nonce = process.env.STATE_nonce, registration = process.env.STATE_registration;
  requireFact(/^[0-9a-f]{64}$/.test(nonce) && /^[0-9a-f]{64}$/.test(registration));
  const runtime = JSON.parse(Buffer.from(process.env.STATE_node, 'base64').toString('utf8'));
  requireFact(fs.realpathSync(process.execPath) === runtime.path && process.version === runtime.version
    && sha(fs.readFileSync(runtime.path)) === runtime.sha256);
  const filename = Buffer.from(process.env.STATE_binding, 'base64').toString('utf8');
  // Missing finalizer output is an ordinary failed probe, never the expected negative.
  const info = fs.lstatSync(filename);
  requireFact(info.isFile() && !info.isSymbolicLink() && info.size <= 8192 && info.uid === process.getuid());
  const raw = fs.readFileSync(filename), binding = JSON.parse(raw.toString('utf8'));
  const keys = ['attempt','case','control_sha256','job','nonce','registration_sha256','run','schema_version','source_archive_sha256','task'];
  requireFact(JSON.stringify(Object.keys(binding).sort()) === JSON.stringify(keys));
  requireFact(raw.toString('utf8') === JSON.stringify(Object.fromEntries(keys.map(k => [k, binding[k]]))));
  requireFact(binding.schema_version === 1 && binding.case === variant && binding.nonce === nonce
    && binding.registration_sha256 === registration && binding.source_archive_sha256 === process.env.STATE_archive
    && String(binding.run) === process.env.STATE_run && binding.attempt === 1
    && Number.isSafeInteger(binding.job) && binding.job > 0 && Number.isSafeInteger(binding.task) && binding.task > 0
    && /^[0-9a-f]{64}$/.test(binding.control_sha256));
  fs.unlinkSync(filename); // Only this fixture's bounded private handoff, never proof evidence.
  if (variant === 'post_failure') {
    fs.writeSync(1, 'ASYNC-PROOF-CAPABILITY-EXPECTED-POST-FAILURE 1 ' + nonce + ' ' + binding.control_sha256 + '\n');
    process.exitCode = 1;
  }
} catch (_) {
  fs.writeSync(2, 'Async capability post fixture did not reach its bound expected cause\n');
  process.exitCode = 1;
}
