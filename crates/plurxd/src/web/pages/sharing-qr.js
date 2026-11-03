"use strict";

// Invitations never leave this browser for QR encoding or image decoding.
// A scan fills a revision-fenced draft; the owner still explicitly imports it.
let SHARING_QR_JOB=null;
function sharingQRStop(){
  const job=SHARING_QR_JOB;SHARING_QR_JOB=null;
  if(!job)return;
  if(job.timer)clearTimeout(job.timer);
  job.stream?.getTracks().forEach(track=>track.stop());
  job.dialog?.remove();
}
function sharingQRBegin(){
  sharingQRStop();
  const s=SHARING_MANAGEMENT,e=s?.editor;
  if(!s||!e||!["import","repair"].includes(e.mode)||e.busy||e.saved||!sharingCurrent(s.capture))return null;
  const job={state:s,editor:e,revision:e.revision};SHARING_QR_JOB=job;return job;
}
function sharingQRCurrent(job){
  return SHARING_QR_JOB===job&&SHARING_MANAGEMENT===job.state&&job.state.editor===job.editor&&job.editor.revision===job.revision&&!job.editor.busy&&!job.editor.saved&&sharingCurrent(job.state.capture);
}
function sharingQRFinish(job,text,error=""){
  if(!sharingQRCurrent(job))return;
  const e=job.editor;
  if(text!==null){
    try{sharingEdit("text",sharingInvitation(text));e.qrNotice="Invitation read. Choose Import invitation, then follow the connection and pairing steps.";e.error="";}
    catch{e.error="This QR code is not a Cinema sharing invitation.";}
  }else if(error)e.error=error;
  sharingQRStop();sharingPaint();
}
function sharingQRMatrix(text){
  sharingInvitation(text);
  const bytes=new TextEncoder().encode(text);
  if(bytes.length>2331)throw new Error("This invitation is too large for one QR code. Expand Invitation text and copy it instead.");
  return qrcodegen.QrCode.encodeBinary(Array.from(bytes),qrcodegen.QrCode.Ecc.MEDIUM);
}
function sharingQRCanvas(text){
  const qr=sharingQRMatrix(text),scale=Math.max(4,Math.floor(1024/(qr.size+8))),canvas=document.createElement("canvas");
  canvas.width=canvas.height=(qr.size+8)*scale;
  const ctx=canvas.getContext("2d");if(!ctx)throw new Error("QR images are unavailable in this browser. Expand Invitation text to copy it.");
  ctx.fillStyle="#fff";ctx.fillRect(0,0,canvas.width,canvas.height);ctx.fillStyle="#000";
  for(let y=0;y<qr.size;y++)for(let x=0;x<qr.size;x++)if(qr.getModule(x,y))ctx.fillRect((x+4)*scale,(y+4)*scale,scale,scale);
  return canvas;
}
function sharingQRImage(invitation){
  if(!invitation.qrImage&&!invitation.qrError){
    try{invitation.qrImage=sharingQRCanvas(invitation.invitation).toDataURL("image/png");}
    catch(error){invitation.qrError=error.message;}
  }
  return invitation.qrImage;
}
function sharingQRResultHTML(s){
  const invitation=s.invitation,image=sharingQRImage(invitation);
  return `<section id="sharing-invitation-result" class="sharing-invitation" tabindex="-1"><h4>Invitation created</h4><p role="status">Your invitation is ready.</p><p>On the other Cinema, open Settings → Sharing → Import invitation and scan or upload this QR code. Save or share the image to send it to the other owner.</p>${image?`<img class="sharing-qr-image" src="${image}" alt="Cinema sharing invitation QR code"><p>${sharingButton("Save QR code","sharingQRExport(false)",s.busy)} ${typeof navigator!=="undefined"&&navigator.share?sharingButton("Share QR code","sharingQRExport(true)",s.busy):""}</p>`:`<p role="status">${esc(invitation.qrError)}</p>`}<p>This QR code contains a secret invitation. Share it only with the intended Cinema owner. The secret is cleared here when you leave Sharing; saved copies remain on your device.</p><details id="sharing-invitation-details"${!image||s.invitationTextOpen?" open":""}><summary>Invitation text</summary><textarea id="sharing-invitation-text" aria-label="Invitation to copy" readonly autocomplete="off" spellcheck="false" onfocus="this.select()">${esc(invitation.invitation)}</textarea>${sharingButton("Copy invitation","sharingCopyInvitation()",s.busy)}</details><p role="status" aria-live="polite">${esc(s.invitationCopyNotice||"")}</p>${sharingButton("Cancel invitation","sharingCancelInvitation()",s.busy)} ${sharingButton("Clear secret","sharingClearInvitation()",s.busy)}</section>`;
}
async function sharingQRExport(share){
  const s=SHARING_MANAGEMENT,invitation=s?.invitation;if(!invitation||s.busy||!sharingCurrent(s.capture))return;
  try{
    const image=sharingQRImage(invitation);if(!image)throw new Error(invitation.qrError);
    if(share){
      const bytes=Uint8Array.from(atob(image.split(",")[1]),c=>c.charCodeAt(0)),file=new File([bytes],"cinema-sharing-invitation.png",{type:"image/png"});
      if(!navigator.canShare?.({files:[file]}))throw new Error("Image sharing is unavailable here. Use Save QR code, then send the saved image.");
      await navigator.share({files:[file],title:"Cinema sharing invitation"});
    }else{
      const link=document.createElement("a");link.href=image;link.download="cinema-sharing-invitation.png";document.body.appendChild(link);link.click();link.remove();
    }
  }catch(error){
    if(error.name==="AbortError"||SHARING_MANAGEMENT!==s||s.invitation!==invitation||!sharingCurrent(s.capture))return;
    s.invitationCopyNotice=error.message;sharingPaint();
  }
}
function sharingQRImportHTML(e){
  return `<p>Upload the invitation QR image from the other Cinema, or scan it with this device’s camera.</p><label class="sharing-qr-upload">Upload QR code<input type="file" accept="image/*" onchange="sharingQRUpload(this)"${e.busy||e.saved?" disabled":""}></label>${typeof navigator!=="undefined"&&navigator.mediaDevices?.getUserMedia?sharingButton("Scan QR code","sharingQRScan()",e.busy||e.saved):"<p>Camera scanning needs HTTPS and a supported camera. You can upload an image or paste the invitation text.</p>"}<p role="status" aria-live="polite">${esc(e.qrNotice||"")}</p><details${e.text?" open":""}><summary>Paste invitation text</summary>${sharingInput("Secret invitation","text",e.text)}</details>`;
}
function sharingQRDecode(image){
  const width=image.width||image.videoWidth,height=image.height||image.videoHeight;
  if(!width||!height)return null;
  const factor=Math.min(1,2048/Math.max(width,height)),canvas=document.createElement("canvas");
  canvas.width=Math.max(1,Math.round(width*factor));canvas.height=Math.max(1,Math.round(height*factor));
  const ctx=canvas.getContext("2d",{willReadFrequently:true});if(!ctx)throw new Error("Image decoding is unavailable. Paste the invitation text instead.");
  ctx.drawImage(image,0,0,canvas.width,canvas.height);
  const pixels=ctx.getImageData(0,0,canvas.width,canvas.height);
  return window.jsQR(pixels.data,pixels.width,pixels.height,{inversionAttempts:"attemptBoth"})?.data??null;
}
async function sharingQRUpload(input){
  const file=input.files?.[0];input.value="";if(!file)return;
  const job=sharingQRBegin();if(!job)return;
  let bitmap;
  try{
    if(!file.type.startsWith("image/")||file.size>10*1024*1024)throw new Error("Choose a QR image no larger than 10 MiB.");
    bitmap=await createImageBitmap(file);
    if(!sharingQRCurrent(job))return;
    if(bitmap.width*bitmap.height>20000000)throw new Error("Choose a QR image under 20 megapixels, or crop it to the QR code.");
    const text=sharingQRDecode(bitmap);
    sharingQRFinish(job,text,text===null?"No QR code found. Try a clearer image or paste the invitation text.":"");
  }catch(error){sharingQRFinish(job,null,error.message||"Could not read this image. Paste the invitation text instead.");}
  finally{bitmap?.close();if(SHARING_QR_JOB===job)sharingQRStop();}
}
async function sharingQRScan(){
  const job=sharingQRBegin();if(!job)return;
  try{
    const stream=await navigator.mediaDevices.getUserMedia({audio:false,video:{facingMode:{ideal:"environment"},width:{ideal:1280},height:{ideal:1280}}});
    if(!sharingQRCurrent(job)){stream.getTracks().forEach(track=>track.stop());return;}
    job.stream=stream;
    const dialog=document.createElement("dialog");job.dialog=dialog;dialog.className="sharing-qr-scanner";
    dialog.innerHTML='<h3>Scan invitation QR code</h3><p>Point the camera at the other Cinema’s invitation.</p><video autoplay playsinline muted aria-label="Invitation QR camera preview"></video><p role="status" aria-live="polite">Looking for a QR code…</p><button type="button">Cancel scan</button>';
    dialog.querySelector("button").onclick=sharingQRStop;dialog.addEventListener("cancel",sharingQRStop);document.body.appendChild(dialog);dialog.showModal();
    const video=dialog.querySelector("video");video.srcObject=stream;await video.play();
    const scan=()=>{
      if(!sharingQRCurrent(job)){if(SHARING_QR_JOB===job)sharingQRStop();return;}
      try{
        const text=sharingQRDecode(video);
        if(text!==null){sharingQRFinish(job,text);return;}
        job.timer=setTimeout(scan,300);
      }catch(error){sharingQRFinish(job,null,error.message);}
    };
    scan();
  }catch(error){sharingQRFinish(job,null,error.name==="NotAllowedError"?"Camera access was denied. Upload a QR image or paste the invitation text.":"Camera unavailable. Upload a QR image or paste the invitation text.");}
}
