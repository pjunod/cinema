"use strict";
const PlaybackPolicy = window.PlurxPlaybackPolicy;
const ReaderCore = window.PlurxReaderCore;
const LibraryChannelCore = window.PlurxLibraryChannels;
const API = "/api/v1";
// Native phone/tablet shells load this trusted page at the server origin, then
// hand their bearer directly into this JavaScript realm. It never enters a
// URL, localStorage, or the authored publication frame. The query flag carries
// no authority; it only keeps the ordinary sign-in screen from flashing while
// the native WebView prepares the handoff.
const NATIVE_READER_BOOT = new URLSearchParams(location.search).get("native-reader")==="1";
let TOKEN = NATIVE_READER_BOOT ? null : (localStorage.getItem("plurx_token") || null);
let AUTH_GENERATION=0;
let ME = null;
let AUTH_NOTICE="";
let SIGN_OUT_RUN=null;

function nativeReaderPost(event,message=""){
  if(!NATIVE_READER_BOOT) return false;
  const payload={event,message:String(message||"")};
  try{
    const apple=window.webkit&&window.webkit.messageHandlers&&window.webkit.messageHandlers.cinemaReader;
    if(apple&&typeof apple.postMessage==="function"){ apple.postMessage(payload); return true; }
    if(window.CinemaNative&&typeof window.CinemaNative.postMessage==="function"){
      window.CinemaNative.postMessage(JSON.stringify(payload)); return true;
    }
  }catch(e){}
  return false;
}

async function startNativeReader(token,itemId,fileId){
  const item=String(itemId), file=String(fileId);
  if(!NATIVE_READER_BOOT||typeof token!=="string"||!token||!(/^[1-9]\d*$/).test(item)||!(/^[1-9]\d*$/).test(file)){
    nativeReaderPost("error","Cinema could not validate this reader handoff."); return false;
  }
  TOKEN=token; AUTH_GENERATION++; ME=null;
  refreshClientErrorReporterAuth();
  history.replaceState(null,"",`${location.pathname}?native-reader=1#/read/${item}/${file}`);
  await boot();
  return true;
}
window.startNativeReader=startNativeReader;
