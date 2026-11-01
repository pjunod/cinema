'use strict';
/* ci-fleet console. Static, no build step, no libraries.
 * Every data string reaches the page through esc() (template strings) or textContent. */
(() => {

/* ============================== helpers ============================== */
const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ESC[c]);
const $ = (sel, root = document) => root.querySelector(sel);
const num = (v, d = 0) => (v === null || v === undefined || v === '' || !Number.isFinite(Number(v))) ? d : Number(v);
const has = v => v !== null && v !== undefined && Number.isFinite(Number(v));
const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
const pad2 = n => String(n).padStart(2, '0');
const GIB = 2 ** 30;
const gib = (b, d = 1) => (num(b) / GIB).toFixed(d);
const f1 = v => num(v).toFixed(1);
const cnt = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;
const pctOf = (v, t) => t > 0 ? clamp(v / t * 100, 0, 100).toFixed(2) : '0';

function dur(s) {
  if (!has(s)) return '—';
  s = Math.max(0, Math.round(s));
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${pad2(s % 60)}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${pad2(Math.floor(s % 3600 / 60))}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor(s % 86400 / 3600)}h`;
}
/* Times are shown in the viewer's local zone. */
const clockFmt = new Intl.DateTimeFormat([], { hourCycle: 'h23', hour: '2-digit', minute: '2-digit', second: '2-digit' });
const stampFmt = new Intl.DateTimeFormat([], { hourCycle: 'h23', hour: '2-digit', minute: '2-digit', second: '2-digit', timeZoneName: 'short' });
const dayFmt = new Intl.DateTimeFormat([], { weekday: 'short', day: 'numeric', month: 'short' });
const fmtClock = ts => has(ts) ? clockFmt.format(new Date(ts * 1000)) : '—';
const fmtDay = ts => has(ts) ? dayFmt.format(new Date(ts * 1000)) : '—';
const fmtStamp = ts => has(ts) ? `${fmtDay(ts)} · ${stampFmt.format(new Date(ts * 1000))}` : '—';
const shortDur = s => !has(s) ? '—' : s < 5 ? '≤ 5 s' : dur(s);

const safeUrl = u => /^https?:\/\/[^\s"'<>]+$/i.test(u || '') ? u.replace(/\/+$/, '') : '';
const store = {
  get(k) { try { return sessionStorage.getItem(k) || ''; } catch (e) { return ''; } },
  set(k, v) { try { v ? sessionStorage.setItem(k, v) : sessionStorage.removeItem(k); } catch (e) { /* harmless */ } },
};

const TOKEN_KEY = 'cifleet.token';
const POLL_MS = 5000;
const state = {
  snap: null, d: null, fetchedAt: 0, error: null,
  route: { name: 'fleet' }, job: null,
  q: { status: 'all', why: '', text: '' },
  token: '', who: null,
  forgeParam: safeUrl(new URLSearchParams(location.search).get('forge')) || safeUrl(store.get('cifleet.forge')),
};
if (state.forgeParam) store.set('cifleet.forge', state.forgeParam);

/* ============================== DOM patching ============================== */
/* Re-render by diffing the new markup into the live DOM, so focused elements,
 * the filter input and the scroll position survive a refresh. */
const skipAttr = (el, name) => (name === 'value' && /^(INPUT|TEXTAREA)$/.test(el.tagName)) || (name === 'open' && el.tagName === 'DETAILS');
const sameKind = (a, b) => a.nodeType === b.nodeType && (a.nodeType !== 1 || (a.localName === b.localName && a.namespaceURI === b.namespaceURI));
const keyOf = n => (n.nodeType === 1 && n.dataset) ? (n.dataset.key || '') : '';

function patchNode(o, n) {
  if (o.nodeType !== 1) { if (o.nodeValue !== n.nodeValue) o.nodeValue = n.nodeValue; return; }
  for (const a of [...o.attributes]) if (!n.hasAttribute(a.name) && !skipAttr(o, a.name)) o.removeAttribute(a.name);
  for (const a of n.attributes) if (!skipAttr(o, a.name) && o.getAttribute(a.name) !== a.value) o.setAttribute(a.name, a.value);
  if (o.tagName !== 'INPUT' && o.tagName !== 'TEXTAREA') patchChildren(o, n);
}
function patchChildren(o, n) {
  const next = [...n.childNodes];
  const keyed = new Map();
  for (const c of o.childNodes) { const k = keyOf(c); if (k) keyed.set(k, c); }
  next.forEach((nc, i) => {
    let cur = o.childNodes[i] || null;
    const key = keyOf(nc);
    if (key && keyed.has(key)) {
      const found = keyed.get(key);
      if (found !== cur) o.insertBefore(found, cur);
      cur = found;
    }
    if (cur && sameKind(cur, nc) && keyOf(cur) === key) patchNode(cur, nc);
    else o.insertBefore(document.importNode(nc, true), cur);
  });
  while (o.childNodes.length > next.length) o.lastChild.remove();
}
function patchInto(root, html) {
  const tpl = document.createElement('template');
  tpl.innerHTML = html; // html is built only from esc()-ed data
  patchChildren(root, tpl.content);
}

/* ============================== shared fragments ============================== */
const ICON_X = '<svg class="ico" aria-hidden="true" width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><path d="M2 2l6 6M8 2l-6 6"/></svg>';
const ICON_OK = '<svg class="ico" aria-hidden="true" width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M1.8 5.4l2.3 2.3 4.1-4.8"/></svg>';
const ICON_EXT = '<svg class="ico" aria-hidden="true" width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M9 2.5h4.5V7M13.5 2.5 7 9M12 10v3a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h3"/></svg>';

const kpi = (label, value, unit, sub, tone = '') =>
  `<div class="kpi"><span class="eyebrow">${esc(label)}</span><span class="kpi-v mono ${tone}">${esc(value)}<small>${esc(unit)}</small></span><span class="dim small">${esc(sub)}</span></div>`;
const badge = (text, tone = '') => `<span class="badge ${tone}">${esc(text)}</span>`;
const dot = k => `<span class="dot ${k}" aria-hidden="true"></span>`;
const labelChip = (l, miss) => `<span class="chip ${miss ? 'miss' : ''}">${miss ? ICON_X : ''}${esc(l)}${miss ? '<span class="sr"> (missing)</span>' : ''}</span>`;
const emptyBox = t => `<div class="empty">${esc(t)}</div>`;
const mini = (frac, tone, label, tick) =>
  `<span class="mini" role="img" aria-label="${esc(label)}"><span class="${tone}" style="width:${pctOf(frac, 1)}%"></span>${tick == null ? '' : `<i class="ptick" style="left:${pctOf(tick, 1)}%"></i>`}</span>`;

function controlBtn(o) {
  return `<button type="button" class="btn ${o.cls || ''}" data-act="control" data-action="${esc(o.action)}" data-target="${esc(o.target)}" data-title="${esc(o.title || o.label)}" data-desc="${esc(o.desc || '')}" data-confirm="${esc(o.confirm || o.label)}">${esc(o.label)}</button>`;
}
const repoUrl = () => {
  const f = (state.snap && state.snap.forge) || {};
  return safeUrl(f.repo_url || (f.url && f.repo ? `${String(f.url).replace(/\/+$/, '')}/${f.repo}` : '')) || state.forgeParam;
};
function forgeLink(path, label, cls = 'btn') {
  const base = repoUrl();
  if (!base) return `<button type="button" class="${cls}" disabled title="The collector does not publish the Forgejo repository URL (forge.repo_url), and no ?forge= address was given.">${esc(label)}</button>`;
  return `<a class="${cls}" href="${esc(base + path)}" target="_blank" rel="noopener noreferrer">${esc(label)}${ICON_EXT}</a>`;
}
function prLink(pr) {
  if (!pr) return '';
  const n = String(pr).replace(/^#/, '');
  const base = repoUrl();
  if (base && /^\d+$/.test(n)) return `<a href="${esc(`${base}/pulls/${n}`)}" target="_blank" rel="noopener noreferrer">PR #${esc(n)}</a>`;
  return `<span>PR #${esc(n)}</span>`;
}
const HIDE_LABELS = /^(self-hosted|lab|Linux|X64|ARM64|macOS|incus|ubuntu-[\d.]+)$/;
const niceLabels = ls => (ls || []).filter(l => !HIDE_LABELS.test(l));
const shortRunner = r => r.name.startsWith(`gha-${r.host}-`) ? r.name.slice(`gha-${r.host}-`.length) : r.name;
const eligibleFor = (r, need) => (need || []).every(l => (r.labels || []).includes(l));
const median = a => { const s = a.filter(has).map(Number).sort((x, y) => x - y); return s.length ? s[Math.floor(s.length / 2)] : null; };

function derive(s) {
  const d = {};
  d.runners = s.runners || [];
  d.runnerBy = new Map(d.runners.map(r => [r.name, r]));
  d.hostBy = new Map((s.hosts || []).map(h => [h.name, h]));
  d.stale = new Set((s.drift || []).filter(f => f.kind === 'stale').flatMap(f => (f.runners || []).map(r => typeof r === 'string' ? r : r.runner)));
  const jobs = s.jobs || {};
  d.running = jobs.running || []; d.waiting = jobs.waiting || []; d.blocked = jobs.blocked || [];
  d.runningByTask = new Map(d.running.filter(j => j.task_id).map(j => [j.task_id, j]));
  const online = d.runners.filter(r => r.online);
  d.slots = online.reduce((n, r) => n + num(r.capacity, 1), 0);
  d.busySlots = online.reduce((n, r) => n + Math.min(num(r.busy), num(r.capacity, 1)), 0);
  return d;
}

/* ============================== FLEET ============================== */
function alertView(a) {
  const code = t => `<code>${esc(t)}</code>`;
  switch (a.kind) {
    case 'unsatisfiable': {
      const ls = (a.labels || []).map(code).join(', ');
      return { tag: 'CAN NEVER START', link: ['#queue', 'Open in Queue'],
        html: `${cnt(num(a.count), 'job')} ${num(a.count) === 1 ? 'needs' : 'need'} ${ls || 'labels no runner has'}. No registered runner carries ${(a.labels || []).length === 1 ? 'it' : 'them'}.${num(a.oldest) ? ` Oldest has waited ${esc(dur(a.oldest))}.` : ''}` };
    }
    case 'pool_offline':
      return { tag: 'POOL OFFLINE', link: ['#pools', 'Open in Pools'],
        html: `${code(a.pool)} has 0 of ${esc(a.registered)} registered runners online.${a.next_schedule ? ` A scheduled run at ${esc(fmtStamp(a.next_schedule))} will queue with nowhere to run.` : ' Any job sent there will sit in the queue.'}` };
    case 'pool_degraded':
      return { tag: 'POOL DEGRADED', link: ['#pools', 'Open in Pools'], html: `${code(a.pool)} has ${esc(a.online)} of ${esc(a.registered)} runners online.` };
    default: {
      const rest = Object.entries(a).filter(([k]) => k !== 'severity' && k !== 'kind').map(([k, v]) => `${k}: ${typeof v === 'object' ? JSON.stringify(v) : v}`).join(' · ');
      return { tag: String(a.kind || 'ALERT').toUpperCase().replace(/_/g, ' '), link: ['#drift', 'Open in Drift'], html: esc(rest) };
    }
  }
}
function alertsHtml(s) {
  const list = s.alerts || [];
  if (!list.length) return '';
  return `<section aria-label="Alerts" class="alerts">${list.map(a => {
    const v = alertView(a);
    return `<div class="alert ${a.severity === 'critical' ? 'critical' : ''}"><span class="tag">${esc(v.tag)}</span><span class="msg">${v.html}</span><a class="btn" href="${esc(v.link[0])}">${esc(v.link[1])}</a></div>`;
  }).join('')}</section>`;
}

