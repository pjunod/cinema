"use strict";
// ---- Library channels ----------------------------------------------------
// One source adapter over the shared finite player. It owns schedule/tune
// intent; the existing player continues to own HLS lifecycle and controls.
const LIBRARY_CHANNEL_TUNE=new LibraryChannelCore.TuneFence();
const LIBRARY_CHANNEL_CLOCK=new LibraryChannelCore.ServerClock();
let LIBRARY_CHANNELS={channels:[],guide:[],draft:null,libs:[],search:[],editor:false,error:null,management:false};
let LIBRARY_CHANNEL_TRANSITIONS=0;
let LIBRARY_CHANNEL_BOUNDARY_TIMER=null;

async function loadLibraryChannelGuide(ids,from=null,to=null){
  const rows=[];
  for(let offset=0;offset<ids.length;offset+=20){
    const group=ids.slice(offset,offset+20);let cursor=null;
    do{
      const query=new URLSearchParams({channel_ids:group.join(",")});
      if(from!==null)query.set("start_ms",String(from));if(to!==null)query.set("end_ms",String(to));
      if(cursor)query.set("cursor",cursor);
      const response=await api(`/library-channels/guide?${query}`,{raw:true});
      rows.push(...await response.json());cursor=response.headers.get("x-plurx-next-cursor");
    }while(cursor);
  }
  return rows.sort((a,b)=>a.starts_at_ms-b.starts_at_ms||String(a.channel_id).localeCompare(String(b.channel_id)));
}
async function loadLibraryChannels(){
  const result=[];let after=null;
  do{
    const query=new URLSearchParams({limit:"100"});
    if(LIBRARY_CHANNELS.management)query.set("management","true");if(after)query.set("after",after);
    const page=await api(`/library-channels?${query}`);result.push(...page);
    after=page.length===100?page[page.length-1].id:null;
  }while(after);
  return result;
}

function libraryChannelDraftKey(id=null){
  const server=(SERVER&&SERVER.instance_id)||location.origin;
  return `plurx_library_channel_draft_v1:${server}:${ME&&ME.id||"unknown"}${id?`:edit:${encodeURIComponent(id)}`:""}`;
}
function loadLibraryChannelDraft(id=null){
  try{
    const parsed=JSON.parse(sessionStorage.getItem(libraryChannelDraftKey(id))||"null");
    if(parsed&&parsed.server===String((SERVER&&SERVER.instance_id)||location.origin)
      &&parsed.user===String(ME&&ME.id||"")&&(parsed.id||null)===id) return parsed;
  }catch(e){}
  return LibraryChannelCore.emptyDraft({server:(SERVER&&SERVER.instance_id)||location.origin,user:ME&&ME.id});
}
function saveLibraryChannelDraft(){
  const draft=LIBRARY_CHANNELS.draft;if(!draft)return;
  try{sessionStorage.setItem(libraryChannelDraftKey(draft.id),JSON.stringify(draft));}catch(e){}
}
function clearLibraryChannelDraft(all=false){
  try{
    if(all){
      const base=libraryChannelDraftKey();
      for(let i=sessionStorage.length-1;i>=0;i--){
        const key=sessionStorage.key(i);
        if(key===base||key?.startsWith(`${base}:edit:`))sessionStorage.removeItem(key);
      }
    }else sessionStorage.removeItem(libraryChannelDraftKey(LIBRARY_CHANNELS.draft?.id));
  }catch(e){}
  if(all){LIBRARY_CHANNELS.draft=null;LIBRARY_CHANNELS.editor=false;}
}
function makeLibraryChannelFromItem(itemId,kind,title){
  const draft=LibraryChannelCore.emptyDraft({server:(SERVER&&SERVER.instance_id)||location.origin,user:ME&&ME.id});
  draft.name=String(title||"New channel").slice(0,80);
  if(kind==="show")draft.recipe.include_show_ids=[Number(itemId)];
  else draft.recipe.include_item_ids=[Number(itemId)];
  draft.dirty=true;LIBRARY_CHANNELS.draft=draft;saveLibraryChannelDraft();
  LIBRARY_CHANNELS.openEditorOnLoad=true;location.hash="#/library-channels";
}
function lcTime(value){return new Date(Number(value)).toLocaleTimeString([],{hour:"numeric",minute:"2-digit"});}
function lcDuration(ms){return fmtDur(Number(ms)||0);}
function lcNowFor(channel){return channel&&channel.now||null;}

