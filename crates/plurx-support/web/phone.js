'use strict';
// Remove bearer material before rendering, fetching, or following any link.
let claimSecret = new URLSearchParams(location.hash.slice(1)).get('handoff');
history.replaceState(null, '', location.pathname);
const el = id => document.getElementById(id);
const secret = () => Array.from(crypto.getRandomValues(new Uint8Array(32)), b => b.toString(16).padStart(2, '0')).join('');
let session;
try { session = JSON.parse(sessionStorage.getItem('cinema-handoff') || 'null'); } catch (_) { session = null; }
if (!session || session.expires < Date.now() || !/^[a-f0-9]{64}$/.test(session.proof || '')) session = { proof: secret(), expires: Date.now() + 1800000 };
let stopped = false, pollTimer, busy = false, preview;
function save() { try { sessionStorage.setItem('cinema-handoff', JSON.stringify(session)); } catch (_) { el('status').textContent = 'Browser storage is unavailable. Keep this page open to finish.'; } }
function status(message) { el('status').textContent = message; }
function fail(error) { status(error.code === 'expired' ? 'This handoff expired. Create a new one on the TV.' : error.code === 'state_or_content_conflict' ? 'Another phone claimed this handoff, or its state changed. Check your TV.' : 'The request could not complete. Your draft is retained until its expiry. Try again.'); }
async function api(path, body, grant, method = 'POST') {
  const response = await fetch('/support/v1/' + path, { method, credentials: 'omit', redirect: 'error', cache: 'no-store', referrerPolicy: 'no-referrer', headers: { ...(body ? { 'Content-Type': 'application/json' } : {}), ...(grant ? { Authorization: 'Bearer ' + grant } : {}) }, body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(10000) });
  const value = response.status === 204 ? {} : await response.json();
  if (!response.ok) throw { code: value.error || 'unavailable' };
  return value;
}
async function claim(code) {
  if (busy) return; busy = true; el('claim').querySelector('button').disabled = true;
  try {
    const value = await api('handoffs/claim', { ...(claimSecret ? { claim_secret: claimSecret } : { code: code.toUpperCase().replace(/\s/g, '') }), proof: session.proof });
    claimSecret = null; session.id = value.id; session.expires = value.expires_at * 1000; save();
    el('phrase').textContent = value.comparison; el('comparison').hidden = false; el('claim').hidden = true;
    status('Waiting for approval on your TV.'); poll();
  } catch (error) { fail(error); } finally { busy = false; el('claim').querySelector('button').disabled = false; }
}
async function poll() {
  if (stopped || session.expires < Date.now()) { status('This handoff expired. Create a new one on the TV.'); return; }
  try {
    const value = await api('handoffs/' + session.id + '/phone-status', null, session.proof, 'GET');
    if (value.state === 'submitted') { showReceipt(value.reference); return; }
    if (value.management_grant) {
      session.report = value.report_id; session.grant = value.management_grant; session.expires = value.expires_at * 1000; save();
      stopped = true; await load(); return;
    }
    if (['revoked', 'rejected', 'expired'].includes(value.state)) { stopped = true; status('Your TV canceled this handoff.'); return; }
  } catch (error) { if (['authorization_required', 'expired', 'not_found'].includes(error.code)) { stopped = true; fail(error); return; } status('Connection interrupted. Waiting for your TV’s approval.'); }
  pollTimer = setTimeout(poll, 2000);
}
async function load() {
  preview = await api('reports/' + session.report + '/preview', null, session.grant, 'GET');
  el('summary').textContent = preview.summary; el('description').value = preview.description || '';
  el('titles').checked = preview.privacy.titles_included; el('titles').disabled = !preview.privacy.titles_included;
  el('attachments').checked = preview.privacy.attachments_included; el('attachments').disabled = !preview.privacy.attachments_included;
  el('comparison').hidden = true; el('claim').hidden = true; el('report').hidden = false; status('TV approved. Review the report before sending.');
}
function showReceipt(reference) { stopped = true; clearTimeout(pollTimer); el('report').hidden = true; el('comparison').hidden = true; el('done').hidden = false; el('receipt').textContent = reference ? 'Support reference: ' + reference : 'The report was submitted.'; status('Report sent.'); sessionStorage.removeItem('cinema-handoff'); session.grant = null; }
el('claim').addEventListener('submit', event => { event.preventDefault(); claim(el('code').value); });
el('submit').addEventListener('submit', async event => {
  event.preventDefault(); if (busy) return; busy = true; const button = event.currentTarget.querySelector('button'); button.disabled = true;
  try {
    const privacy = { titles_included: el('titles').checked, attachments_included: el('attachments').checked };
    if (privacy.titles_included !== preview.privacy.titles_included || privacy.attachments_included !== preview.privacy.attachments_included) { await api('reports/' + session.report + '/redact', privacy, session.grant); preview.privacy = privacy; }
    const value = await api('reports/' + session.report + '/submit', { description: el('description').value }, session.grant); showReceipt(value.reference);
  } catch (error) { fail(error); } finally { busy = false; button.disabled = false; }
});
el('cancel').addEventListener('click', async () => { if (busy || !confirm('Delete the staged report? This cannot be undone.')) return; busy = true; try { await api('reports/' + session.report, null, session.grant, 'DELETE'); stopped = true; sessionStorage.removeItem('cinema-handoff'); el('report').hidden = true; status('Staged report deleted.'); } catch (error) { fail(error); } finally { busy = false; } });
addEventListener('pagehide', () => { stopped = true; clearTimeout(pollTimer); });
save();
if (claimSecret && /^[a-f0-9]{64}$/.test(claimSecret)) claim();
else if (session.id) { stopped = false; poll(); }