function meterHtml(label, valueText, parts, total, note, reserveFrac) {
  const segs = parts.map(p => `<span class="seg ${p.cls}" style="width:${pctOf(p.v, total)}%" title="${esc(`${p.name}: ${p.text}`)}"></span>`).join('');
  const tick = reserveFrac == null ? '' : `<span class="tick" style="left:${pctOf(reserveFrac, 1)}%" title="disk reserve"></span>`;
  return `<div class="meter"><div class="meter-top"><span class="dim">${esc(label)}</span><span class="mono">${esc(valueText)}</span></div>` +
    `<div class="bar" role="img" aria-label="${esc(`${label}: ${valueText}. ${note}`)}">${segs}${tick}</div><div class="note">${esc(note)}</div></div>`;
}

function hostMeters(m) {
  const cu = m.cpu_used || {}, mu = m.mem_used || {};
  const cores = num(m.cpu_cores), cpuSum = num(cu.ci) + num(cu.plurx) + num(cu.other);
  const out = [meterHtml('CPU', `${f1(has(m.cpu_busy) ? m.cpu_busy : cpuSum)} / ${cores} cores`,
    [{ cls: 'ci', name: 'CI', v: num(cu.ci), text: f1(cu.ci) + ' cores' }, { cls: 'plurx', name: 'plurx', v: num(cu.plurx), text: f1(cu.plurx) + ' cores' }, { cls: 'other', name: 'other', v: num(cu.other), text: f1(cu.other) + ' cores' }],
    cores, `CI ${f1(cu.ci)} · plurx ${f1(cu.plurx)} · other ${f1(cu.other)} · free ${f1(Math.max(0, cores - cpuSum))}`)];
  const mt = num(m.mem_total), memSum = num(mu.ci) + num(mu.plurx) + num(mu.other);
  out.push(meterHtml('Memory', `${gib(memSum)} / ${gib(mt)} GiB`,
    [{ cls: 'ci', name: 'CI', v: num(mu.ci), text: gib(mu.ci) + ' GiB' }, { cls: 'plurx', name: 'plurx', v: num(mu.plurx), text: gib(mu.plurx) + ' GiB' }, { cls: 'other', name: 'other', v: num(mu.other), text: gib(mu.other) + ' GiB' }],
    mt, `CI ${gib(mu.ci)} · plurx ${gib(mu.plurx)} · other ${gib(mu.other)} · free ${gib(Math.max(0, mt - memSum))} GiB${num(m.swap_used) ? ` · swap ${gib(m.swap_used)} GiB` : ''}`));
  for (const dk of m.disks || []) {
    const total = num(dk.total), used = num(dk.used);
    const ci = num(dk.ci), px = num(dk.plurx), other = has(dk.other) ? num(dk.other) : Math.max(0, used - ci - px);
    const below = has(dk.free) && has(dk.reserve) && num(dk.free) < num(dk.reserve);
    out.push(meterHtml(`Disk ${dk.path || '/'}`, `${gib(used, 0)} / ${gib(total, 0)} GiB`,
      [{ cls: 'ci', name: 'CI', v: ci, text: gib(ci, 0) + ' GiB' }, { cls: 'plurx', name: 'plurx', v: px, text: gib(px, 0) + ' GiB' }, { cls: 'other', name: ci || px ? 'other' : 'used', v: other, text: gib(other, 0) + ' GiB' }],
      total, `free ${gib(dk.free, 0)} GiB · reserve ${gib(dk.reserve, 0)} GiB${below ? ' · BELOW RESERVE' : ''}`,
      total > 0 && has(dk.reserve) ? 1 - num(dk.reserve) / total : null));
  }
  for (const g of m.gpus || []) out.push(`<div class="meter-top"><span class="dim">GPU ${esc(g.name)}</span><span class="mono">${esc(g.util)}% · ${esc(gib(g.mem_used))} / ${esc(gib(g.mem_total))} GiB</span></div>`);
  return out.join('');
}

function usageText(u) {
  if (!u) return '';
  const p = [];
  if (has(u.cpu)) p.push(`cpu ${f1(u.cpu)}${has(u.cpu_cap) ? ` / ${f1(u.cpu_cap)}` : ''}`);
  if (has(u.rss)) p.push(`rss ${gib(u.rss)}${has(u.mem_cap) ? ` / ${gib(u.mem_cap)}` : ''} GiB`);
  if (num(u.swap)) p.push(`swap ${gib(u.swap)} GiB`);
  if (has(u.oom_kills)) p.push(`${u.oom_kills} OOM`);
  return p.join(' · ');
}
function runnerState(r, d) {
  if (!r.online) return d.stale.has(r.name) ? { k: 'stale', word: 'offline · stale registration', tone: 'coral' } : { k: 'off', word: 'offline', tone: '' };
  if (r.paused) return { k: 'paused', word: 'paused', tone: 'amber' };
  if (num(r.busy) > 0) return { k: 'busy', word: 'busy', tone: '' };
  return { k: 'idle', word: 'idle', tone: '' };
}

function slotRow(r, s, d) {
  const st = runnerState(r, d);
  let state_ = st.word;
  if (st.k === 'off' || st.k === 'stale') state_ = `${st.k === 'stale' ? 'offline · stale registration' : 'offline'}${r.offline_reason ? ' · ' + r.offline_reason : ''}`;
  else if (st.k === 'idle' && has(r.idle_since)) state_ = `idle · ${dur(s.generated - r.idle_since)}`;
  const job = (r.tasks || []).length ? d.runningByTask.get(r.tasks[0]) : null;
  let body = '';
  if (st.k === 'busy' || st.k === 'paused' && job) {
    if (job) {
      const scale = Math.max(num(job.p50) * 1.25, num(job.elapsed) * 1.05, 1);
      body = `<a class="jobline" href="#job/${esc(job.task_id)}"><span class="r"><span><strong>${esc(job.name)}</strong> <span class="blue">${esc(job.pr || '')}</span></span><span class="mono dim small">${esc(dur(job.elapsed))}${job.p50 ? ` · p50 ${esc(dur(job.p50))}` : ''}</span></span>` +
        mini(num(job.elapsed) / scale, '', `Elapsed ${dur(job.elapsed)}${job.p50 ? ` of a p50 of ${dur(job.p50)}` : ''}`, job.p50 ? num(job.p50) / scale : null) +
        `<span class="mono dim small">${esc(usageText(r.usage) || 'no usage reported')}</span></a>`;
    } else state_ = 'busy · job not attributed';
  }
  const ctl = (r.controls || []).includes('drain') ? controlBtn({ action: 'drain', target: r.name, label: 'Drain', cls: 'sm', title: `Drain ${r.name}`, desc: 'Stops this runner taking new jobs once its current job finishes.', confirm: 'Drain' })
    : (r.controls || []).includes('resume') ? controlBtn({ action: 'resume', target: r.name, label: 'Resume', cls: 'sm', title: `Resume ${r.name}`, desc: 'Lets this runner take new jobs again.', confirm: 'Resume' }) : '';
  return `<div class="slot" data-key="r-${esc(r.name)}">${dot(st.k)}<span class="sname" title="${esc(r.name)}">${esc(shortRunner(r))}</span>` +
    `<span class="slabels" title="${esc((r.labels || []).join(', '))}">${esc(niceLabels(r.labels).join(' · '))}</span>` +
    `<span class="sstate ${st.tone}">${esc(state_)}</span>${r.paused ? '<span class="pill amber">paused</span>' : ''}${ctl}${body}</div>`;
}

function vmRows(h) {
  if (!(h.vms || []).length) return '';
  return `<div class="vms"><div class="slots-head eyebrow"><span>Runner VMs</span></div>${h.vms.map(v => {
    const up = String(v.status).toLowerCase() === 'running';
    const btn = (v.controls || []).includes('start_vm') ? controlBtn({ action: 'start_vm', target: v.name, label: 'Start VM', cls: 'sm', title: `Start VM ${v.name}`, desc: `Starts the stopped runner VM ${v.name} on ${h.name}.`, confirm: 'Start VM' }) : '';
    return `<div class="vm" data-key="vm-${esc(v.name)}">${dot(up ? 'idle' : 'off')}<span class="mono">${esc(v.name)}</span><span class="dim">${esc(String(v.status || 'unknown').toLowerCase())}</span>` +
      `${v.memory_limit ? `<span class="dim mono small">${esc(v.memory_limit)}</span>` : ''}${v.paused ? '<span class="pill amber">watchdog paused</span>' : ''}${btn}</div>`;
  }).join('')}</div>`;
}

function hostCard(h, s, d) {
  const m = h.metrics;
  const rs = (h.runners || []).map(n => d.runnerBy.get(n)).filter(Boolean).sort((a, b) => a.name.localeCompare(b.name));
  const nBusy = rs.filter(r => r.online && num(r.busy) > 0).length, nOff = rs.filter(r => !r.online).length, nIdle = rs.length - nBusy - nOff;
  let spec = '', body, warn = '', loaded = false;
  if (m) {
    spec = `${num(m.cpu_cores)} cores · ${gib(m.mem_total, 0)} GiB${(h.vms || []).length ? ` · ${cnt(h.vms.length, 'VM')}` : ''}`;
    const cores = num(m.cpu_cores), cu = m.cpu_used || {};
    const foreign = cores ? (num(cu.plurx) + num(cu.other)) / cores : 0;
    if (foreign > 0.4) { loaded = true; warn = `<div class="warnline">Non-CI load at ${Math.round(foreign * 100)}% of CPU (plurx + other). Jobs placed here will run slower than on an idle host.</div>`; }
    body = `<div class="meters">${hostMeters(m)}</div>`;
  } else body = '<div class="noagent">no agent reporting</div>';
  const age = has(h.agent_age) ? `<span>agent reported ${esc(dur(h.agent_age))} ago</span>` : '';
  return `<article class="card host ${loaded ? 'loaded' : ''}" data-key="h-${esc(h.name)}" aria-label="Host ${esc(h.name)}">` +
    `<div class="hosthead"><div><div class="hostname">${esc(h.name)}${h.alias && h.alias !== h.name ? ` <small>· ${esc(h.alias)}</small>` : ''}</div><div class="dim small">${esc(spec)}${spec && age ? ' · ' : ''}${age}</div></div>` +
    `<div class="chips">${h.voter ? '<span class="pill voter">plurx voter</span>' : '<span class="pill">non-voter</span>'}</div></div>` +
    `${body}${warn}${vmRows(h)}` +
    `<div class="slots"><div class="slots-head eyebrow"><span>Runner slots</span><span class="small" style="text-transform:none;letter-spacing:0">${nBusy} busy · ${nIdle} idle · ${nOff} offline</span></div>` +
    `${rs.length ? rs.map(r => slotRow(r, s, d)).join('') : '<div class="dim small pad">no runners registered</div>'}</div></article>`;
}

function renderFleet(s, d) {
  const c = s.counts || {};
  const unsat = num(c.unsatisfiable);
  const pctNow = d.slots ? Math.round(d.busySlots / d.slots * 100) : 0;
  const kpis = [
    kpi('Running', num(c.running, d.running.length), '', d.running.length ? d.running.map(j => j.name).slice(0, 2).join(', ') : 'nothing running', d.running.length ? 'blue' : ''),
    kpi('Waiting', num(c.waiting, d.waiting.length), '', unsat ? `${unsat} unsatisfiable` : `${num(c.blocked, d.blocked.length)} more blocked on needs`, unsat ? 'coral' : ''),
    kpi('Oldest wait', num(s.oldest_wait) ? dur(s.oldest_wait) : 'none', '', 'longest ready-but-unstarted job', num(s.oldest_wait) > 300 ? 'coral' : ''),
    kpi('Runners online', num(c.online), ` / ${num(c.registered)}`, `${num(c.offline)} offline · ${num(c.stale)} stale`),
    kpi('Slots busy', d.busySlots, ` / ${d.slots}`, `${pctNow}% now`),
  ].join('');
  const hosts = s.hosts || [];
  return `<div class="stack">${alertsHtml(s)}<section aria-label="Summary" class="kpis">${kpis}</section>` +
    `<section aria-labelledby="hosts-h"><div class="sechead"><h2 id="hosts-h">Hosts <span class="sub">· ${cnt(hosts.length, 'host')} · ${num(c.registered, d.runners.length)} runners registered</span></h2><div class="grow"></div>` +
    `<div class="legend"><span><i class="sw ci"></i>CI jobs</span><span><i class="sw plurx"></i>plurx production</span><span><i class="sw other"></i>other host load</span><span><i class="sw free"></i>free</span><span><i class="sw reserve"></i>disk reserve</span></div></div>` +
    `<div class="hosts">${hosts.map(h => hostCard(h, s, d)).join('')}</div></section></div>`;
}

