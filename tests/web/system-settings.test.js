"use strict";
const assert=require('node:assert/strict');
const test=require('node:test');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
const ROOT=path.resolve(__dirname,'../..');
const WEB=path.join(ROOT,'crates/plurxd/src/web');
const fixture=name=>JSON.parse(fs.readFileSync(path.join(__dirname,'fixtures/system-settings',name+'.json'),'utf8'));
const esc=value=>String(value??'').replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;').replaceAll('"','&quot;').replaceAll("'",'&#39;');
test('Capacity controls preserve untouched fields and send explicit dirty choices only',()=>{
  const h=harness();
  const row=(key,initialMode,initialValue,mode,value)=>({
    dataset:{capacityRow:key,initialMode,initialValue},
    querySelector:selector=>({value:selector==='[data-capacity-mode]'?mode:value})
  });
  const untouched=row('hardware_sessions','legacy_ambiguous','2','legacy_ambiguous','2');
  assert.equal(JSON.stringify(h.resourceCapacityChanges([untouched])), '{}');
  const automatic=row('software_pool_threads','legacy_ambiguous','15','automatic','15');
  assert.equal(JSON.stringify(h.resourceCapacityChanges([untouched,automatic])), '{"software_pool_threads":{"mode":"automatic"}}');
  const zero=row('hardware_sessions','legacy_ambiguous','2','explicit','0');
  assert.equal(JSON.stringify(h.resourceCapacityChanges([zero])), '{"hardware_sessions":{"mode":"explicit","value":0}}');
  for(const value of ['', '-1', '1.5', '9007199254740992'])assert.throws(()=>h.resourceCapacityChanges([row('hardware_sessions','automatic','','explicit',value)]),/nonnegative whole-number/);
});
test('Capacity presentation distinguishes legacy intent and kind-specific zero without a feature switch',()=>{
  const h=harness();
  const html=h.systemCapacityCard({transcode_capacity_revision:7,transcode_capacity_settings:{
    hardware_sessions:{state:'legacy_ambiguous',stored_value:0},software_pool_threads:{state:'absent',stored_value:null}
  }});
  assert.match(html,/Legacy value: confirm this limit or switch to Automatic/);
  assert.match(html,/Zero disables hardware sessions/);
  assert.match(html,/does not ban software playback/);
  assert.match(html,/no node-local default has been saved/);
  assert.doesNotMatch(html,/type="checkbox"|disabled|benchmark required/i);
});
function harness(extra={}){
  const context={esc,fmtAgo:()=>"just now",systemAttentionHtml:()=>"",...extra};
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(path.join(WEB,'pages/settings-system.js'),'utf8'),context);
  return context;
}
test('System renders approved card order and six factual disclosure states',()=>{
  const h=harness(),sys=fixture('macos'),html=h.systemPanel(sys);
  const names=['Versions','Transcoding','Additional media support','Playback performance','Storage','Logs'];
  for(let i=1;i<names.length;i++)assert.ok(html.indexOf(names[i-1])<html.indexOf(names[i]),names[i]);
  assert.equal((html.match(/data-capability=/g)||[]).length,6);
  assert.equal((html.match(/>Enabled<\/span>/g)||[]).length,6);
  assert.equal((html.match(/id="tone-map-census"/g)||[]).length,1,'one census owner');
  assert.doesNotMatch(html,/<details[^>]+\bopen\b/);
  assert.doesNotMatch(html,/On this node|managed transcode|7 days|Plurx package|Verified Plurx/);
  assert.ok(html.indexOf('Encoding preference')<html.indexOf('Hardware decoding'));
});
test('Versions uses server package pins independently from package verification',()=>{
  const h=harness(),sys=fixture('linux');
  sys.ffmpeg_support.required_package='9.7.2-6';
  sys.ffmpeg_support.expected_version='9.7.2-Jellyfin';
  const html=h.systemVersionsCard(sys);
  assert.match(html,/Jellyfin FFmpeg 9\.7\.2-6/);
  assert.match(html,/9\.7\.2-Jellyfin/);
  assert.doesNotMatch(html,/Required build verified/);
  assert.match(html,/not verified on this platform/);
});
test('Capability enablement follows configuration facts rather than acceptance wording',()=>{
  const h=harness(),sys=fixture('macos');
  sys.transcoding_facts[0].detail='VideoToolbox · acceptance still pending';
  sys.transcoding_facts[1].enabled=false;
  const html=h.transcoderCard(sys);
  assert.match(html,/data-capability="hardware_decoding"[^]*?>Enabled<\/span>/);
  assert.match(html,/data-capability="hardware_encoding"[^]*?>Disabled<\/span>/);
  assert.doesNotMatch(html,/verification incomplete/);
  const additional=h.systemAdditionalSupportCard(sys);
  assert.equal((additional.match(/>Available<\/span>/g)||[]).length,4);
  assert.doesNotMatch(additional,/>Enabled</);
});
test('Auto names the server-selected encoder in parentheses',()=>{
  const h=harness();
  for(const [name,encoder] of [['macos','Apple VideoToolbox'],['linux','Intel Quick Sync'],['unavailable','CPU']]){
    const sys=fixture(name);
    sys.hwaccel_pref='auto';sys.hwaccel_requested='auto';
    sys.encoder_selected=encoder;
    assert.ok(h.transcoderCard(sys).includes(`<option value="auto" selected>Auto (${encoder})</option>`));
  }
});
test('Unavailable media remains neutral Disabled without invented devices or timings',()=>{
  const h=harness(),html=h.systemPanel(fixture('unavailable'));
  assert.equal((html.match(/>Disabled<\/span>/g)||[]).length,6);
  assert.equal((html.match(/>Unavailable<\/span>/g)||[]).length,4);
  assert.match(html,/role="alert"/);
  assert.doesNotMatch(html,/>0\.00<|>NaN<|Graphics devices/);
});
test('Comparison separates incomplete and fallback work from completed measurements',()=>{
  const h=harness(),sys=fixture('linux');
  sys.transcoder_optimization.report={comparison:{version:1,status:'aborted',error:'Yielded to playback',cases:[{id:'case',label:'Real source'}],results:[
    {case_id:'case',backend:'software',decoder:'software',pipeline:'cpu',command:['-vf','<filter>'],completion:'complete',fallback:false,median_seconds:4.123,realtime_ratio:2.4,relative_to_cpu:1,cpu_percent:null},
    {case_id:'case',backend:'nvenc',completion:'complete',fallback:true,median_seconds:1.23,cpu_percent:0},
    {case_id:'case',backend:'qsv',completion:'unavailable',median_seconds:null,error:'No binding'}
  ]}};
  const html=h.systemPerformanceCard(sys);
  assert.match(html,/Comparison incomplete/);
  assert.match(html,/>4\.12<\/td>/);
  assert.match(html,/>Fallback<\/span>/);
  assert.doesNotMatch(html,/>1\.23<|Real-media tests complete|process CPU 0%|Intel VA-API/);
  assert.match(html,/2\.40× realtime/);
  assert.match(html,/1\.00× CPU baseline/);
  assert.match(html,/240 frames @ 24 fps \(10 seconds\)/);
  assert.match(html,/Warm-up \+ median of 3 runs/);
  assert.match(html,/H\.264 · 4 Mb\/s/);
  assert.match(html,/CPU decode · CPU processing/);
  assert.match(html,/&lt;filter&gt;/);
  assert.match(html,/<summary>Command<\/summary>/);
});
test('Collapsed logs never trigger initial or periodic reads',async()=>{
  const source=fs.readFileSync(path.join(WEB,'pages/cluster-troubleshooting.js'),'utf8');
  const fn=source.slice(source.indexOf('function refreshLogs(){'),source.indexOf('function refreshClusterLogs(){'));
  let count=0,open=false;
  const refresh=new Function('document','refreshLogStream',fn+';return refreshLogs;')({getElementById:()=>({open})},()=>{count++;return Promise.resolve();});
  await refresh();await refresh();assert.equal(count,0);
  open=true;await refresh();assert.equal(count,1);
});
test('Polling refreshes server facts while preserving an unsaved encoding preference',async()=>{
  const sys=fixture('macos'),latest=fixture('macos'),replaced=[];
  latest.transcoder_optimization={running:false};
  const field={value:'software',options:[{value:'auto'},{value:'software'},{value:'videotoolbox'}],focus(){this.focused=true;}};
  const status={textContent:''};
  const h=harness({settingsCurrent:()=>true,api:async()=>latest,SETTINGS_DATA:{sys},document:{activeElement:field,getElementById:id=>id==='node-hwaccel'?field:id==='node-hwaccel-status'?status:null}});
  h.replaceSystemCard=(id,html)=>replaced.push({id,html});
  await h.pollTranscoderOptimization(sys.node_id,1);
  assert.equal(field.value,'software');assert.equal(field.focused,true);
  assert.match(status.textContent,/Unsaved preference/);
  assert.deepEqual(replaced.map(item=>item.id),['system-versions','transcoder-card','system-performance']);
});
test('Pipeline columns reject duplicate measurements rather than choosing an arbitrary row',()=>{
  const h=harness(),sys=fixture('linux');
  sys.transcoder_optimization.comparison={status:'complete',cases:[{id:'case',label:'Real source'}],results:[
    {case_id:'case',backend:'nvenc',device_id:null,completion:'complete',fallback:false,median_seconds:2},
    {case_id:'case',backend:'qsv',device_id:'first-card',completion:'complete',fallback:false,median_seconds:3},
    {case_id:'case',backend:'qsv',device_id:'second-card',completion:'complete',fallback:false,median_seconds:4}
  ]};
  const html=h.systemPerformanceCard(sys),table=html.slice(html.indexOf('<table'),html.indexOf('</table>'));
  assert.match(table,/>2\.00<\/td>/);
  assert.doesNotMatch(table,/>0\.10<|>3\.00<|>4\.00</);
  assert.match(table,/See test details/);
  assert.match(html,/NVIDIA NVENC/);
  assert.match(html,/Intel Quick Sync/);
  assert.match(html,/VA-API/);
});

