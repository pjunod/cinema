// SPDX-License-Identifier: Apache-2.0
const state=document.getElementById("status"),toggle=document.getElementById("enabled");
function paint(value){if(value.error){state.textContent=value.error;return;}toggle.checked=value.enabled;state.textContent=value.state+(value.origin?" · "+value.origin:"")+(value.last_key?" · Last key: "+value.last_key:"")+(value.last_outcome?" · "+value.last_outcome:"");}
toggle.addEventListener("change",()=>chrome.runtime.sendMessage({type:"enable",enabled:toggle.checked}).then(paint));
document.getElementById("unbind").addEventListener("click",()=>chrome.runtime.sendMessage({type:"unbind"}).then(paint));
document.getElementById("bind").addEventListener("click",async()=>{
  try{
    const [tab]=await chrome.tabs.query({active:true,currentWindow:true}),origin=new URL(tab.url).origin;
    if(!/^https?:\/\//.test(origin))throw new Error("Open Cinema first.");
    if(!await chrome.permissions.request({origins:[origin+"/*"]}))throw new Error("Origin permission was declined.");
    const old=await chrome.storage.local.get("origin");
    if(old.origin&&old.origin!==origin)await chrome.permissions.remove({origins:[old.origin+"/*"]});
    // Closing the action popup returns actual document focus to Cinema.
    // The worker keeps setup alive; reopen this popup to inspect its result.
    chrome.runtime.sendMessage({type:"bind"}).catch(()=>{});
    window.close();
  }catch(error){state.textContent=error.message;}
});
chrome.runtime.sendMessage({type:"status"}).then(paint);