/* ============================== QUEUE ============================== */
const WHY = [['unsatisfiable', 'Unsatisfiable', 'coral'], ['offline_only', 'Offline only', 'coral'], ['busy', 'Busy', 'blue'], ['not_picked_up', 'Not picked up', 'amber'], ['queued', 'Queued', ''], ['held', 'Held', 'amber'], ['blocked', 'Blocked', 'amber']];
const whyLabel = k => (WHY.find(w => w[0] === k) || [k, String(k || 'unknown').replace(/_/g, ' ')])[1];
const whyTone = k => (WHY.find(w => w[0] === k) || [])[2] || '';
const FLAG_TEXT = { dead_on_pickup: 'DEAD ON PICKUP', superseded: 'SUPERSEDED', loaded_host: 'LOADED HOST' };

function matches(j, text) {
  const terms = String(text || '').toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return true;
  const hay = [j.name, j.pr, `#${j.run_index}`, `run ${j.run_index}`, j.title, j.workflow, j.pool, j.runner, j.host, j.reason, j.class, ...(j.runs_on || [])].join(' ').toLowerCase();
  return terms.every(t => hay.includes(t));
}
function chipBtn(act, v, label, count, pressed, cls = '') {
  return `<button type="button" class="fchip ${cls}" data-act="${act}" data-v="${esc(v)}" aria-pressed="${pressed}">${esc(label)} <span class="mono">${esc(count)}</span></button>`;
}

function classDetails(j) {
  const need = j.runs_on || [];
  if (j.class === 'unsatisfiable' && (j.closest || []).length) {
    return `<div class="closest"><div class="eyebrow ttl">Closest runners</div>${j.closest.map(c => {
      const miss = new Set(c.missing || []);
      return `<div class="row"><span class="mono small">${esc(c.runner)}</span><span class="mono dim small">${esc(c.has)} of ${need.length}</span>` +
        `<span class="chips">${need.map(l => `<span class="chip ${miss.has(l) ? 'miss' : 'have'}">${miss.has(l) ? ICON_X : ICON_OK}${esc(l)}${miss.has(l) ? '<span class="sr"> (missing)</span>' : '<span class="sr"> (has)</span>'}</span>`).join('')}</span></div>`;
    }).join('')}</div>`;
  }
  if (j.class === 'offline_only') {
    return `<div class="closest"><div class="eyebrow ttl">Eligible runners, all unavailable</div>${(j.offline || []).map(o => `<div class="row">${dot('off')}<span class="mono small">${esc(o.runner)}</span><span class="dim small">${esc(o.reason || 'offline')}</span></div>`).join('')}</div>`;
  }
  if (j.class === 'busy') {
    return `<div class="closest"><div class="eyebrow ttl">Eligible runners are all busy${has(j.eta) ? ` · soonest free about ${esc(fmtClock(j.eta))}` : ''}</div>${(j.busy || []).map(b => `<div class="row">${dot('busy')}<span class="mono small">${esc(b.runner)}</span><span class="small">running ${esc(b.job || 'a job')}</span><span class="dim small mono">${has(b.eta) ? `ETA ${esc(fmtClock(b.eta))}` : 'no ETA'}</span></div>`).join('')}</div>`;
  }
  if (j.class === 'not_picked_up') {
    return `<div class="infobox amber">Idle for 60 s or more without taking it: <span class="mono">${esc((j.idle || []).join(', '))}</span></div>`;
  }
  if ((j.eligible || []).length) return `<div class="infobox">Eligible runners: <span class="mono">${esc(j.eligible.join(', '))}</span></div>`;
  return '';
}

function whyText(j) {
  const L = [`${j.name} — ${j.pr ? 'PR ' + j.pr : 'no PR'} — run #${j.run_index} (${j.workflow || 'workflow ?'})`,
    j.title ? `title: ${j.title}` : '', `class: ${j.class}`, `reason: ${j.reason}`, `requires: ${(j.runs_on || []).join(', ')}`];
  if ((j.missing_labels || []).length) L.push(`missing labels: ${j.missing_labels.join(', ')}`);
  if (has(j.waited)) L.push(`waited: ${dur(j.waited)}`);
  if ((j.flags || []).length) L.push(`flags: ${j.flags.join(', ')}`);
  for (const n of j.notes || []) L.push(`note: ${n}`);
  for (const c of j.closest || []) L.push(`closest: ${c.runner} (${c.has} labels, missing ${(c.missing || []).join(', ') || 'none'})`);
  for (const o of j.offline || []) L.push(`offline: ${o.runner} — ${o.reason}`);
  for (const b of j.busy || []) L.push(`busy: ${b.runner} running ${b.job || '?'}${has(b.eta) ? ` until ${fmtClock(b.eta)}` : ''}`);
  if ((j.idle || []).length) L.push(`idle but not taking it: ${j.idle.join(', ')}`);
  L.push(`run id (API): ${j.run_id}`);
  return L.filter(Boolean).join('\n');
}

function waitingCard(j) {
  const miss = new Set(j.missing_labels || []);
  const flags = (j.flags || []).map(f => badge(FLAG_TEXT[f] || String(f).toUpperCase().replace(/_/g, ' '), 'amber')).join(' ');
  const cancelable = has(j.run_id);
  return `<article class="card wcard ${j.class === 'unsatisfiable' ? 'coral' : ''}" data-key="w-${esc(j.job_id)}" aria-label="${esc(j.name)}, ${esc(whyLabel(j.class))}">` +
    `<div class="wmain"><div><h3>${esc(j.name)}</h3><div class="meta">${prLink(j.pr)}${j.title ? `<span>${esc(j.title)}</span>` : ''}<span>run #${esc(j.run_index)}</span><span>${esc(j.workflow || '')}</span></div></div>` +
    `<div><div class="eyebrow">Requires · runs-on</div><div class="chips">${(j.runs_on || []).map(l => labelChip(l, miss.has(l))).join('')}</div></div>` +
    `<p>${esc(j.reason)}</p>${flags ? `<div class="chips">${flags}</div>` : ''}` +
    `${(j.notes || []).length ? `<ul class="notes">${j.notes.map(n => `<li>${esc(n)}</li>`).join('')}</ul>` : ''}${classDetails(j)}</div>` +
    `<div class="wside">${badge(whyLabel(j.class).toUpperCase(), whyTone(j.class))}` +
    `<div><div class="eyebrow">Waiting</div><div class="age ${whyTone(j.class) === 'coral' ? 'coral' : ''}">${j.ready_exact === false ? '≤ ' : ''}${esc(has(j.waited) ? dur(j.waited) : '—')}</div>` +
    `<div class="mono dim small">${has(j.ready) ? (j.ready_exact === false ? `run started ${esc(fmtClock(j.ready))} · its needs' finish times are not recorded yet` : `ready since ${esc(fmtClock(j.ready))} (needs finished)`) : 'not ready yet (needs unfinished)'}</div></div>` +
    `<div class="btnrow">${cancelable ? controlBtn({ action: 'cancel_run', target: j.run_id, label: `Cancel run #${j.run_index}`, cls: 'danger', title: `Cancel run #${j.run_index}`, desc: `Cancels the whole Forgejo run #${j.run_index} (all of its jobs), not just "${j.name}".`, confirm: 'Cancel run' }) : ''}` +
    `<button type="button" class="btn" data-act="copy-why" data-job="${esc(j.job_id)}" title="Copies the diagnosis as plain text">Copy why</button>` +
    `${forgeLink(`/actions/runs/${encodeURIComponent(j.run_index)}`, 'Open in Forgejo')}</div>` +
    `<span class="note">Forgejo cancels whole runs, not single jobs.</span></div></article>`;
}

function blockedRow(j) {
  return `<div class="card brow" data-key="b-${esc(j.job_id)}">${badge(whyLabel(j.class).toUpperCase(), whyTone(j.class))}<strong>${esc(j.name)}</strong>${prLink(j.pr)}` +
    `<span class="mono dim small">run #${esc(j.run_index)}</span><span class="why">${esc(j.reason)}${(j.flags || []).map(f => ' ' + badge(FLAG_TEXT[f] || f.toUpperCase(), 'amber')).join('')}` +
    `${(j.notes || []).length ? `<div class="note">${esc(j.notes.join(' · '))}</div>` : ''}</span>` +
    `${has(j.run_id) ? controlBtn({ action: 'cancel_run', target: j.run_id, label: `Cancel run #${j.run_index}`, cls: 'sm danger', title: `Cancel run #${j.run_index}`, desc: `Cancels the whole Forgejo run #${j.run_index} (all of its jobs).`, confirm: 'Cancel run' }) : ''}</div>`;
}

