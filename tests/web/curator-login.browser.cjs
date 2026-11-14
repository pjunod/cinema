"use strict";
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");
const consent="#/curator-consent?clientId=client&state="+"s".repeat(64)+"&challenge="+"c".repeat(43);
async function fixture(browser,signedIn,admin=false){
 const page=await browser.newPage({viewport:{width:390,height:844}}),errors=[],consents=[];
 page.on("pageerror",e=>errors.push(e.message));
 await page.addInitScript(({signedIn})=>{if(signedIn)localStorage.setItem("plurx_token","native-fixture");localStorage.setItem("plurx_layout","catalog")},{signedIn});
 await page.route("**/*",async route=>{
  const url=new URL(route.request().url()),p=url.pathname;
  if(url.hostname==="curator.example")return route.fulfill({contentType:"text/html",body:"<h1>Returned to Curator</h1>"});
  if(url.hostname!=="cinema.test")return route.abort();
  if(p==="/"||p.startsWith("/assets/")){const file=path.join(root,p==="/"?"index.html":p.slice(8));return fs.existsSync(file)?route.fulfill({contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",body:fs.readFileSync(file)}):route.fulfill({status:404,body:""})}
  const key=p.replace("/api/v1","");let data={};const user={id:1,username:"household-person",is_admin:admin};
  if(key==="/server")data={name:"Cinema",setup_required:false};
  else if(key==="/me")data=user;
  else if(key==="/auth/login")data={user,token:"native-fixture"};
  else if(key==="/features")data={movies:true,tv:true,books:true,music:true};
  else if(key==="/libraries"||key==="/dvr/reminders/due"||key==="/delegation/clients")data=[];
  else if(key==="/delegation/info")data={client:{id:"client",name:"My Curator",redirect:"https://curator.example/api/v1/auth/cinema/callback"},scope:"household-requests",issuer:"issuer"};
  else if(key==="/delegation/consent"){consents.push(route.request().postDataJSON());data={redirect:"https://curator.example/api/v1/auth/cinema/callback?code=one-use&state="+"s".repeat(64)+"&iss=issuer"}}
  return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
 });
 return {page,errors,consents};
}
for(const signedIn of [true,false])test(`cold consent deep link survives ${signedIn?"existing login":"local login"} and reload`,async()=>{
 const browser=await chromium.launch({headless:true,...(process.env.PW_CHROMIUM_PATH?{executablePath:process.env.PW_CHROMIUM_PATH}:{})});
 try{const{page,errors,consents}=await fixture(browser,signedIn);await page.goto("http://cinema.test/"+consent);
  if(!signedIn){await page.locator("#u").fill("household-person");await page.locator("#p").fill("local-password");await page.getByRole("button",{name:"Sign in",exact:true}).click()}
  await page.getByRole("heading",{name:"Connect Curator"}).waitFor();await page.reload();const allow=page.getByRole("button",{name:"Continue as household-person"});await allow.waitFor();
  const box=await allow.boundingBox();assert.ok(box&&box.y>=0&&box.y+box.height<=844,"visible consent action on phone");await allow.click();await page.getByRole("heading",{name:"Returned to Curator"}).waitFor();
  assert.equal(consents.length,1);assert.deepEqual(Object.keys(consents[0]).sort(),["challenge","clientId","state"]);assert.deepEqual(errors,[]);
 }finally{await browser.close()}
});
test("connections management mounts the normal shell on a cold page",async()=>{
 const browser=await chromium.launch({headless:true,...(process.env.PW_CHROMIUM_PATH?{executablePath:process.env.PW_CHROMIUM_PATH}:{})});
 try{const{page,errors}=await fixture(browser,true,true);await page.goto("http://cinema.test/#/curator-connections");await page.getByRole("heading",{name:"Curator login connections"}).waitFor();assert.equal(await page.locator("#main").count(),1);await page.reload();await page.getByRole("button",{name:"Register connection"}).waitFor();assert.deepEqual(errors,[])}finally{await browser.close()}
});
