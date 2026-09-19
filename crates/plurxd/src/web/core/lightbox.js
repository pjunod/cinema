"use strict";
// ---- photo lightbox -------------------------------------------------------
// Full-screen original, ←/→ through the photos of whatever grid you opened it
// from (videos are skipped — they belong to the player, not here), Esc closes,
// swipe on touch. Neighbours are preloaded so paging doesn't flash.
let PHOTO_SET=[], LB_AT=-1;
function photoUrl(id,size){ return tok(`/api/v1/items/${id}/photo${size?`?size=${size}`:""}`); }
function openLightbox(id){
  if(!PHOTO_SET.some(p=>p.id===id)) PHOTO_SET=[{id,title:"",recorded_at:null}];
  LB_AT=PHOTO_SET.findIndex(p=>p.id===id);
  let box=document.getElementById("lightbox");
  if(!box){
    box=document.createElement("div");
    box.id="lightbox"; box.className="lightbox";
    // It behaves as a modal — it takes the keyboard, locks the page behind it
    // and closes on Escape — so it says it is one. Without the role a screen
    // reader announces a photo appearing in the middle of the grid.
    box.setAttribute("role","dialog");
    box.setAttribute("aria-modal","true");
    box.setAttribute("aria-label","Photo");
    box.tabIndex=-1;
    box.innerHTML=`<img id="lbimg" alt="">
      <button class="lbx" aria-label="Close (Esc)" title="Close (Esc)" onclick="closeLightbox()">✕</button>
      <button class="lbprev" aria-label="Previous photo" onclick="lbStep(-1)">‹</button>
      <button class="lbnext" aria-label="Next photo" onclick="lbStep(1)">›</button>
      <div class="lbcap" id="lbcap"></div>`;
    box.addEventListener("click",e=>{ if(e.target===box) closeLightbox(); });
    box.addEventListener("touchstart",e=>{ LB_TOUCH=e.changedTouches[0].clientX; },{passive:true});
    box.addEventListener("touchend",e=>{
      const dx=e.changedTouches[0].clientX-LB_TOUCH;
      if(Math.abs(dx)>50) lbStep(dx<0?1:-1);
    },{passive:true});
    document.body.appendChild(box);
  }
  document.body.style.overflow="hidden";
  // Remember what opened it; a dialog that returns focus to the body drops a
  // keyboard viewer at the top of the page.
  LB_OPENER=document.activeElement;
  paintLightbox();
  box.focus({preventScroll:true});
}
let LB_TOUCH=0;
let LB_OPENER=null;
function paintLightbox(){
  const p=PHOTO_SET[LB_AT]; if(!p) return closeLightbox();
  const img=document.getElementById("lbimg"); if(img) img.src=photoUrl(p.id);
  const cap=document.getElementById("lbcap");
  const many=PHOTO_SET.length>1? ` · ${LB_AT+1} of ${PHOTO_SET.length}`:"";
  if(cap) cap.innerHTML=`<b>${esc(p.title||"")}</b> ${esc(fmtDate(p.recorded_at))}${esc(many)}`;
  for(const n of [LB_AT-1,LB_AT+1]){
    const q=PHOTO_SET[n]; if(q){ const pre=new Image(); pre.src=photoUrl(q.id); }
  }
  const one=PHOTO_SET.length<2;
  for(const c of ["lbprev","lbnext"]){
    const b=document.querySelector(`.lightbox .${c}`); if(b) b.style.display=one?"none":"";
  }
}
function lbStep(d){
  if(!PHOTO_SET.length) return;
  LB_AT=(LB_AT+d+PHOTO_SET.length)%PHOTO_SET.length;
  paintLightbox();
}
function closeLightbox(){
  const box=document.getElementById("lightbox"); if(box) box.remove();
  document.body.style.overflow="";
  LB_AT=-1;
  const opener=LB_OPENER; LB_OPENER=null;
  if(opener&&opener.focus&&document.contains(opener)) opener.focus({preventScroll:true});
}
function lightboxOpen(){ return !!document.getElementById("lightbox"); }