function runningTable(list, s) {
  if (!list.length) return emptyBox('No jobs running.');
  return `<div class="tablewrap"><table class="tbl min1250"><thead><tr><th scope="col">Job</th><th scope="col">PR / run</th><th scope="col">Runner</th><th scope="col">Host</th><th scope="col">Started</th><th scope="col">Elapsed vs p50</th><th scope="col">CPU</th><th scope="col">Memory (vs cap)</th></tr></thead><tbody>${list.map(j => {
    const u = j.usage, scale = Math.max(num(j.p50) * 1.25, num(j.elapsed) * 1.05, 1);
    const memPct = u && num(u.mem_cap) ? num(u.rss) / num(u.mem_cap) : null;
    const cpuPct = u && num(u.cpu_cap) ? num(u.cpu) / num(u.cpu_cap) : null;
    return `<tr data-key="j-${esc(j.job_id)}"><td><div class="stack-cell">${j.task_id ? `<a href="#job/${esc(j.task_id)}"><strong>${esc(j.name)}</strong></a>` : `<strong>${esc(j.name)}</strong>`}<span class="mono dim small">run #${esc(j.run_index)}${j.task_id ? ` · task ${esc(j.task_id)}` : ''}</span></div></td>` +
      `<td><div class="stack-cell"><span class="mono">${prLink(j.pr) || '—'}</span><span class="dim small">${esc(j.title || '')}</span></div></td>` +
      `<td><span class="mono">${dot('busy')} ${esc(j.runner || 'unknown')}</span></td><td class="mono">${esc(j.host || '—')}</td>` +
      `<td class="mono">${esc(fmtClock(j.started))}</td>` +
      `<td><div class="stack-cell"><span class="mono small">${esc(dur(j.elapsed))}${j.p50 ? ` <span class="dim">of p50 ${esc(dur(j.p50))}</span>` : ''}</span>${mini(num(j.elapsed) / scale, '', `Elapsed ${dur(j.elapsed)}${j.p50 ? ` against a p50 of ${dur(j.p50)}` : ''}`, j.p50 ? num(j.p50) / scale : null)}<span class="dim small">${j.p50 ? (num(j.elapsed) > num(j.p50) ? `over p50 by ${esc(dur(num(j.elapsed) - num(j.p50)))}` : `about ${esc(dur(num(j.p50) - num(j.elapsed)))} left`) : 'no p50 yet'}</span></div></td>` +
      `<td>${u ? `<div class="stack-cell"><span class="mono small">${esc(f1(u.cpu))}${has(u.cpu_cap) ? ` / ${esc(f1(u.cpu_cap))} cores` : ' cores'}</span>${cpuPct == null ? '' : mini(cpuPct, '', `CPU at ${Math.round(cpuPct * 100)}% of its cap`)}<span class="dim small">${cpuPct != null && cpuPct >= 0.98 ? 'at cap' : has(u.throttled_pct) ? `throttled ${esc(Math.round(u.throttled_pct))}%` : ''}</span></div>` : '<span class="dim small">no usage reported</span>'}</td>` +
      `<td>${u && has(u.rss) ? `<div class="stack-cell"><span class="mono small">${esc(gib(u.rss))}${has(u.mem_cap) ? ` / ${esc(gib(u.mem_cap))} GiB` : ' GiB'}</span>${memPct == null ? '' : mini(memPct, memPct >= .95 ? 'coral' : memPct >= .85 ? 'amber' : '', `Memory at ${Math.round(memPct * 100)}% of its cap`)}<span class="dim small">${memPct != null ? Math.round(memPct * 100) + '% of cap' : ''}${num(u.swap) ? ` · swap ${esc(gib(u.swap))} GiB` : ''}${has(u.oom_kills) ? ` · ${esc(u.oom_kills)} OOM` : ''}</span></div>` : '<span class="dim small">no usage reported</span>'}</td></tr>`;
  }).join('')}</tbody></table></div>`;
}

function resultDot(status) {
  if (status === 'success') return 'ok';
  if (status === 'failure' || status === 'cancelled') return 'fail';
  return 'dimdot';
}
function finishedTable(list, s) {
  if (!list.length) return emptyBox('Nothing finished in the last 30 minutes.');
  return `<div class="tablewrap"><table class="tbl min1100"><thead><tr><th scope="col">Result</th><th scope="col">Job</th><th scope="col">PR</th><th scope="col">Runner</th><th scope="col">Waited</th><th scope="col">Ran</th><th scope="col">Ended</th></tr></thead><tbody>${list.map(f => {
    const waited = has(f.started) && has(f.ready) ? f.started - f.ready : null;
    return `<tr data-key="f-${esc(f.task_id)}"><td>${dot(resultDot(f.status))} ${esc(f.status)}</td><td><a href="#job/${esc(f.task_id)}">${esc(f.name)}</a></td><td class="mono">${esc(f.pr || '—')}</td><td class="mono">${esc(f.runner || '—')}</td>` +
      `<td class="mono ${num(waited) > 120 ? 'amber' : ''}">${esc(shortDur(waited))}</td><td class="mono">${esc(has(f.started) && has(f.stopped) ? dur(f.stopped - f.started) : '—')}</td>` +
      `<td class="mono">${esc(fmtClock(f.stopped))} <span class="dim">· ${esc(dur(s.generated - num(f.stopped)))} ago</span></td></tr>`;
  }).join('')}</tbody></table></div>`;
}

function renderQueue(s, d) {
  const q = state.q;
  const nRun = d.running.length, nWait = d.waiting.length, nBlk = d.blocked.length, nFin = (s.finished || []).length;
  const statusChips = [['all', 'All', nRun + nWait + nBlk], ['running', 'Running', nRun], ['waiting', 'Waiting', nWait], ['blocked', 'Blocked', nBlk], ['finished', 'Finished', nFin]]
    .map(([v, l, n]) => chipBtn('status', v, l, n, q.status === v)).join('');
  const whyChips = WHY.map(([k, l, tone]) => chipBtn('why', k, l, num((s.classes || {})[k]), q.why === k, tone)).join('');
  const show = k => q.status === 'all' || q.status === k;
  const wait = d.waiting.filter(j => (!q.why || j.class === q.why) && matches(j, q.text));
  const blk = d.blocked.filter(j => (!q.why || j.class === q.why) && matches(j, q.text));
  const run = d.running.filter(j => matches(j, q.text));
  const fin = (s.finished || []).filter(j => matches(j, q.text));
  const filtered = q.why || q.text;
  const sec = (id, title, sub, inner) => `<section aria-labelledby="${id}"><h2 id="${id}">${esc(title)} <span class="sub">· ${esc(sub)}</span></h2>${inner}</section>`;
  const oldest = Math.max(0, ...wait.map(j => num(j.waited)));
  return `<div class="stack"><section aria-label="Filters" class="filters">` +
    `<div class="fgroup"><span class="eyebrow" id="f-status">Status</span><div class="fchips" role="group" aria-labelledby="f-status">${statusChips}</div></div>` +
    `<div class="fgroup"><span class="eyebrow" id="f-why">Why waiting</span><div class="fchips" role="group" aria-labelledby="f-why">${whyChips}</div></div>` +
    `<div class="fgroup grow" style="min-width:240px"><label for="queue-filter">Filter by PR, job, runner or label</label><input id="queue-filter" type="text" value="${esc(q.text)}" placeholder="#1340 · xcode-27.1 · fast Rust gate" autocomplete="off" spellcheck="false"></div></section>` +
    (show('waiting') ? sec('waiting-h', 'Waiting', `${cnt(wait.length, 'job')}${filtered ? ` of ${nWait}` : ''}${oldest ? ` · oldest ${dur(oldest)}` : ''}`, wait.length ? `<div class="list">${wait.map(waitingCard).join('')}</div>` : emptyBox(nWait ? 'No waiting jobs match the filters.' : 'Nothing is waiting for a runner.')) : '') +
    (show('running') ? sec('running-h', 'Running', `${cnt(run.length, 'job')} · ${d.busySlots} of ${d.slots} online slots busy`, runningTable(run, s)) : '') +
    (show('blocked') ? sec('blocked-h', 'Blocked', `${cnt(blk.length, 'job')} · waiting on another job in its run`, blk.length ? `<div class="list">${blk.map(blockedRow).join('')}</div>` : emptyBox(nBlk ? 'No blocked jobs match the filters.' : 'Nothing is blocked.')) : '') +
    (show('finished') ? sec('finished-h', 'Finished', `last 30 min · ${cnt(fin.length, 'job')}`, finishedTable(fin, s) + '<span class="note">Waited is started minus ready; under 5 s is shown as ≤ 5 s because the collector polls every 5 s.</span>') : '') +
    `</div>`;
}

/* ============================== POOLS ============================== */
function poolRow(p, d) {
  const rs = (p.runners || []).map(n => d.runnerBy.get(n)).filter(Boolean);
  const kinds = rs.map(r => ({ r, k: !r.online ? 'off' : r.paused ? 'paused' : num(r.busy) > 0 ? 'busy' : 'idle' }));
  const nOff = kinds.filter(x => x.k === 'off').length, nPaused = kinds.filter(x => x.k === 'paused').length;
  const bits = [`${num(p.busy)} busy`, `${Math.max(0, num(p.online) - num(p.busy))} idle`, `${nOff} offline`].concat(nPaused ? [`${nPaused} paused`] : []);
  const w = (p.stats && p.stats.wait) || {};
  const demand = num(p.waiting) > 0 || num(w.n) > 0;
  const tone = num(p.free) === 0 && demand ? 'coral' : num(p.free) <= 1 ? 'amber' : '';
  const freeWord = tone === 'coral' ? 'none free, jobs wait' : tone === 'amber' ? (num(p.free) === 0 ? 'none free' : 'last free slot') : '';
  const edge = num(p.registered) && !num(p.online) ? 'amber' : (num(p.waiting) && !num(p.registered) ? 'coral' : '');
  const names = (p.name || '').split(' · ');
  const used = [...new Set((p.used_by || []).map(u => u.replace(/\s\([^()]*\.ya?ml\)$/, '')))];
  let note = '';
  if (num(p.waiting) && !num(p.registered)) note = `<span class="coral-t small">${cnt(num(p.waiting), 'job')} waiting · no registered runner carries this set</span>`;
  else if (num(p.registered) && !num(p.online)) note = '<span class="amber small">0 online</span>';
  return `<tr class="${edge ? 'edge-' + (edge === 'amber' ? 'amber' : 'coral') : ''}" data-key="p-${esc(p.name)}"><td><div class="stack-cell"><span class="chips" title="${esc((p.labels || []).join(', '))}">${names.map((n, i) => `<span class="chip ${num(p.waiting) && !num(p.registered) && i === names.length - 1 ? 'miss' : ''}">${esc(n)}</span>`).join('')}</span>${note}</div></td>` +
    `<td style="max-width:300px" title="${esc(used.join(', '))}"><span class="small">${esc(used.slice(0, 3).join(', ') || '—')}${used.length > 3 ? ` <span class="dim">+${used.length - 3} more</span>` : ''}</span></td>` +
    `<td class="r mono">${esc(p.registered)}</td><td class="r mono">${esc(p.online)}</td><td class="r mono">${esc(p.busy)}</td>` +
    `<td class="r"><div class="stack-cell"><span class="free-n ${tone}">${esc(p.free)}</span><span class="small ${tone}">${esc(freeWord)}</span></div></td>` +
    `<td>${rs.length ? `<div class="segs" role="img" aria-label="${esc(bits.join(', ') + ' of ' + p.registered + ' registered')}">${kinds.map(x => `<span class="${x.k}" title="${esc(`${x.r.name}: ${x.k === 'off' ? 'offline' : x.k}`)}"></span>`).join('')}</div><div class="dim small">${esc(bits.join(' · '))}</div>` : '<span class="dim small">no runners</span>'}</td>` +
    `<td class="r mono ${num(p.waiting) ? 'coral-t' : ''}">${esc(p.waiting)}${num(p.waiting) && num(p.oldest_wait) ? `<div class="dim small">oldest ${esc(dur(p.oldest_wait))}</div>` : ''}</td>` +
    `<td class="mono small">${esc(p.next_schedule ? fmtStamp(p.next_schedule) : '—')}</td>` +
    `<td class="r mono">${num(w.n) ? `${esc(dur(w.p50))} <span class="dim">/</span> <span class="${num(w.p95) >= 300 ? 'amber' : ''}">${esc(dur(w.p95))}</span><div class="dim small">${esc(w.n)} jobs</div>` : '<span class="dim">—</span>'}</td>` +
    `<td class="r mono">${has(p.stats && p.stats.busy_pct) ? esc(p.stats.busy_pct) + '%' : '<span class="dim">—</span>'}</td></tr>`;
}
const POOL_HEAD = '<thead><tr><th scope="col">Pool</th><th scope="col">Used by</th><th scope="col" class="r">Registered</th><th scope="col" class="r">Online</th><th scope="col" class="r">Busy</th><th scope="col" class="r">Free</th><th scope="col">Capacity · one segment per runner</th><th scope="col" class="r">Waiting</th><th scope="col">Next schedule</th><th scope="col" class="r">Wait p50 / p95 · 24 h</th><th scope="col" class="r">Busy % · 24 h</th></tr></thead>';

function matrixHtml(pools, s, d) {
  const hosts = s.hosts || [];
  if (!hosts.length || !pools.length) return emptyBox('No hosts or pools to show.');
  const cols = `minmax(150px,1.4fr) repeat(${hosts.length}, minmax(70px,1fr))`;
  const head = `<div class="matrix" style="grid-template-columns:${cols}"><span class="eyebrow">Pool</span>${hosts.map(h => `<div class="mhead"><span>${esc(h.name)}</span>${h.alias && h.alias !== h.name ? `<small>${esc(h.alias)}</small>` : ''}${h.voter ? '<span class="pill voter">voter</span>' : ''}</div>`).join('')}`;
  const rows = pools.map(p => {
    const rs = (p.runners || []).map(n => d.runnerBy.get(n)).filter(Boolean);
    return `<span class="mono small">${esc(p.name)}${!rs.length ? '<div class="coral-t">no host carries it</div>' : ''}</span>` + hosts.map(h => {
      const here = rs.filter(r => r.host === h.name), on = here.filter(r => r.online).length;
      if (!here.length) return '<span class="mcell" title="no runners"></span>';
      const cls = on === here.length ? 'all' : on === 0 ? 'none' : 'some';
      const word = cls === 'all' ? 'all online' : cls === 'none' ? 'offline only' : 'some offline';
      return `<span class="mcell ${cls}" title="${esc(`${h.name}: ${on} of ${here.length} online (${word})`)}">${on}/${here.length}<span class="sr"> ${word}</span></span>`;
    }).join('');
  }).join('');
  return `<div class="tablewrap">${head}${rows}</div></div>`;
}

function heatHtml(pools, s) {
  const withHours = pools.filter(p => p.stats && (p.stats.hours || []).length);
  if (!withHours.length) return emptyBox('No 24 h history recorded yet.');
  const nowHour = Math.floor(num(s.generated) / 3600) * 3600;
  const slots = Array.from({ length: 24 }, (_, i) => nowHour - (23 - i) * 3600);
  const hourLabel = t => pad2(new Date(t * 1000).getHours());
  const rows = withHours.map(p => {
    const by = new Map(p.stats.hours.map(h => [num(h.hour), h]));
    let peak = null, waits = 0;
    const cells = slots.map((t, i) => {
      const h = by.get(t), when = `${fmtDay(t)} ${hourLabel(t)}:00${i === 23 ? ' (now)' : ''}`;
      if (!h) return `<span class="hcell none ${i === 23 ? 'now' : ''}" title="${esc(`${p.name} · ${when} · no data`)}"></span>`;
      const f = num(h.busy);
      if (!peak || f > peak.f) peak = { f, t };
      const waited = num(h.max_wait) > 300; if (waited) waits++;
      return `<span class="hcell h${Math.min(4, Math.floor(f * 5 + 1e-9))} ${i === 23 ? 'now' : ''}" title="${esc(`${p.name} · ${when} · ${Math.round(f * 100)}% busy (${f.toFixed(3)}) · max wait ${dur(h.max_wait)}${num(h.waiting) ? ` · up to ${h.waiting} waiting` : ''}`)}"></span>`;
    }).join('');
    const ticks = slots.map(t => { const h = by.get(t); const w = h && num(h.max_wait) > 300; return `<span class="${w ? 'w' : ''}" ${w ? `title="${esc(`${p.name} · ${fmtDay(t)} ${hourLabel(t)}:00 · a job waited ${dur(h.max_wait)}`)}"` : ''}></span>`; }).join('');
    const peakText = peak ? `peak ${Math.round(peak.f * 100)}% · ${fmtDay(peak.t)} ${hourLabel(peak.t)}:00` : 'no data';
    return `<div data-key="hm-${esc(p.name)}"><div class="hrow"><div class="stack-cell"><span class="mono small">${esc(p.name)}</span><span class="dim small">${esc(p.online)} online slots now</span></div>` +
      `<div class="hcells" role="img" aria-label="${esc(`${p.name}: busy fraction per hour over the last 24 hours, ${peakText}${waits ? `, ${waits} hours with a job waiting over 5 minutes` : ''}`)}">${cells}</div><span class="mono small">${esc(peakText)}</span></div>` +
      `<div class="hrow"><span class="dim small" style="text-align:right">waited &gt; 5 min</span><div class="hticks">${ticks}</div><span class="mono small ${waits ? 'coral-t' : 'dim'}">${waits ? cnt(waits, 'hour') + ' with waits' : 'no waits'}</span></div></div>`;
  }).join('');
  return `<div class="tablewrap"><div class="heat"><div class="hrow"><span></span><div class="hlabels" aria-hidden="true">${slots.map(t => `<span>${hourLabel(t)}</span>`).join('')}</div><span></span></div>${rows}</div></div>`;
}

function renderPools(s, d) {
  const pools = s.pools || [];
  const main = pools.filter(p => p.requested || num(p.waiting) > 0);
  const unused = pools.filter(p => !p.requested && !num(p.waiting));
  const ramp = ['0–20%', '20–40%', '40–60%', '60–80%', '80–100%'];
  return `<div class="stack"><section aria-labelledby="pools-h"><div class="sechead"><h2 id="pools-h">Pools <span class="sub">· capacity by label set · ${main.length} requested by workflows · ${unused.length} unused</span></h2><div class="grow"></div>` +
    `<div class="legend"><span><i class="sw ci"></i>busy runner</span><span><i class="sw idle"></i>idle runner</span><span><i class="sw off"></i>offline runner</span></div></div>` +
    `<div class="note">A pool is a distinct <code>runs-on</code> label set that workflows request. Its capacity is every runner whose labels contain the whole set, so a runner can count toward several pools. Free is coral at 0 when jobs are waiting, amber at 1 or fewer.</div>` +
    `<div class="tablewrap"><table class="tbl min1250">${POOL_HEAD}<tbody>${main.map(p => poolRow(p, d)).join('')}</tbody></table></div></section>` +
    (unused.length ? `<section aria-labelledby="unused-h" class="dimgroup"><h2 id="unused-h" class="dim">Unused labels <span class="sub">· no workflow requests them and nothing is waiting</span></h2><div class="tablewrap"><table class="tbl min1250">${POOL_HEAD}<tbody>${unused.map(p => poolRow(p, d)).join('')}</tbody></table></div></section>` : '') +
    `<section aria-labelledby="matrix-h"><div class="sechead"><h2 id="matrix-h">Where capacity lives <span class="sub">· online / registered runners per pool and host</span></h2><div class="grow"></div>` +
    `<div class="legend"><span><i class="sw" style="background:rgba(95,181,130,.25);border:1px solid rgba(95,181,130,.5)"></i>all online</span><span><i class="sw" style="background:rgba(217,160,91,.25);border:1px solid rgba(217,160,91,.5)"></i>some offline</span><span><i class="sw off"></i>offline only</span></div></div>${matrixHtml(main, s, d)}</section>` +
    `<section aria-labelledby="heat-h"><div class="sechead"><h2 id="heat-h">Utilisation <span class="sub">· last 24 h · hourly, local time</span></h2><div class="grow"></div>` +
    `<div class="legend"><span class="ramp"><span class="mono">0%</span>${ramp.map((l, i) => `<i class="hcell h${i}" title="${esc(l + ' of online slots busy')}"></i>`).join('')}<span class="mono">100%</span></span>` +
    `<span><i class="sw" style="background:var(--coral);width:3px"></i>hour with a job waiting &gt; 5 min</span><span><i class="sw off"></i>no data</span></div></div>` +
    `<div class="note">Colour scale: average share of the pool's online runners that were busy during the hour, in five 20% steps. Hover a cell for the exact value.</div>${heatHtml(main, s)}</section></div>`;
}