test('Storage remeasurement preserves open details and unsaved preference and rejects a different node',async()=>{
  const sys=fixture('macos'),data={sys},preference={value:'software'};
  const storage={ran:true,mounts:[]};
  let count=0,current;
  const card=open=>({
    details:[{open}],querySelectorAll(){return this.details;},
    set outerHTML(html){assert.match(html,/system-storage-card/);count++;current=card(false);}
  });
  current=card(true);
  const document={getElementById:id=>id==='system-storage-card'?current:id==='node-hwaccel'?preference:null};
  const h=harness({SETTINGS_DATA:data,document,api:async()=>storage,toast:()=>{}});
  await h.remeasureStorage({textContent:'Re-measure',disabled:false});
  assert.equal(sys.storage,storage);
  assert.equal(count,1);
  assert.equal(current.details[0].open,true,'the open Storage disclosure survives replacement');
  assert.equal(preference.value,'software','an unsaved encoding preference is not repainted');
  h.api=async()=>{data.sys=fixture('linux');data.sys.node_id='other-node';return {ran:false};};
  await h.remeasureStorage({textContent:'Re-measure',disabled:false});
  assert.equal(count,1);
  assert.notEqual(data.sys.storage.ran,false);
});

test('Resource ownership keeps unknown task and memory peaks distinct from reserved minima',()=>{
  const h=harness();
  const html=h.systemResourceOwnershipCard({resource_ownership:{state:'available',unreconciled_custody:true,accounting:{held:2,unknown_held:1,used:{peak_memory_bytes:4096},managed_memory_peak_bytes:{state:'unknown',value:'unbounded'},managed_kernel_task_peak:{state:'unknown',value:'unbounded'},capacity:{kernel_tasks:{state:'known',value:20}},capacity_observation_age_ms:{state:'unknown',value:'first_sample'}}}});
  assert.match(html,/Known reserved memory/);
  assert.match(html,/Service history is unknown/);
  assert.match(html,/Unknown \(unbounded\)/);
  assert.match(html,/Other playback paths are outside/);
  assert.match(html,/Unreconciled custody remains held/);
  assert.doesNotMatch(html,/type="checkbox"|disabled|>0<|NaN/);
  assert.match(h.systemResourceOwnershipCard({resource_ownership:{state:'accounting_unavailable',unreconciled_custody:null,accounting:null}}),/Accounting is unknown/);
  const fenced=h.systemResourceOwnershipCard({resource_ownership:{state:'available',unreconciled_custody:true,accounting:{held:1,unknown_held:1,used:{peak_memory_bytes:4096},capacity:{},native_overage_fenced:true}}});
  assert.match(fenced,/owned operation exceeded its reserved resources/);
  assert.match(fenced,/custody and extra charge remain held/);
  assert.match(fenced,/counted direct reads/);
  assert.doesNotMatch(fenced,/type="checkbox"|NaN/);
  const planning=h.systemResourceOwnershipCard({resource_ownership:{state:'available',accounting:{held:1,unknown_held:0,used:{peak_memory_bytes:8192},capacity:{},planning_metadata_held:1,planning_metadata_reserved_bytes:{state:'known',value:8192}}}});
  assert.match(planning,/Planning metadata: 1 held roles/);
  assert.match(planning,/8192 reserved bytes \(included above\)/);
  assert.match(planning,/cleanup records stay charged/);
  const planningWork=h.systemResourceOwnershipCard({resource_ownership:{state:'available',planning:{metadata_owners:2,native_roles_held:1,original_tasks_pending:1,cleanup_requested:1,uncertain_native_owners:1}}});
  assert.match(planningWork,/2 retained metadata owners; 1 native roles held/);
  assert.match(planningWork,/1 original tasks pending; 1 cleanup requests; 1 uncertain native owners/);
  const unavailable=h.systemResourceOwnershipCard({resource_ownership:{state:'available',planning:null}});
  assert.match(unavailable,/Planning owner details are unavailable/);
  assert.doesNotMatch(unavailable,/0 retained metadata owners/);
  assert.equal(h.systemResourceOwnershipCard({}), '');
});