async function viewLibraryChannels(generation=PAGE_RENDER_GENERATION){
  layoutChrome("library-channels",`<div class="empty">Loading Library channels…</div>`);
  setPagePhase("#/library-channels",generation,"shell");
  try{
    const channels=await loadLibraryChannels();
    if(generation!==PAGE_RENDER_GENERATION)return;
    LIBRARY_CHANNELS.channels=channels||[];
    const ids=LIBRARY_CHANNELS.channels.map(channel=>channel.id);
    LIBRARY_CHANNELS.guide=ids.length?await loadLibraryChannelGuide(ids):[];
    if(generation!==PAGE_RENDER_GENERATION)return;
    if(LIBRARY_CHANNELS.openEditorOnLoad){
      LIBRARY_CHANNELS.openEditorOnLoad=false;
      await openLibraryChannelEditor();
      return;
    }
    libraryChannelsPaint();
    setPagePhase("#/library-channels",generation,"content");
    setPageTimer(()=>refreshLibraryChannels(generation),30000,generation);
    setPagePhase("#/library-channels",generation,"settled");
  }catch(error){
    if(generation!==PAGE_RENDER_GENERATION)return;
    const view=LibraryChannelCore.errorView(error,{collection:true});
    layoutChrome("library-channels",`<div class="empty"><b>${esc(view.title)}</b><br>${esc(view.detail)}<div class="lc-actions"><button onclick="viewLibraryChannels()">Retry</button></div></div>`);
    setPageFailure("#/library-channels",generation,view.code);
  }
}
async function refreshLibraryChannels(generation){
  if(location.hash!=="#/library-channels"||generation!==PAGE_RENDER_GENERATION||LIBRARY_CHANNELS.editor)return;
  try{
    LIBRARY_CHANNELS.channels=await loadLibraryChannels();
    const ids=LIBRARY_CHANNELS.channels.map(channel=>channel.id);
    LIBRARY_CHANNELS.guide=ids.length?await loadLibraryChannelGuide(ids):[];
    if(location.hash==="#/library-channels"&&generation===PAGE_RENDER_GENERATION)libraryChannelsPaint();
  }catch(error){LIBRARY_CHANNELS.error=error;}
}
function libraryChannelsPaint(){
  const main=document.getElementById("main");if(!main)return;
  if(LIBRARY_CHANNELS.editor){lcPaintEditor(main);return;}
  const channels=LIBRARY_CHANNELS.channels||[];
  const tools=`<button class="primary" onclick="openLibraryChannelEditor()">Create channel</button>${ME&&ME.is_admin?`<button class="ghost" onclick="toggleLibraryChannelManagement()">${LIBRARY_CHANNELS.management?'Leave management':'Manage all channels'}</button>`:""}`;
  if(!channels.length){
    main.innerHTML=`${setHead("Library channels","Turn movies and episodes into a shared, always-on schedule.",tools)}<div class="empty">No Library channels yet.<br>Start with an idea, then preview what will play.</div><div class="lc-empty-start">${[["space","Space documentaries","Explore your documentary collection"],["standup","Stand-up comedy","Comedy specials, ready to tune into"],["all","Your whole library","A rotation of titles you choose"]].map(([id,title,detail])=>`<button class="ghost" onclick="openLibraryChannelEditor().then(()=>lcPreset('${id}'))"><strong>${title}</strong><span>${detail}</span></button>`).join("")}</div>`;
    return;
  }
  const cards=channels.map(channel=>{
    const now=lcNowFor(channel),next=channel.next;
    return `<article class="card lc-card"><div class="row"><span class="pill">LIBRARY</span>${channel.favourite?'<span title="Favourite">★</span>':''}<span class="spacer"></span><button class="ghost sm" onclick="toggleLibraryChannelFavourite('${esc(channel.id)}',${!channel.favourite})">${channel.favourite?'Unfavourite':'Favourite'}</button></div>
      <h2>${esc(channel.name)}</h2><div class="lc-now">${esc(now?now.title:"No schedule yet")}</div>
      <div class="lc-time">${now?`${lcTime(now.starts_at_ms)}–${lcTime(now.ends_at_ms)}`:"Preview or edit the selection"}</div>
      <div class="lc-next">${next?`Next: ${esc(next.title)} · ${lcTime(next.starts_at_ms)}`:""}</div>
      <div class="lc-actions">${now&&channel.enabled?`<button class="primary" onclick="libraryChannelTune('${esc(channel.id)}')">Tune in</button><button class="ghost" onclick="libraryChannelWatchProgramme('${esc(channel.id)}',${now.file_id},${now.item_id})">Watch from start</button>`:""}${channel.can_edit?`<button class="ghost" onclick="openLibraryChannelEditor('${esc(channel.id)}')">Edit</button>${now?`<button class="ghost" onclick="rebuildLibraryChannel('${esc(channel.id)}',${channel.revision},'next_programme',false)">Apply after this programme</button><button class="ghost" onclick="rebuildLibraryChannel('${esc(channel.id)}',${channel.revision},'next_rotation',true)">Reshuffle next rotation</button>`:""}`:""}</div></article>`;
  }).join("");
  const guide=(LIBRARY_CHANNELS.guide||[]).map(programme=>`<div class="lc-programme"><time>${lcTime(programme.starts_at_ms)}</time><span><b>${esc(programme.title)}</b><small>${esc((channels.find(channel=>channel.id===programme.channel_id)||{}).name||"")}</small></span><button class="ghost sm" onclick="libraryChannelWatchProgramme('${esc(programme.channel_id)}',${programme.file_id},${programme.item_id})">Watch from start</button></div>`).join("");
  main.innerHTML=`${setHead("Library channels","What is on now and what follows. Channel viewing does not change watch history.",tools)}<div class="lc-grid">${cards}</div><h2 class="section">Guide</h2><div class="card lc-guide">${guide||'<div class="empty">No scheduled programmes.</div>'}</div>`;
}
async function toggleLibraryChannelManagement(){LIBRARY_CHANNELS.management=!LIBRARY_CHANNELS.management;await viewLibraryChannels(PAGE_RENDER_GENERATION);}