/* ============================== JOB DETAIL ============================== */
const CH = { x0: 48, x1: 544, y0: 190, hh: 176 };
function niceMax(v) {
  if (!(v > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  for (const m of [1, 2, 3, 4, 6, 8, 10]) if (v <= m * p) return m * p;
  return 10 * p;
}
const tickNum = v => String(+v.toFixed(2));
function mmss(sec) { sec = Math.round(sec); return sec >= 3600 ? `${Math.floor(sec / 3600)}:${pad2(Math.floor(sec % 3600 / 60))}h` : `${Math.floor(sec / 60)}:${pad2(sec % 60)}`; }
function chart(o, body) {
  const T = Math.max(1, o.T);
  const X = t => CH.x0 + (t / T) * (CH.x1 - CH.x0);
  const Y = v => CH.y0 - clamp(v / o.yMax, 0, 1.05) * CH.hh;
  let g = '', yl = '', xl = '';
  for (let i = 1; i <= 4; i++) g += `<line x1="${CH.x0}" y1="${CH.y0 - CH.hh * i / 4}" x2="${CH.x1}" y2="${CH.y0 - CH.hh * i / 4}"/>`;
  for (let i = 0; i <= 4; i++) yl += `<text x="40" y="${CH.y0 - CH.hh * i / 4 + 4}" text-anchor="end">${tickNum(o.yMax * i / 4)}</text>`;
  const step = [30, 60, 120, 300, 600, 1200, 1800, 3600, 7200, 14400, 43200].find(st => T / st <= 6) || 86400;
  for (let t = 0; t <= T + 0.1; t += step) xl += `<text x="${X(t).toFixed(1)}" y="206" text-anchor="middle">${mmss(t)}</text>`;
  const caps = (o.caps || []).map(c => { const y = Y(c.v); return `<line x1="${CH.x0}" y1="${y.toFixed(1)}" x2="${CH.x1}" y2="${y.toFixed(1)}" stroke="#d9a05b" stroke-width="1.5" stroke-dasharray="5 4"/><text class="cap" x="${CH.x1}" y="${(y < 14 ? y + 13 : y - 6).toFixed(1)}" text-anchor="end">${esc(c.label)}</text>`; }).join('');
  return `<svg class="chart" role="img" aria-label="${esc(o.aria)}" viewBox="0 0 560 230" preserveAspectRatio="xMidYMid meet"><g class="grid">${g}</g><line class="axis" x1="${CH.x0}" y1="${CH.y0}" x2="${CH.x1}" y2="${CH.y0}"/>${yl}${xl}` +
    `<text x="296" y="224" text-anchor="middle">elapsed (min:s)</text><text x="12" y="102" text-anchor="middle" transform="rotate(-90 12 102)">${esc(o.yLabel)}</text>${body(X, Y)}${caps}</svg>`;
}
const pts = (xs, vs, X, Y) => xs.map((t, i) => `${X(t).toFixed(1)},${Y(vs[i]).toFixed(1)}`).join(' ');
const areaPts = (xs, vs, X, Y) => `${X(xs[0]).toFixed(1)},${CH.y0} ${pts(xs, vs, X, Y)} ${X(xs[xs.length - 1]).toFixed(1)},${CH.y0}`;

function usageCharts(usage) {
  const u = usage.filter(x => has(x.ts) && has(x.cpu) && has(x.mem));
  if (u.length < 2) return emptyBox(u.length ? 'Only one usage sample so far; charts need at least two.' : 'No usage samples were recorded for this job (no agent on its host).');
  const t0 = u[0].ts, xs = u.map(x => x.ts - t0), T = xs[xs.length - 1];
  const cpu = u.map(x => num(x.cpu)), capC = num(u[u.length - 1].cpu_cap), mem = u.map(x => num(x.mem) / GIB), swap = u.map(x => num(x.swap) / GIB), capM = num(u[u.length - 1].mem_cap) / GIB;
  const cpuMax = niceMax(Math.max(...cpu, capC) * 1.05), memMax = niceMax(Math.max(...mem, capM) * 1.05);
  const dots = (X, Y, vs) => `<circle cx="${X(xs[xs.length - 1]).toFixed(1)}" cy="${Y(vs[vs.length - 1]).toFixed(1)}" r="3.5" fill="#6ea8fe"/>`;
  const c1 = chart({ T, yMax: cpuMax, yLabel: 'cores', caps: capC ? [{ v: capC, label: `cap ${f1(capC)}` }] : [],
    aria: `Line chart of CPU cores used over ${dur(T)}. Peak ${f1(Math.max(...cpu))} cores${capC ? ` against a ${f1(capC)} core cap` : ''}.` },
  (X, Y) => `<polygon points="${areaPts(xs, cpu, X, Y)}" fill="rgba(110,168,254,.14)"/><polyline points="${pts(xs, cpu, X, Y)}" fill="none" stroke="#6ea8fe" stroke-width="2" stroke-linejoin="round"/>${dots(X, Y, cpu)}`);
  const c2 = chart({ T, yMax: memMax, yLabel: 'GiB', caps: capM ? [{ v: capM, label: `cap ${tickNum(capM)} GiB` }] : [],
    aria: `Line chart of memory in GiB over ${dur(T)}. Peak ${f1(Math.max(...mem))} GiB${capM ? ` against a ${tickNum(capM)} GiB cap` : ''}${Math.max(...swap) > 0.005 ? `; swap reached ${f1(Math.max(...swap))} GiB` : ''}.` },
  (X, Y) => `<polygon points="${areaPts(xs, mem, X, Y)}" fill="rgba(110,168,254,.14)"/>${Math.max(...swap) > 0 ? `<polyline points="${pts(xs, swap, X, Y)}" fill="none" stroke="#9a9aa3" stroke-opacity=".7" stroke-width="1.5"/>` : ''}<polyline points="${pts(xs, mem, X, Y)}" fill="none" stroke="#6ea8fe" stroke-width="2" stroke-linejoin="round"/>${dots(X, Y, mem)}`);
  const leg = (swatch, t) => `<span><i class="sw" style="${swatch}"></i>${esc(t)}</span>`;
  return `<div class="charts"><div class="card"><div class="sechead"><span class="eyebrow">CPU cores used</span><div class="grow"></div><div class="legend">${leg('background:#6ea8fe;height:3px', 'cores in use')}${capC ? leg('border-top:2px dashed #d9a05b;height:0', `cap ${f1(capC)}`) : ''}</div></div><div class="chartbox">${c1}</div></div>` +
    `<div class="card"><div class="sechead"><span class="eyebrow">Memory</span><div class="grow"></div><div class="legend">${leg('background:#6ea8fe;height:3px', 'RSS')}${leg('background:#9a9aa3;height:3px', 'swap')}${capM ? leg('border-top:2px dashed #d9a05b;height:0', `cap ${tickNum(capM)} GiB`) : ''}</div></div><div class="chartbox">${c2}</div></div></div>`;
}

function hostChart(hu, host) {
  const u = hu.filter(x => has(x.ts) && x.cpu_by);
  if (u.length < 2) return emptyBox('No host load samples were recorded around this job.');
  const t0 = u[0].ts, xs = u.map(x => x.ts - t0), T = xs[xs.length - 1];
  const ci = u.map(x => num(x.cpu_by.ci)), px = u.map(x => num(x.cpu_by.plurx)), ot = u.map(x => num(x.cpu_by.other));
  const l1 = ci, l2 = ci.map((v, i) => v + px[i]), l3 = l2.map((v, i) => v + ot[i]);
  const band = (X, Y, lo, hi, fill) => `<polygon points="${pts(xs, hi, X, Y)} ${xs.map((t, i) => `${X(xs[xs.length - 1 - i]).toFixed(1)},${Y(lo[lo.length - 1 - i]).toFixed(1)}`).join(' ')}" fill="${fill}"/>`;
  const zero = ci.map(() => 0);
  const svg = chart({ T, yMax: niceMax(Math.max(...l3, 1) * 1.05), yLabel: 'cores',
    aria: `Stacked area chart of ${host || 'host'} CPU in cores: CI ${f1(ci[ci.length - 1])}, plurx ${f1(px[px.length - 1])}, other ${f1(ot[ot.length - 1])} at the last sample.` },
  (X, Y) => band(X, Y, zero, l1, 'rgba(110,168,254,.85)') + band(X, Y, l1, l2, 'rgba(201,162,255,.85)') + band(X, Y, l2, l3, 'rgba(107,107,118,.9)'));
  const last = u.length - 1;
  const row = (cls, name, v) => `<span style="display:flex;justify-content:space-between;gap:8px"><span><i class="sw ${cls}"></i> ${esc(name)}</span><span class="mono">${f1(v)} cores</span></span>`;
  return `<div class="card" style="display:flex;flex-wrap:wrap;gap:16px 28px"><div style="flex:1 1 460px;min-width:0"><div class="eyebrow">Host CPU by source · stacked</div><div class="chartbox">${svg}</div></div>` +
    `<div style="flex:1 1 240px;display:flex;flex-direction:column;gap:8px">${row('ci', 'CI jobs', ci[last])}${row('plurx', 'plurx', px[last])}${row('other', 'other', ot[last])}` +
    `<span style="display:flex;justify-content:space-between;border-top:1px solid var(--line);padding-top:8px"><span class="dim">total (last sample)</span><span class="mono">${f1(l3[last])} cores</span></span></div></div>`;
}

function renderJob(s, d) {
  const J = state.job;
  const back = '<a href="#queue" class="dim small">← Queue</a>';
  if (!J || !J.data) return `<div class="stack">${back}${J && J.error ? emptyBox(`Could not load task ${J.id}: ${J.error}`) : '<p class="dim pad">Loading job…</p>'}</div>`;
  const { task = {}, recent = [], usage = [], host_usage = [] } = J.data;
  if (!task.task_id) return `<div class="stack">${back}${emptyBox(`Task ${J.id} is not known to the collector.`)}</div>`;
  const live = d.runningByTask.get(task.task_id);
  const terminal = ['success', 'failure', 'cancelled', 'skipped'].includes(task.status);
  const runner = d.runnerBy.get(task.runner);
  const waited = has(task.started) && has(task.ready) ? task.started - task.ready : null;
  const elapsed = live ? num(live.elapsed) : has(task.started) ? (has(task.stopped) ? task.stopped : s.generated) - task.started : null;
  const p50 = live && live.p50 ? live.p50 : median(recent.map(r => has(r.stopped) && has(r.started) ? r.stopped - r.started : null));
  const tone = task.status === 'running' ? 'blue' : task.status === 'success' ? 'green' : terminal ? 'coral' : 'amber';
  const scale = Math.max(num(p50) * 1.25, num(elapsed) * 1.05, 1);
  const eligible = d.runners.filter(r => eligibleFor(r, task.runs_on));
  eligible.sort((a, b) => (b.name === task.runner) - (a.name === task.runner) || (b.online - a.online) || a.name.localeCompare(b.name));
  const eligRow = r => { const st = runnerState(r, d); const here = r.name === task.runner; const hv = d.hostBy.get(r.host);
    return `<div class="elig ${here ? 'here' : ''}">${dot(st.k)}<span class="n">${esc(r.name)}</span><span class="h">${esc(r.host)}</span>${hv && hv.voter ? '<span class="pill voter">plurx voter</span>' : '<span class="pill">non-voter</span>'}<span class="s">${here ? 'ran this job · ' : ''}${esc(st.word)} now</span></div>`; };
  const onlineEl = eligible.filter(r => r.online).length;
  const peakCpu = Math.max(0, ...usage.map(u => num(u.cpu))), avgCpu = usage.length ? usage.reduce((n, u) => n + num(u.cpu), 0) / usage.length : 0;
  const peakMem = Math.max(0, ...usage.map(u => num(u.mem))), memCap = num((usage[usage.length - 1] || {}).mem_cap), cpuCap = num((usage[usage.length - 1] || {}).cpu_cap);
  const tiles = usage.length ? [
    kpi('Peak CPU', f1(peakCpu), ' cores', cpuCap ? `${Math.round(peakCpu / cpuCap * 100)}% of ${f1(cpuCap)} core cap` : 'no cap recorded'),
    kpi('Average CPU', f1(avgCpu), ' cores', `${usage.length} samples, 5 s apart`),
    kpi('Peak memory', gib(peakMem), ' GiB', memCap ? `${Math.round(peakMem / memCap * 100)}% of ${gib(memCap)} GiB cap` : 'no cap recorded', memCap && peakMem / memCap >= .9 ? 'amber' : ''),
    kpi('Peak swap', gib(Math.max(0, ...usage.map(u => num(u.swap)))), ' GiB', 'cgroup swap'),
    kpi('Max CPU throttling', Math.round(Math.max(0, ...usage.map(u => num(u.throttled_pct)))), '%', 'share of periods throttled'),
    kpi('OOM kills', Math.max(0, ...usage.map(u => num(u.oom_kills))), '', 'in this job\'s cgroup', Math.max(0, ...usage.map(u => num(u.oom_kills))) ? 'coral' : ''),
  ].join('') : '';
  const cancelBtn = !terminal && has(task.run_id) ? controlBtn({ action: 'cancel_run', target: task.run_id, label: `Cancel run #${task.run_index} (all jobs)`, cls: 'danger', title: `Cancel run #${task.run_index}`, desc: `Cancels the whole Forgejo run #${task.run_index}, including every other job in it. Forgejo cannot cancel a single job.`, confirm: 'Cancel run' }) : '';
  const drainBtn = runner && (runner.controls || []).includes('drain') ? controlBtn({ action: 'drain', target: runner.name, label: `Drain ${runner.name} after this job`, title: `Drain ${runner.name}`, desc: 'Stops this runner taking new jobs once the current job finishes.', confirm: 'Drain' }) : '';
  return `<div class="stack">` +
    `<section aria-labelledby="job-h">${back}<div class="titleblock"><div style="flex:1 1 520px;min-width:0;display:flex;flex-direction:column;gap:8px">` +
    `<div class="chips" style="align-items:center"><h1 id="job-h">${esc(task.name)}</h1>${badge(String(task.status || 'unknown').toUpperCase(), tone)}</div>` +
    `<div class="kv">${task.pr ? `<span>${prLink(task.pr)}</span>` : ''}<b>${esc(task.workflow || '')}</b><span>run <b>#${esc(task.run_index)}</b></span><span>task <b>${esc(task.task_id)}</b></span><span>job <b>${esc(task.job_id)}</b></span></div></div>` +
    `<div class="btnrow">${cancelBtn}${drainBtn}${forgeLink(`/actions/runs/${encodeURIComponent(task.run_index)}`, 'Open logs in Forgejo')}</div></div></section>` +
    `<section aria-label="Progress" class="card"><div class="bignums"><div><div class="eyebrow">${terminal ? 'Ran' : 'Elapsed'}</div><div class="n ${terminal ? '' : 'blue'}">${esc(dur(elapsed))}</div></div>` +
    `<div><div class="eyebrow">${live && live.p50 ? 'p50' : 'Typical (median of recent)'}</div><div class="n">${esc(p50 ? dur(p50) : '—')}</div></div>` +
    (live && live.p50 ? `<div><div class="eyebrow">ETA</div><div class="n">≈ ${esc(fmtClock(task.started + live.p50))}</div></div>` : '') +
    `<div class="grow"></div><div class="small dim">started <b class="mono" style="color:var(--ink)">${esc(fmtClock(task.started))}</b> · waited <b class="mono" style="color:var(--ink)">${esc(shortDur(waited))}</b></div></div>` +
    `${p50 && !terminal ? `<div class="progress" role="img" aria-label="${esc(`Progress: ${dur(elapsed)} elapsed, p50 duration ${dur(p50)}`)}"><span class="fill" style="width:${pctOf(elapsed, scale)}%"></span><span class="p50" style="left:${pctOf(p50, scale)}%"></span><span class="lbl" style="left:${pctOf(elapsed, scale)}%;color:var(--blue-t)">now ${esc(dur(elapsed))}</span><span class="lbl" style="left:${pctOf(p50, scale)}%;margin-top:${Math.abs(num(p50) - num(elapsed)) / scale < .12 ? 14 : 0}px">p50 ${esc(dur(p50))}</span></div>` : ''}</section>` +
    `<section aria-labelledby="place-h"><h2 id="place-h">Placement <span class="sub">· why it ran here</span></h2><div class="card" style="display:flex;flex-direction:column;gap:14px">` +
    `<div class="kv"><span>runner <b>${esc(task.runner || 'unknown')}</b></span>${runner ? `<span>host <b>${esc(runner.host)}</b></span>` : ''}<span>${waited !== null && waited < 5 ? 'picked up <b>≤ 5 s</b> after it was ready' : waited !== null ? `waited <b>${esc(dur(waited))}</b> for a runner` : 'wait time unknown'}</span></div>` +
    `<div><div class="eyebrow">Requested labels</div><div class="chips" style="margin-top:6px">${(task.runs_on || []).map(l => `<span class="chip">${esc(l)}</span>`).join('') || '<span class="dim">none recorded</span>'}</div></div>` +
    `<div><div class="slots-head eyebrow"><span>Eligible runners (labels ⊇ requested)</span><span class="small" style="text-transform:none;letter-spacing:0">${onlineEl} online of ${eligible.length} registered · current state</span></div>${eligible.length ? eligible.map(eligRow).join('') : emptyBox('No registered runner has all of these labels.')}</div>` +
    `<div class="infobox amber">Forgejo gives the job to whichever eligible runner polls first; it has no notion of host load or voter status.</div></div></section>` +
    `<section aria-labelledby="res-h"><h2 id="res-h">Resources <span class="sub">· this job's cgroup · sampled every 5 s</span></h2>${usageCharts(usage)}${tiles ? `<div class="kpis" style="margin-top:2px">${tiles}</div>` : ''}</section>` +
    `<section aria-labelledby="host-h"><h2 id="host-h">Host while this job ran <span class="sub">· ${esc(runner ? runner.host : task.runner || 'unknown')}</span></h2>${hostChart(host_usage, runner ? runner.host : '')}</section>` +
    `<section aria-labelledby="runs-h"><h2 id="runs-h">Recent runs of this job <span class="sub">· last ${recent.length} successful</span></h2>${recent.length ? `<div class="tablewrap"><table class="tbl min620"><thead><tr><th scope="col">Started</th><th scope="col">Runner</th><th scope="col">Run</th><th scope="col">Waited</th><th scope="col">Ran</th><th scope="col">Result</th></tr></thead><tbody>${recent.map(r =>
      `<tr><td class="mono"><a href="#job/${esc(r.task_id)}">${esc(fmtDay(r.started))} ${esc(fmtClock(r.started))}</a></td><td class="mono">${esc(r.runner || '—')}</td><td class="mono">#${esc(r.run_index)}${r.pr ? ' ' + esc(r.pr) : ''}</td><td class="mono">${esc(shortDur(has(r.started) && has(r.ready) ? r.started - r.ready : null))}</td><td class="mono">${esc(has(r.started) && has(r.stopped) ? dur(r.stopped - r.started) : '—')}</td><td>${dot(resultDot(r.status))} ${esc(r.status)}</td></tr>`).join('')}</tbody></table></div>` : emptyBox('No earlier successful runs recorded.')}</section></div>`;
}

/* ============================== DRIFT ============================== */
const SEV = { blocking: ['BLOCKING', 'coral'], critical: ['CRITICAL', 'coral'], warn: ['WARN', 'amber'], warning: ['WARN', 'amber'], info: ['INFO', ''] };
const FIX_LINK = { unsatisfiable: ['#queue', 'Open in Queue'], pool_offline: ['#pools', 'Open in Pools'], expected_online: ['#fleet', 'Open in Fleet (Start VM)'] };
const KNOWN_KEYS = new Set(['severity', 'kind', 'title', 'fix', 'jobs', 'runners', 'hosts', 'versions', 'common', 'labels', 'reasons', 'next_schedule', 'roster']);

function runnerLines(r, reasons) {
  if (typeof r === 'string') return [`<span>${esc(r)}${reasons && reasons[r] ? ` <span class="dim">— ${esc(reasons[r])}</span>` : ''}</span>`];
  const out = [`<span>${esc(r.runner)}</span>`];
  if ((r.roster_only || []).length) out.push(`<span class="sub">in roster only: ${esc(r.roster_only.join(', '))}</span>`);
  if ((r.live_only || []).length) out.push(`<span class="sub">registered only: ${esc(r.live_only.join(', '))}</span>`);
  return out;
}
function fcol(label, lines) { return lines.length ? `<div class="fcol"><span class="eyebrow">${esc(label)}</span><div class="lines">${lines.join('')}</div></div>` : ''; }

function findingCard(f, i) {
  const [sevText, sevTone] = SEV[f.severity] || [String(f.severity || 'info').toUpperCase(), ''];
  const cols = [];
  if ((f.jobs || []).length) cols.push(fcol('Jobs', f.jobs.map(j => `<span>job ${esc(j)}</span>`)));
  if ((f.runners || []).length) cols.push(fcol('Runners', f.runners.flatMap(r => runnerLines(r, f.reasons))));
  else if (f.reasons && typeof f.reasons === 'object') cols.push(fcol('Reasons', Object.entries(f.reasons).map(([k, v]) => `<span>${esc(k)} <span class="dim">— ${esc(v)}</span></span>`)));
  if ((f.roster || []).length) cols.push(fcol('Roster', f.roster.map(r => `<span>${esc(typeof r === 'string' ? r : JSON.stringify(r))}</span>`)));
  if ((f.hosts || []).length) cols.push(fcol('Hosts', f.hosts.map(h => `<span>${esc(typeof h === 'string' ? h : JSON.stringify(h))}</span>`)));
  if (f.versions && typeof f.versions === 'object') cols.push(fcol(`Versions${f.common ? ` (most runners: ${f.common})` : ''}`, Object.entries(f.versions).map(([v, names]) => `<span>${esc(v)}</span><span class="sub">${esc([].concat(names).join(', '))}</span>`)));
  if ((f.labels || []).length) cols.push(fcol('Labels', f.labels.map(l => `<span>${esc(l)}</span>`)));
  if (has(f.next_schedule)) cols.push(fcol('Next scheduled run', [`<span>${esc(fmtStamp(f.next_schedule))}</span>`]));
  for (const [k, v] of Object.entries(f)) if (!KNOWN_KEYS.has(k)) cols.push(fcol(k.replace(/_/g, ' '), [`<span>${esc(typeof v === 'object' ? JSON.stringify(v) : v)}</span>`]));
  const btns = [];
  if (f.kind === 'stale') for (const r of f.runners || []) {
    const n = typeof r === 'string' ? r : r.runner;
    btns.push(controlBtn({ action: 'delete_runner', target: n, label: `Delete ${n}`, cls: 'sm danger', title: `Delete registration ${n}`, desc: `Removes the stale registration ${n} from Forgejo. Only offline runners that are not in the roster and have no agent can be deleted.`, confirm: 'Delete registration' }));
  }
  const link = FIX_LINK[f.kind];
  if (link) btns.push(`<a class="btn sm" href="${esc(link[0])}">${esc(link[1])}</a>`);
  return `<article class="card finding ${f.severity === 'blocking' ? 'blocking' : ''}" data-key="f-${i}-${esc(f.kind)}"><div class="fhead">${badge(sevText, sevTone)}<h3>${esc(f.title)}</h3><span class="mono dim small">${esc(f.kind)}</span></div>` +
    `${cols.length ? `<div class="fcols">${cols.join('')}</div>` : ''}` +
    `${f.fix || btns.length ? `<div class="fix"><span class="eyebrow">Fix</span><span class="txt">${esc(f.fix || '')}</span><div class="btnrow">${btns.join('')}</div></div>` : ''}</article>`;
}

function hygieneTable(s, d) {
  const online = d.runners.filter(r => r.online);
  const counts = {}; online.forEach(r => { counts[r.version] = (counts[r.version] || 0) + 1; });
  const common = Object.keys(counts).sort((a, b) => counts[b] - counts[a])[0];
  let attn = 0;
  const rows = online.map(r => {
    const hy = r.hygiene || {}, host = d.hostBy.get(r.host), disk = host && host.metrics && (host.metrics.disks || []).find(x => x.path === '/') || (host && host.metrics && (host.metrics.disks || [])[0]);
    const odd = r.version && r.version !== common;
    const jan = hy.janitor && has(hy.janitor.finished) ? s.generated - hy.janitor.finished : null;
    const janOld = jan !== null && jan > 86400;
    const below = disk && has(disk.free) && has(disk.reserve) && disk.free < disk.reserve;
    if (odd || janOld || below) attn++;
    const diskCell = disk ? `<div class="stack-cell"><span class="mono ${below ? 'amber' : ''}">${esc(gib(disk.free, 0))} GiB free vs ${esc(gib(disk.reserve, 0))} reserve${below ? ' — below reserve' : ''}</span>` +
      `<span class="mini" role="img" aria-label="${esc(`Free disk ${gib(disk.free, 0)} GiB, reserve ${gib(disk.reserve, 0)} GiB`)}"><span class="${below ? 'amber' : 'green'}" style="width:${pctOf(disk.free, disk.total)}%"></span><i class="ptick" style="left:${pctOf(disk.reserve, disk.total)}%"></i></span></div>` : '<span class="dim">no agent</span>';
    return `<tr class="${odd || janOld || below ? 'attn' : ''}" data-key="hy-${esc(r.name)}"><td class="mono">${esc(r.name)}</td><td class="mono dim">${esc(r.host)}</td>` +
      `<td class="mono ${odd ? 'amber' : ''}">${esc(r.version || '—')}${odd ? ' (differs)' : ''}</td>` +
      `<td class="mono ${janOld ? 'amber' : ''}">${jan === null ? '<span class="dim">no report</span>' : `${esc(dur(jan))} ago${janOld ? ' (stale)' : ''}`}</td>` +
      `<td class="mono">${has(hy.cache_bytes) ? esc(gib(hy.cache_bytes)) + ' GiB' : '<span class="dim">—</span>'}</td><td class="mono">${has(hy.work_bytes) ? esc(gib(hy.work_bytes)) + ' GiB' : '<span class="dim">—</span>'}</td><td>${diskCell}</td></tr>`;
  }).join('');
  return { attn, count: online.length, html: online.length ? `<div class="tablewrap"><table class="tbl min900"><thead><tr><th scope="col">Runner</th><th scope="col">Host</th><th scope="col">Version</th><th scope="col">Janitor last run</th><th scope="col">cache.dir</th><th scope="col">_work</th><th scope="col">Disk free vs reserve</th></tr></thead><tbody>${rows}</tbody></table></div>` : emptyBox('No online runners.') };
}

function renderDrift(s, d) {
  const list = s.drift || [], c = s.counts || {};
  const by = k => list.filter(f => f.severity === k).length;
  const hy = hygieneTable(s, d);
  const audit = s.audit || [];
  return `<div class="stack"><section aria-labelledby="drift-h"><h1 id="drift-h">Drift <span class="sub">· what the roster says vs what is registered vs what is running</span></h1></section>` +
    `<section aria-label="Summary" class="kpis">${kpi('Findings', list.length, '', 'total', list.length ? 'amber' : '')}${kpi('Blocking', by('blocking'), '', 'jobs can never start', by('blocking') ? 'coral' : '')}${kpi('Warnings', by('warn') + by('warning'), '', 'needs attention', by('warn') ? 'amber' : '')}${kpi('Info', by('info'), '', 'housekeeping')}${kpi('Stale registrations', num(c.stale), '', 'offline and not in the roster', num(c.stale) ? 'amber' : '')}</section>` +
    `<section aria-labelledby="findings-h"><h2 id="findings-h">Findings <span class="sub">· ${list.length} · ${by('blocking')} blocking · ${by('warn') + by('warning')} warn · ${by('info')} info</span></h2>${list.length ? `<div class="list">${list.map(findingCard).join('')}</div>` : emptyBox('No drift: the roster, the registrations and the agents agree.')}</section>` +
    `<section aria-labelledby="hygiene-h"><h2 id="hygiene-h">Runner hygiene <span class="sub">· ${cnt(hy.count, 'online runner')} · ${hy.attn} need attention</span></h2>${hy.html}</section>` +
    `<section aria-labelledby="audit-h"><h2 id="audit-h">Recent control actions <span class="sub">· audit log</span></h2>${audit.length ? `<div class="tablewrap"><table class="tbl min760"><thead><tr><th scope="col">When</th><th scope="col">Who</th><th scope="col">Action</th><th scope="col">Target</th><th scope="col">Reason</th><th scope="col">Result</th></tr></thead><tbody>${audit.map((a, i) =>
      `<tr data-key="a-${i}-${esc(a.ts)}"><td class="mono">${esc(fmtDay(a.ts))} ${esc(fmtClock(a.ts))}</td><td class="mono">${esc(a.actor)}</td><td class="mono">${esc(a.action)}</td><td class="mono">${esc(a.target)}</td><td>${esc(a.reason || '—')}</td><td class="${/^refused/.test(a.result || '') ? 'coral-t' : ''}">${esc(a.result)}</td></tr>`).join('')}</tbody></table></div>` : emptyBox('No control actions recorded.')}</section></div>`;
}

/* ============================== controls, sign-in, toasts ============================== */
const els = {};
let pendingControl = null, afterSignin = null;

function toast(text, kind = 'ok') {
  const t = document.createElement('div');
  t.className = `toast ${kind}`; t.textContent = text; // text only
  t.addEventListener('click', () => t.remove());
  els.toasts.append(t);
  setTimeout(() => t.remove(), kind === 'err' ? 12000 : 6000);
}
function openDialog(dlg) { if (typeof dlg.showModal === 'function') dlg.showModal(); else dlg.setAttribute('open', ''); }
function closeDialog(dlg) { if (typeof dlg.close === 'function') dlg.close(); else dlg.removeAttribute('open'); }
async function post(url, body, token) {
  const r = await fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: `token ${token}` }, body: JSON.stringify(body) });
  let data = null; try { data = await r.json(); } catch (e) { /* non-JSON body */ }
  return { ok: r.ok, status: r.status, data };
}
async function whoami(token) {
  const r = await post('/api/whoami', {}, token);
  if (!r.ok) throw new Error((r.data && r.data.error) || `HTTP ${r.status}`);
  return r.data;
}
function setMsg(text, cls = '') { els.signinMsg.textContent = text; els.signinMsg.className = `msg ${cls}`; }
function signOut() { state.token = ''; state.who = null; store.set(TOKEN_KEY, ''); renderChrome(); }

