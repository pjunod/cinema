"use strict";

// Owns only diagnostic data. A denied database leaves capture/export in memory.
const CinemaDiagnosticStore = (() => {
  let database = null, available = false;
  const ready = new Promise(resolve => {
    try {
      if (!window.indexedDB) { resolve(null); return; }
      const request = window.indexedDB.open('cinema-diagnostics-v1', 1);
      request.onupgradeneeded = () => request.result.createObjectStore('records', { keyPath: 'key' });
      request.onsuccess = () => { database = request.result; available = true; database.onversionchange = () => { available = false; database.close(); }; resolve(database); };
      request.onerror = request.onblocked = () => resolve(null);
    } catch (_) { resolve(null); }
  });
  async function operation(mode, action) {
    const db = await ready; if (!db || !available) return null;
    return new Promise(resolve => {
      try {
        const transaction = db.transaction('records', mode), store = transaction.objectStore('records'); let value = null;
        action(store, result => { value = result; });
        transaction.oncomplete = () => resolve(value);
        transaction.onerror = transaction.onabort = () => { available = false; resolve(null); };
      } catch (_) { available = false; resolve(null); }
    });
  }
  async function get(key) { return operation('readonly', (store, done) => { const request = store.get(key); request.onsuccess = () => done(request.result || null); }); }
  async function put(record) {
    if(!record || typeof record.key!=='string' || record.key.length>160)return null;
    let actual;try{actual=new TextEncoder().encode(JSON.stringify(record)).length+(record.delivery?.archive instanceof Blob?record.delivery.archive.size:0);}catch(_){return null;}
    if(actual>8388608 || !Number.isFinite(record.expires))return null;record={...record,bytes:actual};
    return operation('readwrite', (store, done) => {
      store.put(record);
      // Walk rows one at a time; retain only metadata, never every archive.
      const rows=[],now=Date.now(),request=store.openCursor();let scanned=0;
      request.onsuccess=()=>{
        const cursor=request.result;
        if(cursor){
          if(++scanned>128){store.transaction.abort();return;}
          const row=cursor.value;let size;
          try{size=new TextEncoder().encode(JSON.stringify(row)).length+(row.delivery?.archive instanceof Blob?row.delivery.archive.size:0);}catch(_){cursor.delete();cursor.continue();return;}
          if(size>8388608||!Number.isFinite(row.expires)||row.expires<=now)cursor.delete();
          else rows.push({key:row.key,at:row.at||0,type:row.type,bytes:size});
          cursor.continue();return;
        }
        rows.sort((a,b)=>b.at-a.at);let bytes=0,captures=0;
        for(let index=0;index<rows.length;index++){
          const row=rows[index];bytes+=row.bytes;if(row.type==='capture')captures++;
          if(index>=32||bytes>33554432||captures>10)store.delete(row.key);
        }
        done(true);
      };
    });
  }
  async function remove(key) { return operation('readwrite', (store, done) => { store.delete(key); done(true); }); }
  async function clear() { return operation('readwrite', (store, done) => { store.clear(); done(true); }); }
  return Object.freeze({ get, put, remove, clear, persistent: () => available });
})();