async function openLibraryChannelEditor(id=null){
  let draft=id?null:loadLibraryChannelDraft();
  if(id){
    const channel=await api(`/library-channels/${encodeURIComponent(id)}`);
    draft={server:String((SERVER&&SERVER.instance_id)||location.origin),user:String(ME&&ME.id||""),step:"content",dirty:false,
      id:channel.id,expected_revision:channel.revision,request_id:null,name:channel.name,
      description:channel.description,visibility:channel.visibility,enabled:channel.enabled,
      recipe:channel.subject_recipe||channel.recipe,preview:null,subjectPreview:channel.matching?{...channel.matching,rows:[]}:null,subjectSaved:!!channel.matching};
    if(channel.matching)draft.subjectRecipe=JSON.stringify(draft.recipe);
    const saved=loadLibraryChannelDraft(id);
    if(saved.id===id&&saved.dirty)draft=saved;
  }
  LIBRARY_CHANNELS.draft=draft;LIBRARY_CHANNELS.editor=true;LIBRARY_CHANNELS.search=[];
  try{LIBRARY_CHANNELS.libs=await api("/libraries");}catch(e){LIBRARY_CHANNELS.libs=[];}
  libraryChannelsPaint();
  const selected=[...new Set(['include_item_ids','include_show_ids','exclude_item_ids','exclude_show_ids'].flatMap(key=>draft.recipe[key]||[]).map(String))];
  draft.titleNames=draft.titleNames||{};
  for(let at=0;at<selected.length;at+=4){
    await Promise.allSettled(selected.slice(at,at+4).filter(key=>!draft.titleNames[key]).map(async key=>{
      const result=await api(`/items/${encodeURIComponent(key)}`);draft.titleNames[key]=result.item.title;
    }));
    if(LIBRARY_CHANNELS.draft!==draft||!LIBRARY_CHANNELS.editor)return;
  }
  if(LIBRARY_CHANNELS.draft.subjectPreview&&LIBRARY_CHANNELS.draft.subjectRecipe===JSON.stringify(LIBRARY_CHANNELS.draft.recipe)){
    const request=(LIBRARY_CHANNELS.previewRequest||0)+1;LIBRARY_CHANNELS.previewRequest=request;
    lcPollSubject(LIBRARY_CHANNELS.draft,request);
  }else if(draft.id||draft.recipe.subject||draft.recipe.match_all_in_scope||draft.recipe.include_item_ids.length||draft.recipe.include_show_ids.length)await previewLibraryChannel(false);
  libraryChannelsPaint();
}
function closeLibraryChannelEditor(){
  lcReadEditor();
  const draft=LIBRARY_CHANNELS.draft;
  if(draft&&draft.dirty&&!confirm("Discard unsaved channel changes?"))return;
  clearLibraryChannelDraft();
  LIBRARY_CHANNELS.editor=false;LIBRARY_CHANNELS.draft=null;libraryChannelsPaint();
}
function lcReadEditor(){
  const draft=LIBRARY_CHANNELS.draft;if(!draft)return;
  const previousRecipe=JSON.stringify(draft.recipe);
  const value=id=>document.getElementById(id)?.value;
  let touched=false;
  for(const id of ["lc-name","lc-description","lc-visibility","lc-enabled","lc-order","lc-specials","lc-refresh","lc-all","lc-movies","lc-episodes","lc-genres","lc-tags","lc-keywords","lc-year-min","lc-year-max"])
    if(document.getElementById(id)){touched=true;break;}
  if(document.getElementById("lc-subject")){
    draft.recipe.subject=(value("lc-subject")||"").trim().normalize("NFC")||null;
  }
  if(document.getElementById("lc-name"))draft.name=value("lc-name")||"";
  if(document.getElementById("lc-description"))draft.description=value("lc-description")||"";
  if(document.getElementById("lc-visibility"))draft.visibility=value("lc-visibility")||"personal";
  if(document.getElementById("lc-enabled"))draft.enabled=!!document.getElementById("lc-enabled").checked;
  if(document.getElementById("lc-order"))draft.recipe.ordering=value("lc-order");
  for(const [id,key] of [["lc-specials","include_specials"],["lc-refresh","auto_refresh"],["lc-all","match_all_in_scope"]])
    if(document.getElementById(id))draft.recipe[key]=!!document.getElementById(id).checked;
  if(document.getElementById("lc-movies"))draft.recipe.kinds=[...(document.getElementById("lc-movies").checked?["movie"]:[]),...(document.getElementById("lc-episodes").checked?["episode"]:[])];
  for(const [id,key] of [["lc-genres","genres_any"],["lc-tags","tags_any"],["lc-keywords","keywords_any"]])
    if(document.getElementById(id))draft.recipe[key]=LibraryChannelCore.cleanStrings(value(id));
  if(document.getElementById("lc-year-min"))draft.recipe.year_min=value("lc-year-min")?Number(value("lc-year-min")):null;
  if(document.getElementById("lc-year-max"))draft.recipe.year_max=value("lc-year-max")?Number(value("lc-year-max")):null;
  const libraryChecks=document.querySelectorAll("[data-lc-library]");
  if(libraryChecks.length)draft.recipe.library_ids=Array.from(libraryChecks).filter(node=>node.checked).map(node=>Number(node.dataset.lcLibrary));
  if(previousRecipe!==JSON.stringify(draft.recipe)){draft.preview=null;draft.previewError=null;}
  if(touched)draft.dirty=true;
  saveLibraryChannelDraft();
}
function lcStep(step){
  lcReadEditor();const draft=LIBRARY_CHANNELS.draft;
  if(LibraryChannelCore.STEPS.indexOf(step)>LibraryChannelCore.STEPS.indexOf(draft.step)){
    draft.showValidation=true;if(Object.keys(lcEditorErrors(draft)).length){libraryChannelsPaint();return;}
  }
  if(draft.step==="content"&&step!=="content"&&!draft.id&&!draft.name&&draft.recipe.subject)draft.name=Array.from(draft.recipe.subject).slice(0,80).join("");
  draft.step=step;draft.showValidation=false;saveLibraryChannelDraft();libraryChannelsPaint();
}
async function previewLibraryChannel(paint=true){
  lcReadEditor();const draft=LIBRARY_CHANNELS.draft;if(!draft)return;
  const request=(LIBRARY_CHANNELS.previewRequest||0)+1;LIBRARY_CHANNELS.previewRequest=request;
  const recipe=JSON.stringify(draft.recipe),seed=draft.preview&&draft.preview.preview_seed;
  const current=()=>{
    if(LIBRARY_CHANNELS.draft!==draft||LIBRARY_CHANNELS.previewRequest!==request)return false;
    lcReadEditor();return JSON.stringify(draft.recipe)===recipe;
  };
  draft.preview=null;draft.previewError=null;draft.previewBusy=true;
  if(paint)libraryChannelsPaint();
  try{
    if(draft.recipe.subject){
      const preview=await api("/library-channels/subject-previews",{method:"POST",body:{recipe:JSON.parse(recipe),request_id:newRequestId(),preview_seed:seed||draft.subjectPreview?.preview_seed||null}});
      if(!current()){if(LIBRARY_CHANNELS.previewRequest===request)draft.previewBusy=false;return;}draft.previewBusy=false;draft.subjectSaved=false;draft.subjectPreview=preview;draft.subjectRecipe=recipe;
      saveLibraryChannelDraft();if(paint)libraryChannelsPaint();lcPollSubject(draft,request);return;
    }
    draft.subjectPreview=null;
    const preview=await api("/library-channels/preview",{method:"POST",body:{recipe:JSON.parse(recipe),limit:50,preview_seed:seed}});
    if(!current()){if(LIBRARY_CHANNELS.previewRequest===request)draft.previewBusy=false;return;}draft.previewBusy=false;draft.preview=preview;
  }
  catch(error){if(!current()){if(LIBRARY_CHANNELS.previewRequest===request)draft.previewBusy=false;return;}draft.previewBusy=false;draft.previewError=error.message;}
  saveLibraryChannelDraft();if(paint)libraryChannelsPaint();
}
async function lcPollSubject(draft,request,cursor=null){
  clearTimeout(LIBRARY_CHANNELS.subjectTimer);
  if(document.hidden){LIBRARY_CHANNELS.subjectTimer=setTimeout(()=>lcPollSubject(draft,request),10000);return;}
  const poll=LIBRARY_CHANNELS.subjectPoll=(LIBRARY_CHANNELS.subjectPoll||0)+1;
  const current=()=>LIBRARY_CHANNELS.subjectPoll===poll&&LIBRARY_CHANNELS.editor&&LIBRARY_CHANNELS.draft===draft&&LIBRARY_CHANNELS.previewRequest===request&&location.hash==="#/library-channels";
  if(!current()||!draft.subjectPreview)return;
  lcReadEditor();if(JSON.stringify(draft.recipe)!==draft.subjectRecipe)return;
  try{
    const path=`/library-channels/subject-previews/${encodeURIComponent(draft.subjectPreview.job_id)}?verdict=${encodeURIComponent(draft.subjectFilter||"match")}${cursor?`&cursor=${encodeURIComponent(cursor)}`:""}`;
    const result=await api(path);
    if(!current())return;lcReadEditor();if(JSON.stringify(draft.recipe)!==draft.subjectRecipe)return;
    draft.subjectPreview=result;saveLibraryChannelDraft();libraryChannelsPaint();
    if(["queued","running","waiting_for_provider"].includes(result.state))LIBRARY_CHANNELS.subjectTimer=setTimeout(()=>lcPollSubject(draft,request),result.state==="waiting_for_provider"?10000:2000);
  }catch(error){
    if(!current())return;
    if(error.status===409){LIBRARY_CHANNELS.subjectTimer=setTimeout(()=>lcPollSubject(draft,request),2000);return;}
    draft.previewError=error.message;libraryChannelsPaint();
  }
}
function lcSubjectFilter(filter){const d=LIBRARY_CHANNELS.draft;d.subjectFilter=filter;return lcPollSubject(d,LIBRARY_CHANNELS.previewRequest);}
function lcSubjectPage(){const d=LIBRARY_CHANNELS.draft;return lcPollSubject(d,LIBRARY_CHANNELS.previewRequest,d.subjectPreview.next_cursor);}
async function lcCancelSubject(){const d=LIBRARY_CHANNELS.draft;if(!d.subjectPreview||d.subjectSaved)return;await api(`/library-channels/subject-previews/${encodeURIComponent(d.subjectPreview.job_id)}`,{method:"DELETE"});d.subjectPreview.state="cancelled";saveLibraryChannelDraft();libraryChannelsPaint();}
async function lcSearchTitles(){
  const input=document.getElementById("lc-search"),q=input?.value.trim();if(!q)return;
  lcReadEditor();const draft=LIBRARY_CHANNELS.draft;if(!draft)return;
  draft.searchQuery=input.value;
  const request=(LIBRARY_CHANNELS.searchRequest||0)+1;LIBRARY_CHANNELS.searchRequest=request;
  const current=()=>LIBRARY_CHANNELS.editor&&LIBRARY_CHANNELS.draft===draft&&LIBRARY_CHANNELS.searchRequest===request&&draft.searchQuery.trim()===q;
  try{
    const result=await api(`/search?q=${encodeURIComponent(q)}`);
    if(!current())return;
    lcReadEditor();
    LIBRARY_CHANNELS.search=(result.results||result.items||result||[]).filter(item=>["movie","show","episode"].includes(item.kind)).slice(0,12);
    draft.searchMessage=LIBRARY_CHANNELS.search.length?null:`No movies, series or episodes found for “${q}”. Try another title.`;
    libraryChannelsPaint();
  }
  catch(error){if(!current())return;lcReadEditor();LIBRARY_CHANNELS.search=[];draft.searchMessage=`Title search failed: ${error.message}`;libraryChannelsPaint();}
}

