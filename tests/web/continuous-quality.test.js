"use strict";
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const context=vm.createContext({AbortController,TextDecoder,setTimeout,clearTimeout});
vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-quality.js','utf8'),context);
const {continuousQualityProtocol:protocol,continuousQualityResponse:valid}=context;
const uuid=n=>`00000000-0000-4000-8000-${String(n).padStart(12,'0')}`;
const bootstrap={generation:uuid(1),control_epoch:3,schedule_url:'/api/v1/hls/session/quality-schedule'};
const attachment={client_instance_id:uuid(2),lifetime_id:'one-film',attachment_id:uuid(3),family_id:'a'.repeat(64)};
const interval={artifact_id:'b'.repeat(64),rendition_id:'c'.repeat(64),timescale:24000,from_tick:0,through_tick:48048,byte_length:1000};
const clone=value=>JSON.parse(JSON.stringify(value));
function answer(request,revision=1){
 const transition=request.transition;
 const transaction={transaction_id:transition?.transaction_id||uuid(4),intent_revision:1,target_rendition_id:interval.rendition_id,
  state:'ready',intent_superseded:false,cancel_requested:false,ready:[interval],reserved:[],appended:[],
  ever_appended:false,disposed:[],first_presented_tick:null,first_presented_at_ms:null};
 const ledger={version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:clone(request.attachment),
  accepted_sequence:transition?.sequence||0,latest_intent_revision:1,transactions:[clone(transaction)]};
 const receipt=transition?{version:1,generation:request.generation,control_epoch:request.control_epoch,
  accepted_sequence:transition.sequence,attachment:clone(request.attachment),transaction}:null;
 return {version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:clone(request.attachment),revision,ledger,receipt};
}
test('lost acknowledgement retries exactly before a newer sequence',async()=>{
 const requests=[];let lost=true;
 const client=protocol(bootstrap,attachment,async(url,request)=>{
  requests.push(clone(request));if(lost){lost=false;throw new Error('ack lost');}return answer(request,requests.length);
 });
 const first=client.transition(uuid(4),{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id},{timescale:24000,through_tick:0});
 const second=client.transition(uuid(4),{kind:'scheduled',intervals:[interval]});
 await Promise.all([first,second]);
 assert.deepEqual(requests[0],requests[1]);assert.equal(requests[2].transition.sequence,2);
 assert.equal(client.pending,null);
});
test('queued operation mutation cannot change the transmitted command',async()=>{
 let release;const paused=new Promise(resolve=>release=resolve),requests=[];
 const client=protocol(bootstrap,attachment,async(url,request)=>{requests.push(clone(request));if(requests.length===1)await paused;return answer(request,requests.length);});
 const first=client.snapshot();const operation={kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id};
 const frontier={timescale:24000,through_tick:0};const next=client.transition(uuid(4),operation,frontier);
 operation.target_rendition_id='d'.repeat(64);frontier.through_tick=999;
 release();await Promise.all([first,next]);
 assert.equal(requests[1].transition.operation.target_rendition_id,interval.rendition_id);assert.equal(requests[1].frontier.through_tick,0);
});
test('an unresolved exchange stays ahead of newer work',async()=>{
 let refuse=true;const requests=[];
 const client=protocol(bootstrap,attachment,async(url,request)=>{requests.push(clone(request));if(refuse)throw new Error('disconnected');return answer(request,requests.length);});
 await assert.rejects(client.transition(uuid(4),{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id},{timescale:24000,through_tick:0}));
 assert.ok(client.pending);refuse=false;
 await client.transition(uuid(4),{kind:'scheduled',intervals:[interval]});
 assert.deepEqual(requests[0],requests[2]);assert.equal(requests[3].transition.sequence,2);
});
test('wrong owner, attachment and unreserved append facts refuse receipts',()=>{
 const request={version:1,...bootstrap,attachment,transition:{sequence:1,transaction_id:uuid(4),operation:{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id}}};
 const response=answer(request);assert.equal(valid(response,request),true);
 const owner=clone(response);owner.control_epoch++;assert.equal(valid(owner,request),false);
 const wrong=clone(response);wrong.ledger.attachment.attachment_id=uuid(99);assert.equal(valid(wrong,request),false);
 const unreserved=clone(response);unreserved.ledger.transactions[0].ever_appended=true;unreserved.ledger.transactions[0].appended=[interval];
 assert.equal(valid(unreserved,request),false);
});