function openSignin(after) {
  afterSignin = after || null;
  els.signinToken.value = '';
  els.signinSignout.hidden = !state.token;
  setMsg(state.who ? `Signed in as ${state.who.login}${state.who.can_write ? '' : ' (read-only: cannot write this repository)'}.` : '');
  openDialog(els.dlgSignin);
  els.signinToken.focus();
}
async function onSignin(e) {
  e.preventDefault();
  const token = els.signinToken.value.trim();
  if (!token) return;
  els.signinSubmit.disabled = true; setMsg('Checking token…');
  try {
    const who = await whoami(token);
    state.token = token; state.who = who; store.set(TOKEN_KEY, token);
    closeDialog(els.dlgSignin); renderChrome();
    toast(`Signed in as ${who.login}${who.can_write ? '' : ' — this account cannot write the repository, controls will be refused'}${who.address_allowed === false ? ' — controls are not accepted from this address' : ''}.`, who.can_write && who.address_allowed !== false ? 'ok' : 'err');
    const next = afterSignin; afterSignin = null; if (next) next();
  } catch (err) { setMsg(`Sign-in failed: ${err.message}`, 'err'); }
  finally { els.signinSubmit.disabled = false; }
}

function startControl(spec) {
  if (!state.token) { openSignin(() => startControl(spec)); return; }
  pendingControl = spec;
  els.cTitle.textContent = spec.title; els.cDesc.textContent = spec.desc;
  const warns = [];
  if (state.who && !state.who.can_write) warns.push(`${state.who.login} cannot write this repository; the collector will refuse.`);
  if (state.who && state.who.address_allowed === false) warns.push('Controls are not accepted from this address.');
  els.cWarn.hidden = !warns.length; els.cWarn.textContent = warns.join(' ');
  els.cReason.value = ''; els.cSubmit.textContent = spec.confirm; els.cSubmit.disabled = false;
  openDialog(els.dlgConfirm);
}
async function onConfirm(e) {
  e.preventDefault();
  const spec = pendingControl; if (!spec) return;
  els.cSubmit.disabled = true;
  try {
    const r = await post('/api/control', { action: spec.action, target: spec.target, reason: els.cReason.value.trim() }, state.token);
    const ok = r.data && r.data.ok === true;
    toast(ok ? `${spec.title}: ${r.data.result}` : `${spec.title} failed: ${(r.data && r.data.error) || 'HTTP ' + r.status}`, ok ? 'ok' : 'err');
    if (r.status === 401) signOut();
  } catch (err) { toast(`${spec.title} failed: ${err.message}`, 'err'); }
  pendingControl = null; closeDialog(els.dlgConfirm); poll();
}
async function copyText(t) {
  try { await navigator.clipboard.writeText(t); return true; } catch (e) { /* fall back */ }
  try {
    const ta = document.createElement('textarea'); ta.value = t; ta.setAttribute('readonly', ''); ta.className = 'offscreen';
    document.body.append(ta); ta.select(); const ok = document.execCommand('copy'); ta.remove(); return ok;
  } catch (e) { return false; }
}