function lcAddTitle(id,kind){
  lcReadEditor();lcRememberTitle(id);
  const recipe=LIBRARY_CHANNELS.draft.recipe,key=kind==="show"?"include_show_ids":"include_item_ids";
  recipe[key.replace("include_","exclude_")]=recipe[key.replace("include_","exclude_")].filter(value=>String(value)!==String(id));
  if(!recipe[key].some(value=>String(value)===String(id)))recipe[key].push(String(id));
  LIBRARY_CHANNELS.draft.dirty=true;saveLibraryChannelDraft();previewLibraryChannel();
}
function lcExcludeTitle(id,kind){
  lcReadEditor();lcRememberTitle(id);
  const recipe=LIBRARY_CHANNELS.draft.recipe,key=kind==="show"?"exclude_show_ids":"exclude_item_ids";
  recipe[key.replace("exclude_","include_")]=recipe[key.replace("exclude_","include_")].filter(value=>String(value)!==String(id));
  if(!recipe[key].some(value=>String(value)===String(id)))recipe[key].push(String(id));
  LIBRARY_CHANNELS.draft.dirty=true;saveLibraryChannelDraft();previewLibraryChannel();
}
function lcPreset(name){
  lcReadEditor();const recipe=LIBRARY_CHANNELS.draft.recipe;
  recipe.match_all_in_scope=false;recipe.genres_any=[];recipe.tags_any=[];recipe.keywords_any=[];recipe.year_min=null;recipe.year_max=null;
  const subjects={space:"format:documentary topic:space",nineties:"genre:comedy year:1990-1999",noir:"noir",standup:"format:stand-up -format:sitcom -format:talk-show -format:documentary"};
  recipe.subject=subjects[name]||null;
  if(name==="all")recipe.match_all_in_scope=true;
  if(!LIBRARY_CHANNELS.draft.id&&!LIBRARY_CHANNELS.draft.name&&recipe.subject)LIBRARY_CHANNELS.draft.name=({space:"Space documentaries",nineties:"90s comedy",noir:"Film noir",standup:"Stand-up comedy"})[name]||"New channel";
  LIBRARY_CHANNELS.draft.dirty=true;LIBRARY_CHANNELS.draft.preview=null;saveLibraryChannelDraft();
  libraryChannelsPaint();return previewLibraryChannel();
}
function lcRemoveTitle(id,key){
  LIBRARY_CHANNELS.draft.recipe[key]=LIBRARY_CHANNELS.draft.recipe[key].filter(value=>String(value)!==String(id));
  LIBRARY_CHANNELS.draft.dirty=true;saveLibraryChannelDraft();previewLibraryChannel();
}
function lcEditorErrors(draft){
  const errors=LibraryChannelCore.validateDraft(draft);
  if(!draft.showValidation)return {};
  if(draft.step==='content')return Object.fromEntries(Object.entries(errors).filter(([key])=>['subject','kinds'].includes(key)));
  if(draft.step==='playback')return {};
  return errors;
}
function lcSelectedTitles(recipe){
  const names=LIBRARY_CHANNELS.draft.titleNames||{};
  return ['include_item_ids','include_show_ids','exclude_item_ids','exclude_show_ids'].map(key=>{
    const ids=recipe[key]||[];if(!ids.length)return '';
    return `<div class="lc-selected"><b>${key.startsWith('exclude')?'Excluded':'Included'} ${key.includes('show')?'series':'titles'}</b><div class="lc-actions">${ids.map(id=>`<button class="ghost sm" onclick='lcRemoveTitle(${esc(JSON.stringify(String(id)))},${esc(JSON.stringify(key))})' aria-label="Remove ${esc(names[String(id)]||`item ${id}`)}">${esc(names[String(id)]||`Item ${id}`)} ×</button>`).join('')}</div></div>`;
  }).join('');
}
function lcRememberTitle(id){
  const d=LIBRARY_CHANNELS.draft;if(!d)return;
  const items=[...(LIBRARY_CHANNELS.search||[]),...(d.subjectPreview?.rows||[]).map(r=>({id:r.item_id,title:r.title})),...(d.preview?.matches||[]).map(r=>r.candidate)];
  const item=items.find(it=>exactWireId(it)===String(id));
  if(item)(d.titleNames||(d.titleNames={}))[String(id)]=item.title;
}
function lcPaintEditor(main){
  const focused=document.activeElement,id=focused&&main.contains(focused)?focused.id:null;
  const selection=id&&typeof focused.selectionStart==='number'?[focused.selectionStart,focused.selectionEnd]:null;
  const opened=[...main.querySelectorAll('.lc-editor details')].map(d=>d.open);
  const search=main.querySelector('#lc-search')?.value;
  main.innerHTML=libraryChannelEditorHtml();
  if(search!=null&&main.querySelector('#lc-search'))main.querySelector('#lc-search').value=search;
  main.querySelectorAll('.lc-editor details').forEach((d,i)=>{if(opened[i]!=null)d.open=opened[i];});
  const again=id&&document.getElementById(id);if(again){again.focus({preventScroll:true});if(selection)try{again.setSelectionRange(...selection);}catch(e){}}
}
// Editing a recipe invalidates the visible preview immediately, without
// repainting the form or moving the insertion point.
document.addEventListener('input',event=>{
  if(!event.target.closest||!event.target.closest('.lc-editor')||event.target.id==='lc-search')return;
  const draft=LIBRARY_CHANNELS.draft;if(!draft)return;
  const before=JSON.stringify(draft.recipe);lcReadEditor();
  if(before!==JSON.stringify(draft.recipe)){
    const preview=document.querySelector('.lc-preview');
    if(preview)preview.innerHTML='<span class="pill">PREVIEW OUT OF DATE</span><h2>Your choices changed</h2><p>Update the preview to see matching titles for these choices.</p><button class="ghost" onclick="previewLibraryChannel()">Update preview</button>';
  }
});

