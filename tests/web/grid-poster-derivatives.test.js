"use strict";
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {test} = require('node:test');

test('c03_grid_poster_uses_only_advertised_bucket_and_preserves_original_routes', () => {
  const root=path.resolve(__dirname,'../../crates/plurxd/src/web');
  const measurements=fs.readFileSync(path.join(root,'player/measurements.js'),'utf8');
  const api=fs.readFileSync(path.join(root,'core/api.js'),'utf8');
  const cards=fs.readFileSync(path.join(root,'core/cards.js'),'utf8');
  const tok=api.split('\n').find(line=>line.startsWith('const tok ='));
  assert.ok(tok,'exercise the actual token URL appender');
  const esc=measurements.slice(measurements.indexOf('function esc('),measurements.indexOf('// Artwork with'));
  const artwork=measurements.slice(measurements.indexOf('function gridPosterSource('));
  const context=vm.createContext({URLSearchParams});
  vm.runInContext(`const TOKEN='a&b'; ${tok}\n${esc}\n${artwork}\n${cards}\nconst exactWireId=it=>it.id;`,context);
  const render=(item,grid=true)=>{
    context.item=item;
    return vm.runInContext(grid?'card(item)':'artHtml(item)',context);
  };
  const poster={id:'1',kind:'movie',title:'Poster',poster:'/api/v1/images/p.jpg?v=revision&x=1',poster_sizes:['w300','w500','w780']};
  assert.match(render(poster),/p\.jpg\?v=revision&amp;x=1&amp;size=w300&amp;token=a%26b/);
  assert.doesNotMatch(render(poster,false),/size=w300/);
  for(const sizes of [undefined,[],['w500'],['w300','unknown'],'w300']) {
    assert.doesNotMatch(render({...poster,poster_sizes:sizes}),/size=w300/);
  }
  for(const source of ['/api/v1/images/p.jpg?size=original&v=1','/api/v1/images/p.jpg?%73ize=w780','https://other.invalid/p.jpg','/api/v1/items/1/photo?size=thumb']) {
    assert.doesNotMatch(render({...poster,poster:source}),/size=w300/);
  }
  assert.match(render({...poster,poster:'/api/v1/images/p.jpg'}),/p\.jpg\?size=w300&amp;token=a%26b/);
  assert.match(render({...poster,kind:'folder'}),/size=w300/);
  assert.doesNotMatch(render({...poster,kind:'photo'}),/size=w300/);
  assert.match(render({id:'2',kind:'photo',title:'Photo'}),/\/items\/2\/photo\?size=thumb&amp;token=a%26b/);
  assert.doesNotMatch(render({...poster,poster:null,backdrop:'/api/v1/images/back.jpg?v=2'}),/size=w300/);
  assert.match(render({id:'3',kind:'movie',title:'No artwork'}),/>NA<\/div>/);
});
