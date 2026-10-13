/* Compile the Android app first. JAVA_HOME selects the JDK; NODE_PATH may
 * select an installed Playwright. CINEMA_BROWSER_EXECUTABLE is optional. */
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const os=require('node:os');
const cp=require('node:child_process');
const {chromium}=require('playwright');

test('TV ZIP form downloads the exact native attachment', {timeout:20000}, async()=>{
  const root=path.resolve(__dirname,'../..');
  const classes=path.join(root,'clients/android/app/build/intermediates/built_in_kotlinc/debug/compileDebugKotlin/classes');
  const javaHome=process.env.JAVA_HOME;
  assert.ok(javaHome,'JAVA_HOME must select a JDK');
  const cache=path.join(process.env.GRADLE_USER_HOME||path.join(os.homedir(),'.gradle'),'caches/modules-2/files-2.1/org.jetbrains.kotlin/kotlin-stdlib/2.3.10');
  const jars=fs.readdirSync(cache).flatMap(hash=>fs.readdirSync(path.join(cache,hash)).filter(name=>name==='kotlin-stdlib-2.3.10.jar').map(name=>path.join(cache,hash,name)));
  assert.equal(jars.length,1,'The compiled app Kotlin standard library must be available');
  const folder=fs.mkdtempSync(path.join(os.tmpdir(),'cinema-tv-archive-browser-'));
  const classpath=[classes,jars[0],folder].join(path.delimiter);
  const source=`import java.net.*; import java.nio.file.*; import java.io.*; import java.util.zip.*; import tv.plurx.app.diagnostics.DiagnosticLocalExport;
public class TvArchiveBrowserFixture {
 public static void main(String[] args) throws Exception {
  var output=new ByteArrayOutputStream();
  try(var zip=new ZipOutputStream(output)) { for(var name:new String[]{"summary.txt","manifest.json","description.txt"}) { zip.putNextEntry(new ZipEntry(name)); zip.write(("TV archive browser qualification: "+name).getBytes()); zip.closeEntry(); } }
  var bytes=output.toByteArray(); Files.write(Path.of(args[0]),bytes);
  try(var export=new DiagnosticLocalExport(bytes,"00000000-0000-4000-8000-000000000001",InetAddress.getByName("127.0.0.1"),300000L,()->kotlin.Unit.INSTANCE)) { System.out.println(export.getUrl()); System.out.flush(); Thread.sleep(45000); }
 }
}`;
  let child,browser;
  try {
    fs.writeFileSync(path.join(folder,'TvArchiveBrowserFixture.java'),source);
    cp.execFileSync(path.join(javaHome,'bin/javac'),['-cp',classpath,path.join(folder,'TvArchiveBrowserFixture.java')]);
    child=cp.spawn(path.join(javaHome,'bin/java'),['-cp',classpath,'TvArchiveBrowserFixture',path.join(folder,'expected.zip')]);
    const url=await new Promise((resolve,reject)=>{
      child.stdout.once('data',data=>resolve(data.toString().trim()));
      child.once('error',reject);
      child.once('exit',()=>reject(new Error('Native fixture ended before publication')));
    });
    browser=await chromium.launch({headless:true,...(process.env.CINEMA_BROWSER_EXECUTABLE?{executablePath:process.env.CINEMA_BROWSER_EXECUTABLE}:{})});
    const context=await browser.newContext({acceptDownloads:true,viewport:{width:390,height:844}});
    const page=await context.newPage();
    await page.goto(url);
    await page.getByRole('heading',{name:'Save this TV’s diagnostic ZIP'}).waitFor();
    const [download]=await Promise.all([page.waitForEvent('download',{timeout:10000}),page.getByRole('button',{name:'Download diagnostic ZIP'}).click()]);
    assert.equal(download.suggestedFilename(),'cinema-report-00000000-0000-4000-8000-000000000001.zip');
    assert.deepEqual(fs.readFileSync(await download.path()),fs.readFileSync(path.join(folder,'expected.zip')));
    assert.equal(await download.failure(),null);
    await context.close();
  } finally {
    if(browser) await browser.close();
    if(child) { child.kill(); await new Promise(resolve=>child.exitCode!==null?resolve():child.once('exit',resolve)); }
    fs.rmSync(folder,{recursive:true,force:true});
  }
});