function lcSubjectStatus(subject){
  if(subject.state==="waiting_for_provider"){
    const reason=subject.error==="provider_model_missing"
      ? "The subject matching model is not installed on the server."
      : subject.error==="provider_timeout"
        ? "The subject matching service is taking too long to respond."
        : "The server cannot use the subject matching service.";
    return `<h2>Subject matching is unavailable</h2><p>${reason} Matching will resume automatically when the service is ready.</p><p class="hint">${subject.processed} of ${subject.total} titles checked; ${subject.matched} matches so far. You can still search for specific titles and include them manually.</p>${ME&&ME.is_admin?'<a href="#/settings/livetv">Check subject matching in Live TV settings</a>':'<p class="hint">Ask your server administrator to check the subject matching service.</p>'}`;
  }
  const status=subject.complete?"Scan complete.":({queued:"Waiting to start matching.",running:"Selection is partial; matching continues.",cancelled:"Preview cancelled.",failed:"Matching stopped. Update the preview to try again."}[subject.state]||"Matching is paused.");
  return `<h2>${subject.matched} matches; checked ${subject.processed} of ${subject.total}</h2><p>${status}</p>${subject.error?`<p class="hint">${esc(subject.error)}</p>`:''}`;
}
function libraryChannelEditorHtml(){
  const draft=LIBRARY_CHANNELS.draft,step=draft.step||"content",recipe=draft.recipe;
  const stepNames={content:"1 · Choose content",playback:"2 · Playback",channel:"3 · Name and create"};
  const steps=LibraryChannelCore.STEPS.map(name=>`<button class="pill ${name===step?'on':''}" aria-current="${name===step?'step':'false'}" onclick="lcStep('${name}')">${stepNames[name]}</button>`).join("");
  const libs=(LIBRARY_CHANNELS.libs||[]).filter(lib=>lib.kind==="movies"||lib.kind==="shows").map(lib=>`<label><input type="checkbox" data-lc-library="${lib.id}" ${recipe.library_ids.some(id=>String(id)===String(lib.id))?'checked':''}>${esc(lib.name)}</label>`).join("")||"<span class=\"muted\">No movie or TV libraries.</span>";
  let fields="";
  if(step==="content")fields=`<h2 class="lc-intro">What should this channel play?</h2><label for="lc-subject">Search and rules</label><textarea id="lc-subject" placeholder='e.g. format:documentary topic:space'>${esc(recipe.subject||"")}</textarea><p class="hint">Choose a preset, or search using words, &quot;quoted phrases&quot;, and -exclusions. Use format:, topic:, genre: and year: for precise rules. Matching runs locally using your catalogue and generated labels.</p><div class="lc-actions"><button class="ghost sm" onclick="lcPreset('standup')">Stand-up comedy</button><button class="ghost sm" onclick="lcPreset('space')">Space documentaries</button><button class="ghost sm" onclick="lcPreset('nineties')">'90s comedy</button><button class="ghost sm" onclick="lcPreset('noir')">Film noir</button><button class="ghost sm" onclick="lcPreset('all')">All titles in scope</button></div><div class="lc-checks"><label><input id="lc-movies" type="checkbox" ${recipe.kinds.includes("movie")?'checked':''}>Movies</label><label><input id="lc-episodes" type="checkbox" ${recipe.kinds.includes("episode")?'checked':''}>Episodes</label><label><input id="lc-all" type="checkbox" ${recipe.match_all_in_scope?'checked':''}>All titles in scope</label></div><p class="hint">Use all titles in scope for a channel without subject matching. Fine-tune the selection below.</p><h2 class="section">Libraries</h2><div class="lc-checks">${libs}</div><details><summary>Advanced metadata filters</summary><div class="formgrid"><div><label>Genres, comma separated</label><input id="lc-genres" value="${esc(recipe.genres_any.join(', '))}"></div><div><label>Tags, comma separated</label><input id="lc-tags" value="${esc(recipe.tags_any.join(', '))}"></div><div class="wide"><label>Subject keywords or phrases, comma separated</label><input id="lc-keywords" placeholder="stand-up, standup, stand up comedy" value="${esc(recipe.keywords_any.join(', '))}"><div class="hint">Matches text in title or overview, including series metadata. It does not infer a subject from meaning.</div></div><div><label>Year from</label><input id="lc-year-min" type="number" value="${recipe.year_min||''}"></div><div><label>Year through</label><input id="lc-year-max" type="number" value="${recipe.year_max||''}"></div></div></details><h2 class="section">Explicit titles and exclusions</h2><form class="lc-search" onsubmit="event.preventDefault();lcSearchTitles()"><input id="lc-search" type="search" aria-label="Search movies, series, episodes" placeholder="Search movies, series, episodes" value="${esc(draft.searchQuery||'')}" oninput="LIBRARY_CHANNELS.draft.searchQuery=this.value"><button type="submit">Search</button></form><div>${(LIBRARY_CHANNELS.search||[]).map(item=>`<span><button class="ghost sm" onclick="lcAddTitle('${esc(exactWireId(item))}','${esc(item.kind)}')">+ ${esc(item.title)}</button><button class="ghost sm" onclick="lcExcludeTitle('${esc(exactWireId(item))}','${esc(item.kind)}')">Exclude</button></span>`).join(' ')}</div>${draft.searchMessage?`<p class="hint" role="status">${esc(draft.searchMessage)}</p>`:''}${lcSelectedTitles(recipe)}`;
  else if(step==="playback")fields=`<div class="formgrid"><div><label>Order</label><select id="lc-order"><option value="balanced_shuffle" ${recipe.ordering==='balanced_shuffle'?'selected':''}>Balanced shuffle</option><option value="release_order" ${recipe.ordering==='release_order'?'selected':''}>Release year; keep episodes in order</option></select></div></div><div class="lc-checks"><label><input id="lc-specials" type="checkbox" ${recipe.include_specials?'checked':''}>Include specials</label><label><input id="lc-refresh" type="checkbox" ${recipe.auto_refresh?'checked':''}>Refresh automatically</label></div><div class="hint">Normal changes begin at the next full rotation. “Apply after this programme” is available after saving.</div>`;
  else fields=`<div class="formgrid"><div><label>Name (display only)</label><input id="lc-name" maxlength="80" value="${esc(draft.name)}"></div><div><label>Visibility</label><select id="lc-visibility"><option value="personal">Personal</option>${ME&&ME.is_admin?'<option value="shared" '+(draft.visibility==='shared'?'selected':'')+'>Shared</option>':''}</select></div><div class="wide"><label>Description (display only)</label><textarea id="lc-description" maxlength="500">${esc(draft.description)}</textarea></div></div><label><input id="lc-enabled" type="checkbox" ${draft.enabled?'checked':''}> Enable channel</label><div class="hint">Enablement is your explicit choice. Settings → Live TV shows advisory Store, media, and client readiness without blocking it.</div>`;
  const preview=draft.preview;
  const subject=draft.subjectRecipe===JSON.stringify(recipe)&&draft.subjectPreview;
  const subjectHtml=subject?`${lcSubjectStatus(subject)}${draft.id?'<p class="hint">The existing schedule keeps playing until a replacement is available.</p>':''}<div class="lc-actions"><button onclick="lcSubjectFilter('match')">Matched</button><button onclick="lcSubjectFilter('uncertain')">Uncertain</button><button onclick="lcSubjectFilter('no_match')">Excluded</button>${draft.subjectSaved?"":'<button onclick="lcCancelSubject()">Cancel preview</button>'}</div>${(subject.rows||[]).map(row=>`<div class="lc-programme lc-match"><span>${esc(row.title)}<small>${esc(row.reason)}</small></span><div class="lc-actions"><button class="ghost sm" onclick="showClassification('${esc(row.item_id)}')">Labels</button><button onclick="lcAddTitle('${esc(row.item_id)}','movie')">Include</button><button onclick="lcExcludeTitle('${esc(row.item_id)}','movie')">Exclude</button></div></div>`).join('')}${subject.next_cursor?'<button onclick="lcSubjectPage()">More results</button>':''}`:null;
  const previewHtml=draft.previewBusy?`<div class="empty" role="status">Updating your preview…</div>`:subjectHtml||(preview?`<h2>${preview.eligible_count} eligible title${preview.eligible_count===1?'':'s'}</h2><p>${esc(preview.repeat_description)}</p>${preview.excluded_count?`<p class="hint">${preview.excluded_count} candidates excluded by eligibility or recipe rules.</p>`:""}<h3>Matching titles</h3><p class="hint">A sample of the selection, before playback ordering.</p>${(preview.matches||[]).slice(0,10).map(match=>`<div class="lc-programme"><span>${esc(match.candidate.title)}</span><small>${esc((match.reasons||[]).join(' · '))}</small></div>`).join('')}`:`<div class="empty">${esc(draft.previewError||'Choose a subject or scope, then update the preview to see matching titles and time before repeats.')}</div>`);
  const errors=lcEditorErrors(draft),errorHtml=Object.entries(errors).map(([field,message])=>`<div class="err">${esc(field)}: ${esc(message)}</div>`).join('');
  return `${setHead(draft.id?"Edit channel":"Create channel","Choose content, set playback, then name your channel. Your draft stays on this browser for this account.",'<button class="ghost" onclick="closeLibraryChannelEditor()">Close</button>')}<div class="lc-editor"><section class="card"><div class="lc-steps">${steps}</div>${fields}${errorHtml}<div class="lc-actions lc-editor-footer"><button class="ghost" onclick="previewLibraryChannel()">Update preview</button>${step!=="content"?`<button class="ghost" onclick="lcStep('${LibraryChannelCore.nextStep(step,-1)}')">Back</button>`:''}${step!=="channel"?`<button onclick="lcStep('${LibraryChannelCore.nextStep(step,1)}')">Next: ${step==='content'?'playback':'name and create'} →</button>`:`<button class="primary" onclick="saveLibraryChannel()">${draft.id?'Save channel':'Create channel'}</button>`}${draft.id?'<button class="ghost" onclick="deleteLibraryChannel()">Delete</button>':''}</div><div class="err" id="lc-error"></div></section><aside class="card lc-preview"><span class="pill">PREVIEW</span>${previewHtml}</aside></div>`;
}
async function saveLibraryChannel(){
  lcReadEditor();const draft=LIBRARY_CHANNELS.draft;draft.showValidation=true;const fields=LibraryChannelCore.validateDraft(draft);
  if(Object.keys(fields).length){libraryChannelsPaint();return;}
  const payload=JSON.stringify({name:draft.name,description:draft.description,visibility:draft.visibility,enabled:draft.enabled,recipe:draft.recipe,revision:draft.expected_revision});
  if(draft.savePayload!==payload){draft.request_id=newRequestId();draft.savePayload=payload;draft.saveBody=null;}
  const body=draft.saveBody||{request_id:draft.request_id,name:draft.name.trim(),description:draft.description.trim(),visibility:draft.visibility,enabled:draft.enabled,recipe:draft.recipe,preview_seed:draft.subjectPreview?.preview_seed||draft.preview&&draft.preview.preview_seed||null};
  draft.saveBody=JSON.parse(JSON.stringify(body));saveLibraryChannelDraft();
  try{
    if(draft.id)await api(`/library-channels/${encodeURIComponent(draft.id)}`,{method:"PUT",body:Object.assign({expected_revision:draft.expected_revision},body)});
    else await api("/library-channels",{method:"POST",body});
    clearLibraryChannelDraft();LIBRARY_CHANNELS.editor=false;LIBRARY_CHANNELS.draft=null;toast(draft.id?"Channel saved":"Channel created");await viewLibraryChannels(PAGE_RENDER_GENERATION);
  }catch(error){const node=document.getElementById("lc-error");if(node)node.textContent=error.message;}
}
async function deleteLibraryChannel(){
  const draft=LIBRARY_CHANNELS.draft;if(!draft||!draft.id||!confirm(`Delete ${draft.name}? Media will not be deleted.`))return;
  try{await api(`/library-channels/${encodeURIComponent(draft.id)}?expected_revision=${draft.expected_revision}&request_id=${encodeURIComponent(newRequestId())}`,{method:"DELETE"});clearLibraryChannelDraft();LIBRARY_CHANNELS.editor=false;LIBRARY_CHANNELS.draft=null;await viewLibraryChannels(PAGE_RENDER_GENERATION);}
  catch(error){toast(error.message);}
}
async function toggleLibraryChannelFavourite(id,favourite){
  try{await api(`/library-channels/${encodeURIComponent(id)}/favourite`,{method:"PUT",body:{favourite}});await refreshLibraryChannels(PAGE_RENDER_GENERATION);}
  catch(error){toast(error.message);}
}
async function rebuildLibraryChannel(id,expectedRevision,activation,reshuffle){
  try{
    const result=await api(`/library-channels/${encodeURIComponent(id)}/rebuild`,{method:"POST",body:{expected_revision:expectedRevision,request_id:newRequestId(),activation,reshuffle}});
    toast(result.pending_activation_ms?`Changes begin ${new Date(result.pending_activation_ms).toLocaleString()}`:"Schedule rebuilt");
    await refreshLibraryChannels(PAGE_RENDER_GENERATION);
  }catch(error){toast(error.message);}
}

