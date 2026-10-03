"use strict";
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const {test}=require("node:test");
const source=fs.readFileSync("crates/plurxd/src/web/core/file-context.js","utf8");
function harness(){
  const context=vm.createContext({});
  vm.runInContext(source+"\nthis.helper={localPlaybackFileContext,sharedPlaybackFileContextFromDetail,playbackFileKey,playbackFileUrl,playbackFileApiPath,withPlaybackFileSession,playbackFileContext};",context);
  return context.helper;
}
const reference={import_id:"11111111-1111-4111-8111-111111111111",server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"7",item_id:"9"};
function detail(ref=reference,locator="opaque_7-A"){return {file_base:`/api/v1/shared/imports/${ref.import_id}/files/${locator}`,id:"7"};}
test("same numeric source file cannot collide with local playback resources or source keys",()=>{
  const h=harness(),local=h.localPlaybackFileContext(7),shared=h.sharedPlaybackFileContextFromDetail(reference,detail());
  assert.equal(h.playbackFileUrl(local,"decision"),"/api/v1/files/7/decision");
  for(const suffix of ["decision","hls/sessions","direct","stream.mp4","subs/0.vtt","subs/0/overlay.json","chapters/0/thumb"]){
    const url=h.playbackFileUrl(shared,suffix);
    assert.equal(url,detail().file_base+"/"+suffix);
    assert.ok(!url.startsWith("/api/v1/files/7/"));
    assert.equal(h.playbackFileApiPath(shared,suffix),url.slice(7));
  }
  assert.notEqual(h.playbackFileKey(local),h.playbackFileKey(shared));
  const other=h.sharedPlaybackFileContextFromDetail({...reference,server_id:"44444444-4444-4444-8444-444444444444"},detail());
  assert.notEqual(h.playbackFileKey(shared),h.playbackFileKey(other));
});
test("shared detail rejects absolute, encoded, traversing, mismatched and local bases",()=>{
  const h=harness(),base=detail().file_base;
  for(const file_base of ["https://source/files/7","//source/files/7","/api/v1/files/7",base+"?session=x",base+"#x",base+"/../direct",base+"%2fthing",base+"%2e",base+"\\thing",base.replace(reference.import_id,reference.server_id),base.replace("opaque_7-A",""),base.replace("opaque_7-A","a".repeat(2049))]){
    assert.throws(()=>h.sharedPlaybackFileContextFromDetail(reference,{file_base}));
  }
  assert.throws(()=>h.sharedPlaybackFileContextFromDetail({...reference,item_id:7},detail()));
  assert.throws(()=>h.sharedPlaybackFileContextFromDetail({...reference,library_id:"9223372036854775808"},detail()));
  assert.throws(()=>h.playbackFileContext({source_ref:reference,file_base:base,id:7}));
});
test("contexts copy source authority and retain exact unsafe-for-Number source IDs",()=>{
  const h=harness(),ref={...reference,item_id:"9007199254740993"},file=detail(ref),c=h.sharedPlaybackFileContextFromDetail(ref,file);
  ref.item_id="1";file.file_base="/api/v1/files/7";
  assert.equal(c.source_ref.item_id,"9007199254740993");
  assert.ok(Object.isFrozen(c));assert.ok(Object.isFrozen(c.source_ref));
  assert.equal(h.playbackFileUrl(h.localPlaybackFileContext("9007199254740993"),"direct"),"/api/v1/files/9007199254740993/direct");
  assert.throws(()=>h.localPlaybackFileContext(9007199254740993));
});
test("file suffixes and query keys stay closed and typed",()=>{
  const h=harness(),c=h.sharedPlaybackFileContextFromDetail(reference,detail());
  for(const suffix of ["../direct","direct?url=x","/direct","subs/-1","subs/01","chapters/1000000/thumb","download","content","subs/0/overlay/../objects/x.png"])
    assert.throws(()=>h.playbackFileUrl(c,suffix));
  for(const query of [{url:"https://upstream"},{token:"secret"},{session:reference.import_id},{start:Infinity},{audio:1.1},{audio_offset_ms:15001},{vcodec:"hevc&token=x"},{force:"unknown"}])
    assert.throws(()=>h.playbackFileUrl(c,"stream.mp4",query));
  assert.equal(h.playbackFileUrl(c,"decision",{force:"auto",audio:0,subtitle:-1}),detail().file_base+"/decision?force=auto&audio=0&subtitle=-1");
  assert.equal(h.playbackFileUrl(c,"subs/0/overlay/"+"a".repeat(64)+"/objects/"+"b".repeat(64)+".png"),detail().file_base+"/subs/0/overlay/"+"a".repeat(64)+"/objects/"+"b".repeat(64)+".png");
});
test("only an exact admitted B UUID may bind headerless file delivery",()=>{
  const h=harness(),c=h.sharedPlaybackFileContextFromDetail(reference,detail()),id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",bound=h.withPlaybackFileSession(c,id);
  assert.equal(c.session_id,null);assert.equal(h.playbackFileKey(c),h.playbackFileKey(bound));
  assert.equal(h.playbackFileUrl(bound,"subs/0"),detail().file_base+"/subs/0?session="+id);
  assert.equal(h.playbackFileUrl(bound,"decision"),detail().file_base+"/decision");
  assert.equal(h.playbackFileUrl(bound,"hls/sessions"),detail().file_base+"/hls/sessions");
  for(const bad of [id.toUpperCase(),"7","../session",id.replace("4aaa","1aaa")])assert.throws(()=>h.withPlaybackFileSession(c,bad));
});