/* ============================== chrome, routing, polling ============================== */
const ROUTES = { fleet: renderFleet, queue: renderQueue, pools: renderPools, drift: renderDrift, job: renderJob };
const TITLES = { fleet: 'Fleet', queue: 'Queue', pools: 'Pools', drift: 'Drift', job: 'Job' };
let lastHtml = null, timer = null, polling = false, again = false;

function setText(el, t) { if (el.textContent !== t) el.textContent = t; }
function setCount(el, n) { el.hidden = !n; setText(el, n ? String(n) : ''); }

function renderChrome() {
  const s = state.snap, c = (s && s.counts) || {};
  setCount($('#nav-queue'), s ? num(c.waiting) + num(c.blocked) : 0);
  setCount($('#nav-drift'), s ? num(c.drift, (s.drift || []).length) : 0);
  document.querySelectorAll('nav a').forEach(a => {
    if (a.dataset.route === state.route.name) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  });
  const stale = !!state.error || (state.fetchedAt && Date.now() - state.fetchedAt > 20000);
  $('#st-live').className = `live ${stale ? 'bad' : ''}`;
  $('#st-dot').className = `dot ${stale ? 'bad' : 'ok'}`;
  setText($('#st-live-word'), !state.fetchedAt && !state.error ? 'connecting' : stale ? 'stale' : 'live');
  setText($('#st-time'), s ? fmtStamp(s.generated) : '—');
  const f = (s && s.forge) || {};
  setText($('#st-forge'), !s ? 'Forgejo —' : f.ok === false ? 'Forgejo unreachable' : f.version ? `Forgejo ${f.version}` : 'Forgejo');
  setText($('#st-agents'), s ? `agents ${Object.keys(s.agents || {}).length} reporting` : 'agents —');
  setText($('#st-poll'), state.fetchedAt ? `polled ${Math.max(0, Math.round((Date.now() - state.fetchedAt) / 1000))} s ago` : 'not polled yet');
  $('#offline-bar').hidden = !state.error;
  const b = $('#signin-btn');
  setText(b, state.who ? `Signed in: ${state.who.login}` : 'Sign in');
  b.title = state.who ? 'Click to sign out or change token' : 'Sign in with a Forgejo token to use controls';
}

