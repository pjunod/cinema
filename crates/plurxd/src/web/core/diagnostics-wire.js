"use strict";
// Closed v1 data vocabulary. No account, URL, free-form error or credential fields.
const CinemaDiagnosticWire = (() => {
  const version = 'cinema.diagnostics.v1';
  const reasons = ['connection_failed','timeout','unsupported','context_unavailable','telemetry_disabled','storage_denied','storage_full','overflow','peer_unavailable','cancelled','not_requested','interrupted','clock_unknown','revoked','history_disabled','not_delivered','node_limit'];
  const names = ['summary.txt','description.txt','client-events.jsonl','playback.json','connection.json','server-events.jsonl','server-snapshot.json','diagnostics/platform.json'];
  const require = ok => { if (!ok) throw new Error('Invalid diagnostic data'); };
  const keys = (o, required, optional=[]) => { require(o && typeof o === 'object' && !Array.isArray(o)); require(required.every(k => Object.hasOwn(o,k))); require(Object.keys(o).every(k => required.includes(k)||optional.includes(k))); };
  const one = (v, values) => require(typeof v === 'string' && values.includes(v));
  const integer = (n,max) => require(typeof n === 'number' && Number.isSafeInteger(n) && n >= 0 && !Object.is(n,-0) && n <= max);
  const decimal = s => typeof s === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(s) && (s.length < 20 || s <= '18446744073709551615');
  const uuid = s => typeof s === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(s);
  function strict(text,limit=65536) {
    require(typeof text === 'string' && new TextEncoder().encode(text).length <= limit);
    let stack=[], i=0;
    while(i<text.length) {
      const ch=text[i];
      if(ch==='{'||ch==='[') { require(stack.length<17); stack.push(ch==='{'?new Set():null); i++; }
      else if(ch==='}'||ch===']') { require(stack.length>0); stack.pop(); i++; }
      else if(ch==='"') {
        let start=i++; let closed=false;
        while(i<text.length) { if(text[i]==='\\') { i+=2; continue; } if(text[i++]==='"') { closed=true;break; } }
        require(closed); let next=i; while(/[\t\r\n ]/.test(text[next]||'x')) next++;
        if(text[next]===':') { const key=JSON.parse(text.slice(start,i)); const set=stack[stack.length-1]; require(set instanceof Set && !set.has(key) && key.length<=128);set.add(key); }
      } else if(ch==='-'||/[0-9]/.test(ch)) {
        const start=i++;while(i<text.length&&/[0-9.eE+-]/.test(text[i]))i++;const token=text.slice(start,i);
        require(/^(0|[1-9][0-9]*)$/.test(token)); require(Number.isSafeInteger(Number(token)));
      } else i++;
    }
    require(stack.length===0);const value=JSON.parse(text);let count=60000;
    function check(v,depth) {
      require(count-->0 && depth<=16 && v!==null);
      if(typeof v==='string') { require(new TextEncoder().encode(v).length<=16384 && !/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(v)); }
      else if(typeof v==='object') { for(const x of Object.values(v))check(x,depth+1); }
    }
    check(value,0);return value;
  }
  function manifest(text) {
    const m=strict(text);keys(m,['version','report_id','captured_at','category','scope','client','capture','components','privacy','files']);
    require(m.version===version && uuid(m.report_id));
    require(typeof m.captured_at==='string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/.test(m.captured_at) && new Date(m.captured_at).toISOString().replace('.000Z','Z')===m.captured_at);
    one(m.category,['playback','connection','interface','other']);one(m.scope,['anonymous','viewer','administrator']);
    keys(m.client,['platform','app_version','build','os_version','device_model']);one(m.client.platform,['web','browser_tv','ios','ipados','tvos','android','android_tv']);
    for(const k of ['app_version','build','os_version','device_model'])require(typeof m.client[k]==='string'&&/^[A-Za-z0-9._+-]{1,64}$/.test(m.client[k]));
    keys(m.capture,['mode','window_ms','dropped_events','persistence']);one(m.capture.mode,['ordinary','reproduction']);integer(m.capture.window_ms,900000);require(decimal(m.capture.dropped_events));one(m.capture.persistence,['available','memory_only','unavailable']);
    require(Array.isArray(m.components)&&m.components.length>0&&m.components.length<=16);let componentNames=new Set();
    for(const c of m.components) { keys(c,['name','state'],['reason']);one(c.name,['client','server','workers','connection','platform','clock']);require(!componentNames.has(c.name));componentNames.add(c.name);one(c.state,['complete','partial','unavailable']);if(c.state==='complete')require(!Object.hasOwn(c,'reason'));else one(c.reason,reasons); }
    require(componentNames.has('client'));keys(m.privacy,['titles_included','attachments_included']);for(const v of Object.values(m.privacy))require(typeof v==='boolean');
    require(Array.isArray(m.files)&&m.files.length>=3&&m.files.length<32);let entries=new Set(),total=0;
    for(const f of m.files) { keys(f,['name','size_bytes','sha256']);one(f.name,names);require(!entries.has(f.name));entries.add(f.name);integer(f.size_bytes,8388608);total+=f.size_bytes;require(typeof f.sha256==='string'&&/^[0-9a-f]{64}$/.test(f.sha256)); }
    require(total<=67108864&&['summary.txt','description.txt','client-events.jsonl'].every(n=>entries.has(n))&&m.privacy.attachments_included===entries.has('diagnostics/platform.json'));return m;
  }
  function summary(m) {
    return `Cinema diagnostic report ${m.report_id}\nCaptured: ${m.captured_at}\nCategory: ${m.category}\nClient: ${m.client.platform} ${m.client.app_version} build ${m.client.build}\nCapture: ${m.capture.mode} (${m.capture.window_ms} ms); dropped ${m.capture.dropped_events}; persistence ${m.capture.persistence}\n${m.components.map(c=>`${c.name}: ${c.state}${c.reason?` (${c.reason})`:''}`).join('\n')}\nComplete describes collection scope, not retained history.\n`;
  }
  return Object.freeze({version,reasons,names,strict,manifest,summary,decimal,uuid});
})();
