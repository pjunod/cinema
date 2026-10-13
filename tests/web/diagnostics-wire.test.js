const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const context=vm.createContext({TextEncoder});
vm.runInContext(fs.readFileSync('crates/plurxd/src/web/core/diagnostics-wire.js','utf8')+'\nglobalThis.wire=CinemaDiagnosticWire;',context);
const fixture=JSON.parse(fs.readFileSync('crates/plurx-core/tests/fixtures/diagnostics-v1.json'));
test('shared diagnostic manifests reject privacy and encoding violations',()=>{
 for(const c of fixture.cases) { let valid=false;try {context.wire.manifest(c.json);valid=true;}catch{}assert.equal(valid,c.valid,c.name); }
});
test('readable summary distinguishes unavailable from zero evidence',()=>{
 const m=context.wire.manifest(fixture.cases[0].json);const s=context.wire.summary(m);
 assert.match(s,/server: unavailable \(connection_failed\)/);assert.doesNotMatch(s,/CANARY_SECRET/);
});
test('shared diagnostic events preserve codec layers and refuse private fields',()=>{
 const fixtures=JSON.parse(fs.readFileSync('crates/plurx-core/tests/fixtures/diagnostics-events-v1.json'));
 for(const c of fixtures.cases) { let valid=false;try {context.wire.event(c.json);valid=true;}catch{}assert.equal(valid,c.valid,c.name); }
});