function renderRoute() {
  let html;
  try { html = state.snap ? ROUTES[state.route.name](state.snap, state.d) : '<p class="dim pad">Loading…</p>'; }
  catch (err) { console.error(err); html = `<div class="empty">This screen failed to render: ${esc(err && err.message)}</div>`; }
  if (html === lastHtml) return;
  lastHtml = html;
  patchInto($('#view'), html);
}

function parseHash() {
  const [name, arg] = location.hash.replace(/^#/, '').split('/');
  if (name === 'job' && /^\d+$/.test(arg || '')) return { name: 'job', arg };
  return { name: ['fleet', 'queue', 'pools', 'drift'].includes(name) ? name : 'fleet' };
}
function applyRoute(initial) {
  state.route = parseHash();
  state.job = state.route.name === 'job' ? { id: state.route.arg, data: null, error: null } : null;
  document.title = `ci-fleet — ${TITLES[state.route.name]}${state.job ? ' ' + state.job.id : ''}`;
  $('#view').replaceChildren(); lastHtml = null;
  window.scrollTo(0, 0);
  renderChrome(); renderRoute();
  if (!initial) $('#main').focus({ preventScroll: true });
  if (!initial && state.route.name === 'job') poll();
}

async function getJSON(url) {
  const r = await fetch(url, { cache: 'no-store' });
  if (!r.ok) throw new Error(`${url} → HTTP ${r.status}`);
  return r.json();
}
async function loadJob() {
  const J = state.job; if (!J) return;
  if (J.data && J.data.task && ['success', 'failure', 'cancelled', 'skipped'].includes(J.data.task.status)) return;
  try { const data = await getJSON(`/api/job/${encodeURIComponent(J.id)}`); if (state.job === J) { J.data = data; J.error = null; } }
  catch (err) { if (state.job === J) { J.error = err.message; state.error = err; } }
}
async function poll() {
  if (polling) { again = true; return; }
  polling = true; again = false; clearTimeout(timer);
  try {
    if (!document.hidden || !state.snap) {
      state.error = null;
      try { state.snap = await getJSON('/api/snapshot'); state.d = derive(state.snap); state.fetchedAt = Date.now(); }
      catch (err) { state.error = err; } // keep the last data on screen
      await loadJob();
    }
  } finally { polling = false; }
  renderChrome(); renderRoute();
  if (again) { poll(); return; }
  timer = setTimeout(poll, POLL_MS);
}

function onClick(e) {
  const close = e.target.closest('[data-close]');
  if (close) { closeDialog(close.closest('dialog')); return; }
  const el = e.target.closest('[data-act]');
  if (!el) return;
  const a = el.dataset.act;
  if (a === 'status') { state.q.status = el.dataset.v; renderRoute(); }
  else if (a === 'why') { state.q.why = state.q.why === el.dataset.v ? '' : el.dataset.v; renderRoute(); }
  else if (a === 'signin') openSignin();
  else if (a === 'control') startControl({ action: el.dataset.action, target: el.dataset.target, title: el.dataset.title, desc: el.dataset.desc, confirm: el.dataset.confirm });
  else if (a === 'copy-why') {
    const j = state.d && [...state.d.waiting, ...state.d.blocked].find(x => String(x.job_id) === el.dataset.job);
    if (j) copyText(whyText(j)).then(ok => toast(ok ? 'Copied the diagnosis to the clipboard.' : 'Could not copy; select the text manually.', ok ? 'ok' : 'err'));
  }
}

function init() {
  Object.assign(els, {
    toasts: $('#toasts'), dlgSignin: $('#dlg-signin'), signinToken: $('#signin-token'), signinMsg: $('#signin-msg'), signinSubmit: $('#signin-submit'), signinSignout: $('#signin-signout'),
    dlgConfirm: $('#dlg-confirm'), cTitle: $('#confirm-title'), cDesc: $('#confirm-desc'), cWarn: $('#confirm-warn'), cReason: $('#confirm-reason'), cSubmit: $('#confirm-submit'),
  });
  document.addEventListener('click', onClick);
  $('#signin-form').addEventListener('submit', onSignin);
  $('#confirm-form').addEventListener('submit', onConfirm);
  els.signinSignout.addEventListener('click', () => { signOut(); closeDialog(els.dlgSignin); toast('Signed out.'); });
  $('#view').addEventListener('input', e => { if (e.target.id === 'queue-filter') { state.q.text = e.target.value; renderRoute(); } });
  window.addEventListener('hashchange', () => applyRoute(false));
  document.addEventListener('visibilitychange', () => { if (!document.hidden) poll(); });
  setInterval(renderChrome, 1000); // only refreshes the header ("polled N s ago")
  const saved = store.get(TOKEN_KEY);
  if (saved) whoami(saved).then(who => { state.token = saved; state.who = who; renderChrome(); }).catch(() => store.set(TOKEN_KEY, ''));
  applyRoute(true);
  poll();
}
init();

})();