test('Copy cleanup reports actual held charge and preserves unavailable inventory',()=>{
  const h=harness();
  const card=copy_cleanup=>h.systemResourceOwnershipCard({resource_ownership:{state:'available',copy_cleanup}});
  assert.doesNotMatch(card(undefined),/Copy cleanup|No copy cleanup holds/);
  assert.match(card(null),/Copy cleanup details are unavailable; no empty inventory is implied/);
  assert.doesNotMatch(card(null),/No copy cleanup holds|Reserved CPU/);
  assert.match(card([]),/No copy cleanup holds were observed/);
  const html=card([{preparation_id:'prep<unsafe>',graph_id:'graph"unsafe',reason:'Process launch <unknown>',held_resources:{
    cpu_ns_per_second:750000001,peak_memory_bytes:4097,handles:7,processes:1,hardware_slots:0,tuner_slots:2,scratch_bytes:8193
  }}]);
  assert.match(html,/prep&lt;unsafe&gt;/);
  assert.match(html,/graph&quot;unsafe/);
  assert.match(html,/Process launch &lt;unknown&gt;/);
  for(const [label,value] of [['CPU (ns per second)',750000001],['memory (bytes)',4097],['handles',7],['processes',1],['hardware slots',0],['tuner slots',2],['storage (bytes)',8193]]){
    assert.ok(html.includes(`Reserved ${label}</dt><dd>${value}</dd>`),label);
  }
  assert.match(html,/held for reconciliation/);
  assert.match(html,/A restart alone does not release this charge/);
  assert.doesNotMatch(html,/<button|type="checkbox"|onclick|<unsafe>|measured RSS<\/dd>/);
  const retry=card([{preparation_id:'same',reason:'Child termination is pending another cleanup attempt',held_resources:{handles:3}}]);
  assert.match(retry,/pending another cleanup attempt/);
  assert.match(retry,/graph identity unavailable/);
  assert.match(retry,/Reserved CPU \(ns per second\)<\/dt><dd>Unknown/);
  assert.match(retry,/Reserved handles<\/dt><dd>3/);
  assert.doesNotMatch(retry,/NaN|undefined/);
  const invalid=card([{preparation_id:'invalid',held_resources:{cpu_ns_per_second:Number.MAX_SAFE_INTEGER+1,peak_memory_bytes:-1}}]);
  assert.match(invalid,/Reserved CPU \(ns per second\)<\/dt><dd>Unknown/);
  assert.match(invalid,/Reserved memory \(bytes\)<\/dt><dd>Unknown/);
  assert.match(card([{preparation_id:'missing'}]),/Reserved handles<\/dt><dd>Unknown/);
});