async function libraryChannelTune(channelId){
  const intent=LIBRARY_CHANNEL_TUNE.begin(channelId),sent=performance.now();
  LIBRARY_CHANNEL_TRANSITIONS++;
  try{
    const resolved=await api(`/library-channels/${encodeURIComponent(channelId)}/resolve`,{method:"POST",signal:intent.signal});
    const received=performance.now();LIBRARY_CHANNEL_CLOCK.observe(resolved.server_now_ms,sent,received);
    if(!LIBRARY_CHANNEL_TUNE.current(intent))return;
    const current=PLAYER&&PLAYER.libraryChannel;
    if(current){PLAYER.libraryChannelReplacing=true;closePlayer();}
    else if(document.getElementById("modal")?.classList.contains("open"))closePlayer();
    const channel=LIBRARY_CHANNELS.channels.find(row=>row.id===channelId)||{};
    const programme=(LIBRARY_CHANNELS.guide||[]).find(row=>row.channel_id===channelId&&row.generation_id===resolved.generation_id&&row.ordinal===resolved.occurrence.ordinal)||{};
    const tune={channel_id:channelId,generation_id:resolved.generation_id,occurrence:resolved.occurrence,
      tune_sequence:intent.sequence,starts_at_ms:resolved.starts_at_ms,ends_at_ms:resolved.ends_at_ms,
      item_id:resolved.item_id,file_id:resolved.file_id,title:programme.title||channel.name||"Library channel"};
    LIBRARY_CHANNEL_RETURN={channelId};PENDING_LIBRARY_CHANNEL_PLAYBACK=tune;
    ITEM_FOR_FILE[resolved.file_id]=resolved.item_id;
    await play(resolved.file_id,tune.title,resolved.position_ms,resolved.ends_at_ms-resolved.starts_at_ms,
      {title:tune.title,overview:`Following ${channel.name||'Library channel'}. Channel viewing doesn't change your watch history.`,runtime_ms:resolved.ends_at_ms-resolved.starts_at_ms});
    if(!LIBRARY_CHANNEL_TUNE.current(intent)){
      if(PLAYER&&PLAYER.libraryChannel&&PLAYER.libraryChannel.tune_sequence===intent.sequence)closePlayer();
      return;
    }
    setTimeout(()=>libraryChannelReconcile(intent).catch(()=>{}),30000);
  }catch(error){if(error.name!=="AbortError"){const view=LibraryChannelCore.errorView(error);toast(`${view.title}: ${view.detail}`);}}
  finally{LIBRARY_CHANNEL_TRANSITIONS=Math.max(0,LIBRARY_CHANNEL_TRANSITIONS-1);}
}
function libraryChannelTick(){
  if(!PLAYER||!PLAYER.libraryChannel||LIBRARY_CHANNEL_TRANSITIONS||document.getElementById("video")?.paused)return;
  if(LIBRARY_CHANNEL_CLOCK.now()>=Number(PLAYER.libraryChannel.ends_at_ms||0))libraryChannelBoundary().catch(()=>{});
}
async function libraryChannelBoundary(){
  const context=PLAYER&&PLAYER.libraryChannel;if(!context)return;
  const remaining=Number(context.ends_at_ms||0)-LIBRARY_CHANNEL_CLOCK.now();
  if(remaining>0){clearTimeout(LIBRARY_CHANNEL_BOUNDARY_TIMER);LIBRARY_CHANNEL_BOUNDARY_TIMER=setTimeout(()=>libraryChannelBoundary().catch(()=>{}),remaining);return;}
  if(!document.getElementById("video")?.paused)await libraryChannelTune(context.channel_id);
}
async function libraryChannelReconcile(intent){
  if(!LIBRARY_CHANNEL_TUNE.current(intent)||!PLAYER||!PLAYER.libraryChannel||document.getElementById("video")?.paused)return;
  const context=PLAYER.libraryChannel,sent=performance.now();
  const fresh=await api(`/library-channels/${encodeURIComponent(context.channel_id)}/resolve`,{method:"POST"});
  LIBRARY_CHANNEL_CLOCK.observe(fresh.server_now_ms,sent,performance.now());
  if(!LIBRARY_CHANNEL_TUNE.current(intent)||!PLAYER||!PLAYER.libraryChannel)return;
  if(fresh.generation_id!==context.generation_id||fresh.occurrence.cycle!==context.occurrence.cycle||fresh.occurrence.ordinal!==context.occurrence.ordinal){
    await libraryChannelTune(context.channel_id);return;
  }
  context.ends_at_ms=fresh.ends_at_ms;setTimeout(()=>libraryChannelReconcile(intent).catch(()=>{}),30000);
}
function libraryChannelWatchProgramme(channelId,fileId,itemId,titleOverride){
  const scheduled=(LIBRARY_CHANNELS.guide||[]).find(row=>row.channel_id===channelId&&Number(row.file_id)===Number(fileId));
  const title=titleOverride||(scheduled&&scheduled.title)||(LIBRARY_CHANNELS.channels.find(row=>row.id===channelId)||{}).name||"Library channel";
  LIBRARY_CHANNEL_RETURN={channelId};ITEM_FOR_FILE[fileId]=itemId;
  if(PLAYER&&PLAYER.libraryChannel){PLAYER.libraryChannelReplacing=true;closePlayer();}
  return play(fileId,title,0,0,{title,overview:"Personal playback from a Library channel. Return to channel rejoins what is on now.",return_channel_id:channelId});
}
function libraryChannelWatchFromStart(){
  const context=PLAYER&&PLAYER.libraryChannel;if(!context)return;
  return libraryChannelWatchProgramme(context.channel_id,PLAYER.fileId,context.item_id,PLAYER.title);
}
function returnToLibraryChannel(){
  const channelId=(PLAYER&&PLAYER.meta&&PLAYER.meta.return_channel_id)||(LIBRARY_CHANNEL_RETURN&&LIBRARY_CHANNEL_RETURN.channelId);
  if(channelId)return libraryChannelTune(channelId);
}

