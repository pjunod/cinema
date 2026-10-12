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
      // An indexed scan with a fixed row cap bounds storage before new captures.
      const count = store.count(); count.onsuccess = () => { if(count.result > 32) { store.clear(); store.put(record); } };
      const request = store.getAll(null, 17); request.onsuccess = () => {
        const rows = request.result || [], now = Date.now(); let bytes = 0, captures = 0;
        rows.sort((a, b) => (b.at || 0) - (a.at || 0));
        for (const row of rows) {
          const size = Number(row.bytes || 0);
          if (row.expires <= now || rows.indexOf(row) >= 16 || (row.type === 'capture' && (++captures > 10 || (bytes += size) > 33554432))) store.delete(row.key);
        }
        done(true);
      };
    });
  }
  async function remove(key) { return operation('readwrite', (store, done) => { store.delete(key); done(true); }); }
  async function clear() { return operation('readwrite', (store, done) => { store.clear(); done(true); }); }
  return Object.freeze({ get, put, remove, clear, persistent: () => available });
})();
