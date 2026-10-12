'use strict';
const el = id => document.getElementById(id);
let csrf = '', selected = null, cursor = 0;
function status(message) { el('status').textContent = message; }
const bytes = value => Uint8Array.from(atob(value.replace(/-/g, '+').replace(/_/g, '/')), c => c.charCodeAt(0));
const b64 = value => btoa(String.fromCharCode(...new Uint8Array(value))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
async function api(path, body, method = 'POST', binary = false) {
  const response = await fetch('/operator/v1/' + path, { method, credentials: 'same-origin', redirect: 'error', cache: 'no-store', headers: { ...(body ? { 'Content-Type': 'application/json' } : {}), ...(csrf ? { 'X-CSRF': csrf } : {}) }, body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(15000) });
  if (!response.ok) { if (response.status === 401) { csrf = ''; el('signin').hidden = false; el('inbox').hidden = true; el('detail').hidden = true; } throw new Error('Request refused (' + response.status + ').'); }
  return binary ? response.blob() : response.json();
}
function options(challenge) {
  const publicKey = challenge.publicKey; publicKey.challenge = bytes(publicKey.challenge);
  if (publicKey.user) publicKey.user.id = bytes(publicKey.user.id);
  for (const key of ['allowCredentials', 'excludeCredentials']) for (const item of publicKey[key] || []) item.id = bytes(item.id);
  return { publicKey };
}
function credential(value) {
  const response = { clientDataJSON: b64(value.response.clientDataJSON) };
  if (value.response.attestationObject) { response.attestationObject = b64(value.response.attestationObject); response.transports = value.response.getTransports ? value.response.getTransports() : []; }
  else { response.authenticatorData = b64(value.response.authenticatorData); response.signature = b64(value.response.signature); if (value.response.userHandle) response.userHandle = b64(value.response.userHandle); }
  return { id: value.id, rawId: b64(value.rawId), type: value.type, response, extensions: value.getClientExtensionResults() };
}
async function ceremony(event, enrolling) {
  event.preventDefault(); const button = event.currentTarget.querySelector('button'); button.disabled = true;
  try {
    const token = el('token').value;
    const start = await api('auth/' + (enrolling ? 'enroll-start' : 'login-start'), enrolling ? { token } : { name: el('name').value });
    const result = await navigator.credentials[enrolling ? 'create' : 'get'](options(start.challenge));
    if (!result) throw new Error('Passkey ceremony canceled.');
    const finish = await api('auth/' + (enrolling ? 'enroll-finish' : 'login-finish'), { id: start.id, credential: credential(result), ...(enrolling ? { token } : {}) });
    if (enrolling) { el('token').value = ''; status('Passkey enrolled. Sign in with the operator name.'); }
    else { csrf = finish.csrf; el('signin').hidden = true; el('inbox').hidden = false; status('Authenticated. Session lasts 15 minutes.'); await list(false); }
  } catch (_) { status('Sign-in could not complete. Check your operator name or enrollment token and try again.'); } finally { button.disabled = false; }
}
async function list(more) {
  if (!more) { cursor = 0; el('rows').replaceChildren(); }
  const query = new URLSearchParams({ offset: String(cursor), reference: el('search').value, platform: el('platform').value, label: el('label-filter').value, build: el('build').value, category: el('category').value, since: String(el('since').value ? Math.floor(new Date(el('since').value).getTime() / 1000) : 0), until: String(el('until').value ? Math.floor(new Date(el('until').value).getTime() / 1000) + 86399 : 0) });
  const result = await api('reports?' + query, null, 'GET');
  for (const report of result.reports) {
    const row = document.createElement('tr'), first = document.createElement('td'), button = document.createElement('button'); button.textContent = report.reference; button.addEventListener('click', () => view(report)); first.append(button); row.append(first);
    for (const value of [report.platform + ' / ' + report.build, report.category, new Date(report.created * 1000).toLocaleString(), report.label]) { const cell = document.createElement('td'); cell.textContent = value; row.append(cell); }
    el('rows').append(row);
  }
  cursor = result.next_offset || 0; el('more').hidden = !result.next_offset;
}
async function view(report) {
  try { const result = await api('reports/' + report.id, null, 'GET'); selected = report; el('detail-title').textContent = report.reference; el('summary').textContent = result.summary; el('description').textContent = result.description; el('timeline').textContent = JSON.stringify(result.manifest.components, null, 2) + '\n\n' + result.timeline.map(v => v.name + '\n' + v.text).join('\n\n'); el('label').value = report.label; el('detail').hidden = false; el('detail').scrollIntoView({ behavior: 'smooth' }); } catch (_) { status('Report could not be opened. It may have expired or been deleted.'); }
}
el('login').addEventListener('submit', event => ceremony(event, false)); el('enroll').addEventListener('submit', event => ceremony(event, true));
el('filters').addEventListener('submit', event => { event.preventDefault(); list(false).catch(() => status('Inbox request failed.')); }); el('more').addEventListener('click', () => list(true).catch(() => status('Inbox request failed.')));
el('download').addEventListener('click', async () => { if (!selected) return; try { const blob = await api('reports/' + selected.id + '/download', null, 'GET', true); const url = URL.createObjectURL(blob), link = document.createElement('a'); link.href = url; link.download = selected.reference + '.zip'; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); } catch (_) { status('Archive download refused.'); } });
for (const [form, path, body] of [['label-form', 'label', () => ({ label: el('label').value })], ['retention-form', 'retention', () => ({ days: Number(el('days').value) })]]) el(form).addEventListener('submit', async event => { event.preventDefault(); if (!selected) return; try { await api('reports/' + selected.id + '/' + path, body()); status('Report updated.'); await list(false); } catch (_) { status('Update refused.'); } });
el('delete').addEventListener('click', async () => { if (!selected || !confirm('Permanently delete this report and revoke its capabilities?')) return; try { await api('reports/' + selected.id, null, 'DELETE'); selected = null; el('detail').hidden = true; status('Report deleted.'); await list(false); } catch (_) { status('Deletion could not complete.'); } });
el('invitation').addEventListener('submit', async event => { event.preventDefault(); try { const result = await api('invitations', { publisher: el('publisher').value }); el('invitation-result').textContent = 'One-use upload code: ' + result.code + '. Expires in 10 minutes.'; } catch (_) { status('Invitation creation refused.'); } });
