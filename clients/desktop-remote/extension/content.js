"use strict";
// SPDX-License-Identifier: Apache-2.0
// ISOLATED world: only a heartbeat/lifetime port, never page commands.
(()=>{
  if(globalThis.cinemaDesktopPort)return;
  const port=chrome.runtime.connect({name:"cinema-cec-document"});globalThis.cinemaDesktopPort=port;
  const timer=setInterval(()=>{try{port.postMessage({type:"tick"});}catch(_){clearInterval(timer);}},250);
  port.onDisconnect.addListener(()=>{clearInterval(timer);delete globalThis.cinemaDesktopPort;});
  window.addEventListener("pagehide",()=>{clearInterval(timer);port.disconnect();},{once:true});
  document.addEventListener("visibilitychange",()=>{if(document.visibilityState!=="visible")port.postMessage({type:"hidden"});});
})();
