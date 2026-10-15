"""Regenerate the proposal plates and local gallery. Standard library only."""
from pathlib import Path
from html import escape
import json

ROOT = Path(__file__).resolve().parent
OUT = ROOT / 'renderings'
OUT.mkdir(exist_ok=True)
BG='#0a0a0c'; SURFACE='#17171c'; INK='#ededf0'; MUTED='#aaaab5'; RED='#e5484d'; LINE='#35353e'
class Plate:
    def __init__(self,title):
        self.bits=[f'<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080" viewBox="0 0 1920 1080" role="img" aria-label="{escape(title)}"><title>{escape(title)}</title><defs><linearGradient id="art" x2="1" y2="1"><stop stop-color="#364754"/><stop offset="1" stop-color="#172027"/></linearGradient><linearGradient id="shade" x2="0" y2="1"><stop stop-color="#0a0a0c" stop-opacity="0"/><stop offset="1" stop-color="#0a0a0c"/></linearGradient></defs>']
        self.rect(0,0,1920,1080,BG,0)
    def rect(self,x,y,w,h,fill=SURFACE,r=16,stroke=None,sw=1):
        self.bits.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}"'+(f' stroke="{stroke}" stroke-width="{sw}"' if stroke else '')+'/>')
    def text(self,x,y,s,size=28,color=INK,weight=400):
        self.bits.append(f'<text x="{x}" y="{y}" font-family="Helvetica,Arial,sans-serif" font-size="{size}" fill="{color}" font-weight="{weight}">{escape(str(s))}</text>')
    def lines(self,x,y,lines,size=28,color=MUTED,gap=40):
        for n,s in enumerate(lines):self.text(x,y+n*gap,s,size,color)
    def button(self,x,y,w,label,focus=False,selected=False,h=66):
        self.rect(x,y,w,h,'#29292f' if focus else SURFACE,14,INK if focus else LINE,4 if focus else 1)
        self.text(x+24,y+h/2+10,label,27)
        if selected:self.text(x+w-43,y+h/2+10,'✓',28,RED,700)
    def header(self,active='Home'):
        self.text(80,90,'cinema',38,RED,700)
        x=370
        for label in ['Home','Libraries','Live TV','Recordings','Channels','Search','Settings']:
            self.text(x,83,label,26,INK if label==active else MUTED,700 if label==active else 400)
            width=len(label)*16+30
            if label==active:self.rect(x,101,width-28,4,RED,2)
            x+=width
    def footer(self,text='Back  ·  Previous screen'):
        self.text(80,1030,text,23,MUTED)
    def poster(self,x,y,w,h,title,meta='',focus=False,n=0):
        self.rect(x,y,w,h,'url(#art)',12)
        # Abstract placeholder artwork; no production poster or generated screenshot.
        self.rect(x+int(w*.18),y+int(h*.18),int(w*.64),int(h*.49),['#46545c','#314950','#4e424a','#514a3c'][n%4],int(w*.25))
        self.text(x+20,y+h-30,title,22,INK,700)
        if focus:self.rect(x-6,y-6,w+12,h+12,'none',16,INK,4)
        self.text(x,y+h+42,title,26,INK,700)
        if meta:self.text(x,y+h+77,meta,22,MUTED)
    def scene(self,x,y,w,h):
        self.rect(x,y,w,h,'#1b2931',16)
        self.bits.append(f'<path d="M{x},{y+h*.72} L{x+w*.24},{y+h*.32} L{x+w*.43},{y+h*.61} L{x+w*.68},{y+h*.17} L{x+w},{y+h*.71} V{y+h} H{x} Z" fill="#3b4d51"/>')
        self.bits.append(f'<path d="M{x},{y+h*.86} L{x+w*.32},{y+h*.57} L{x+w*.58},{y+h*.79} L{x+w*.85},{y+h*.48} L{x+w},{y+h*.69} V{y+h} H{x} Z" fill="#253b42"/>')
        self.rect(x,y,w,h,'url(#shade)',0)
    def save(self,key):
        (OUT/(key+'.svg')).write_text(''.join(self.bits)+'</svg>')

def library(focus_tools=True):
    p=Plate('Proposed movie library with reachable tools and retained position')
    p.text(80,92,'‹  Libraries',29,MUTED);p.text(80,170,'Movies',58,INK,700);p.text(350,165,'416 titles',27,MUTED)
    p.button(900,110,280,'View: Grid',selected=True);p.button(1205,110,280,'Sort: Title');p.button(1510,110,300,'Filter: All')
    p.button(310,220,1500,'Find a title in Movies')
    p.button(70,352,185,'Tools',focus=focus_tools);p.button(70,438,185,'Jump to…');p.text(80,572,'M',48,RED,700);p.lines(80,615,['201–212','of 416'],24)
    titles=['Moon Harbor','Northern Light','Open Water','Paper Cities','Quiet Earth','Red Horizon']
    for i,t in enumerate(titles):p.poster(310+i*252,340,224,336,t,'2024 · Movie',n=i)
    for i,t in enumerate(['Silver Road','The Crossing','The Orchard','Velvet Sky','Wild Coast','Winter Sun']):
        p.rect(310+i*252,830,224,336,'url(#art)',12)
    p.rect(0,995,1920,85,BG,0)
    p.footer('Left from the first movie  ·  Library tools       Right from Tools  ·  Return to Moon Harbor')
    return p

def modal():
    p=library(focus_tools=False);p.rect(0,0,1920,1080,'#000000aa',0);p.rect(610,190,700,700,'#1b1b21',24)
    p.text(670,280,'View',46,INK,700);p.text(670,328,'Choose how to browse Movies',26,MUTED)
    p.button(670,380,580,'Rows',h=88);p.button(670,490,580,'Grid',focus=True,selected=True,h=88)
    p.button(670,690,580,'Close',h=78);p.text(670,830,'Back cancels · Return to View',24,MUTED)
    return p

def detail():
    p=Plate('Proposed episode detail with About and episode options')
    p.scene(0,0,1920,1000);p.text(80,90,'‹  Northern Light  /  Season 2',28,MUTED)
    p.text(100,240,'EPISODE 06',23,RED,700);p.text(100,320,'A place to return',64,INK,700)
    p.text(100,380,'2024   ·   48 min   ·   4K   ·   Dolby Vision',28,MUTED)
    p.lines(100,455,['An unexpected arrival changes the course of the expedition.', 'The crew must decide what they are willing to leave behind.'],30,INK,44)
    p.button(100,590,380,'Resume · 18:24');p.button(510,590,260,'Start over')
    p.button(100,690,260,'About');p.button(390,690,300,'Episode options')
    p.rect(1130,175,690,745,'#19191f',24);p.text(1190,255,'Episode options',43,INK,700)
    for y,label,sel in [(320,'Audio',False),(424,'Subtitles',False),(528,'Version',False),(632,'Mark watched',False),(736,'Close',False)]:p.button(1190,y,570,label,focus=y==320,h=80)
    p.footer('Season shelf: Select plays · Episode options opens details and track choices before playback')
    return p

def home():
    p=Plate('Proposed Home with retained shelves and explicit recovery');p.header()
    p.text(80,220,'Pick up where you left off',53,INK,700)
    p.rect(80,265,1760,88,'#231b1e');p.text(110,320,'Home could not refresh. Your previous titles are still available.',27);p.button(1630,278,180,'Retry',focus=True,h=60)
    p.text(80,423,'Continue Watching',34,INK,700)
    for i,t in enumerate(['Northern Light','Moon Harbor','The Crossing','Quiet Earth']):p.poster(80+i*445,465,410,210,t,'S2 E6 · 30 min left' if i==0 else 'Movie · Continue',n=i)
    p.text(80,865,'Next Up',34,INK,700)
    for i,t in enumerate(['The next chapter','A distant shore','The signal','Coming home']):p.button(80+i*445,900,410,t,h=72)
    p.footer('Up from the first shelf  ·  Navigation       Back from a title  ·  Return to the same card')
    return p

def search():
    p=Plate('Proposed Search with fixed query and recoverable results');p.header('Search')
    p.text(80,215,'Find something to watch',53,INK,700);p.button(80,270,1480,'Search: northern',focus=True,h=80);p.button(1590,270,250,'Clear',h=80)
    p.text(80,410,'8 previous results · Search could not update',28,MUTED);p.button(1550,380,290,'Retry search',h=62)
    for i,t in enumerate(['Northern Light','Northern Passage','Northern Coast','Northbound','The North Wind','North of Home']):p.poster(80+i*298,490,262,325,t,'Movie · 2024',n=i)
    p.footer('Back closes the keyboard first · Return from a title keeps its result and scroll position')
    return p

def player():
    p=Plate('Proposed shared TV playback chrome with reporting in More');p.scene(0,0,1920,1080)
    p.text(80,100,'Northern Light',32,INK,700)
    p.rect(1120,130,710,680,'#141419',22);p.text(1180,210,'More',44,INK,700)
    for y,label in [(265,'Audio & subtitles'),(365,'Quality'),(465,'Playback info'),(565,'Report a problem'),(665,'Close')]:p.button(1180,y,590,label,focus=y==565,h=78)
    p.text(80,802,'A place to return',46,INK,700);p.text(80,850,'Episode 6 · Season 2',26,MUTED)
    p.rect(80,890,1760,6,'#42464c',3);p.rect(80,890,650,6,RED,3);p.text(80,937,'18:24',24);p.text(1730,937,'48:00',24)
    for x,w,label in [(590,150,'−10'),(760,190,'Pause'),(970,150,'+10'),(1140,190,'More')]:p.button(x,970,w,label,h=66)
    return p

def live():
    p=Plate('Proposed Live TV and channel guide with compact controls');p.header('Live TV')
    p.button(80,160,220,'On now');p.button(320,160,220,'Guide',selected=True);p.button(1370,160,220,'Search');p.button(1610,160,230,'More')
    p.scene(80,270,610,343);p.text(740,320,'8.1  ·  North Coast',26,RED,700);p.text(740,380,'The evening report',47,INK,700)
    p.lines(740,435,['A look at the stories shaping our region.', 'Today · 18:00–19:00'],28,MUTED,42)
    p.button(740,545,230,'Watch');p.button(990,545,260,'Programme…',focus=True);p.button(1490,545,350,'Guide time: Now')
    p.text(85,680,'CHANNEL',22,MUTED);p.text(420,680,'18:00',22,MUTED);p.text(950,680,'18:30',22,MUTED);p.text(1480,680,'19:00',22,MUTED)
    for y,name,a,b in [(710,'8.1 North Coast','The evening report','Across the valley'),(805,'9.2 Field Notes','Wild places','The long journey'),(900,'12.1 Cinema','Open Water','Moon Harbor')]:
        p.button(80,y,300,name,h=76);p.button(405,y,770,a,h=76);p.button(1200,y,640,b,h=76)
    p.footer('Select a future programme  ·  Details and available actions       Back from details  ·  Same guide cell')
    return p

def dvr():
    p=Plate('Proposed recordings list and safe confirmation');p.header('Recordings')
    p.text(80,210,'Your recordings',52,INK,700)
    for x,label,w in [(80,'Saved',220),(320,'Upcoming',260),(600,'Attention',260),(880,'More',220)]:p.button(x,260,w,label,selected=label=='Saved')
    for y,t,m in [(385,'Across the valley','Recorded today · 52 min'),(505,'The evening report','Recorded yesterday · 60 min'),(625,'Wild places','Recorded Tuesday · 48 min')]:
        p.button(80,y,1760,t,h=95);p.text(1000,y+57,m,26,MUTED)
    p.rect(0,0,1920,1080,'#00000099',0);p.rect(470,330,980,450,'#1b1b21',24)
    p.text(530,420,'Delete “Wild places”?',43,INK,700);p.lines(530,485,['The recording and its saved file will be removed.', 'Other episodes and the recording rule stay.'],28,MUTED,42)
    p.button(530,640,400,'Keep recording',focus=True,h=80);p.button(970,640,420,'Delete recording',h=80)
    p.footer('Confirmation owns focus · Back cancels · Deletion returns to the next recording')
    return p

def settings():
    p=Plate('Proposed settings categories and a simple TV report handoff');p.header('Settings');p.text(80,215,'Settings',52,INK,700)
    for i,label in enumerate(['Playback','Appearance','Help','Live TV','Sharing','Account','Remotes','Developer','About']):p.button(80,270+i*76,350,label,selected=label=='Help',h=60)
    p.text(510,310,'Help',48,INK,700);p.text(510,370,'Capture a problem, then save it on your phone.',29,MUTED)
    p.button(510,435,670,'Report a problem',focus=True,h=85);p.button(510,545,670,'Recent reports',h=75);p.button(510,645,670,'Connection checks',h=75)
    p.rect(1260,270,580,650,'#17171d',20);p.text(1310,345,'Report saved locally',34,INK,700)
    p.lines(1310,400,['Ready to save on your phone.', 'Choose Save on phone to', 'show a temporary download QR.'],26,MUTED,38)
    p.rect(1410,540,260,200,'#2c2c34',12);p.text(1455,635,'QR preview',26,MUTED);p.text(1438,680,'after you choose',21,MUTED)
    p.button(1310,780,480,'Save on phone',h=72)
    p.footer('Help stays in the first visible categories · Advanced diagnostics remain available inside Help')
    return p

def connect():
    p=Plate('Proposed connection recovery with contextual Help');p.text(80,95,'cinema',38,RED,700)
    p.text(450,300,'Cinema is unavailable',58,INK,700);p.lines(450,385,['Your sign-in is saved.', 'Check the server connection, then try again.'],31,MUTED,48)
    p.button(450,535,1020,'Try again',focus=True,h=90);p.button(450,655,1020,'Use a different server',h=80);p.button(450,765,1020,'Help',h=80)
    p.text(450,925,'No account changes are needed to retry.',26,MUTED)
    return p

def shared():
    p=Plate('Proposed Shared title uses the same TV detail grammar');p.scene(0,0,1920,1080);p.text(80,90,'‹  Shared libraries  /  Family Movies',29,MUTED)
    p.text(100,280,'SHARED BY FAMILY CINEMA',24,RED,700);p.text(100,365,'Moon Harbor',72,INK,700);p.text(100,435,'2024   ·   1h 52m   ·   4K',29,MUTED)
    p.lines(100,515,['A remote coastal town prepares for a visitor', 'who may change everything.'],32,INK,47)
    p.button(100,665,340,'Resume · 32:18',focus=True,h=85);p.button(470,665,260,'Start over',h=85);p.button(760,665,300,'Options',h=85)
    p.button(100,785,250,'About',h=72);p.button(380,785,350,'Source status',h=72)
    p.footer('Playback uses the same controls · The Source remains visible in details and options')
    return p

def gate():
    p=Plate('Proposed navigation qualification receipt, not a product screen');p.text(80,100,'RELEASE EVIDENCE · ILLUSTRATIVE',23,RED,700);p.text(80,185,'Navigation must be proven',59,INK,700)
    p.text(80,250,'A compile is not a remote journey. A missing receipt is not a pass.',30,MUTED)
    p.text(80,355,'REQUIRED JOURNEY',23,MUTED);p.text(1020,355,'NATIVE FOCUS',23,MUTED);p.text(1450,355,'RESULT',23,MUTED)
    for y,name,focus,status,color in [(405,'Movies → View → Close','Inside menu → View','PASS',INK),(505,'Movie 416 → Details → Back','Same movie retained','PASS',INK),(605,'Playback → More → Report','More restored','PASS',INK),(705,'Delete → Back','Keep → same recording','PASS',INK),(805,'Physical Siri Remote sweep','Not yet recorded','MISSING',RED)]:
        p.rect(80,y,1760,75,SURFACE,8);p.text(105,y+48,name,29);p.text(1020,y+48,focus,26,MUTED);p.text(1450,y+48,status,27,color,700)
    p.text(80,990,'RELEASE HELD · Complete the physical acceptance receipt for this candidate.',30,RED,700)
    return p

items=[
('R01','library','Library tools within reach',library,'F04–F05 · F27','Tools stays beside the content. Left from the first column reaches it; Right restores the last movie. The current item survives detail, playback and view changes.'),
('R02','choice-sheet','A menu that owns focus',modal,'F01–F03 · F14 · F25','Opening moves focus to the selected choice. Covered content cannot receive input. Checkmark means selected; the light outline means focused. Back restores the opener.'),
('R03','details','Details before playback',detail,'F06–F08 · F28','About exposes the full synopsis. Short, stable action labels open complete track and version choices. Season shelves also expose Episode options without requiring playback.'),
('R04','home','Recover without leaving Home',home,'F09 · F27','Retain usable shelves when a refresh fails and offer Retry. Every shelf remembers its card, and all cards use a visible focus treatment.'),
('R05','search','Search without losing your place',search,'F10','The query stays available. Previous results remain while a replacement loads; failed searches have Retry. Keyboard Back returns to the field before leaving Search.'),
('R06','playback','Report belongs inside More',player,'F11–F12 · F17 · F24','This plate shows revealed controls with More open. With chrome hidden, only the picture remains. Local, live, library-channel and shared players use the same presentation rules.'),
('R07','live-guide','One guide navigation grammar',live,'F13–F16','Keep the toolbar compact. Guide time belongs beside the guide. Every programme has a real action; future programmes open details. Return restores channel and time.'),
('R08','recordings','A safe, reachable confirmation',dvr,'F18–F21 · F28','A native confirmation owns focus and starts on Keep. Back cancels. Recording rows open details; secondary actions and readable history live there. Manual scheduling uses a channel-name picker.'),
('R09','settings-help','Help that does not cover the film',settings,'F22–F23 · F26–F28','Categories keep Help reachable. Capture first, then offer an explicit phone download. Advanced diagnostics and history stay available. Device resets use the same safe confirmation pattern.'),
('R10','connection','Recovery actions in one place',connect,'F09 · F11 · F22','Retry, server selection and Help share one predictable focus path. No detached reporting button competes with the connection form.'),
('R11','shared-title','Shared media feels like Cinema',shared,'F24–F25','Preserve Source identity and authorization while reusing the detail and playback interaction model. Menu closure returns to its actual opener.'),
('R12','qualification','A release contract with evidence',gate,'F29','Illustrative acceptance receipt, not a product screen or a real passing result. Every required journey is bound to the candidate source and app build. Missing physical acceptance holds release.')]
for _,key,_,make,_,_ in items:make().save(key)

cards=''.join(f'<button class="tile" data-key="{key}" aria-pressed="{str(i==0).lower()}"><span>{rid}</span>{escape(title)}</button>' for i,(rid,key,title,_,ids,desc) in enumerate(items))
data=[dict(id=rid,key=key,title=title,findings=ids,description=desc) for rid,key,title,_,ids,desc in items]
html='''<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Apple TV navigation — proposed repairs</title><style>
:root{color-scheme:dark;font:16px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;background:#0b0b0e;color:#ededf0}*{box-sizing:border-box}body{margin:0}main{max-width:1540px;margin:auto;padding:42px 40px 80px}a{color:#f2b7ba;text-underline-offset:4px}header{display:flex;gap:40px;align-items:end;justify-content:space-between;margin-bottom:30px}.eyebrow{color:#ef777b;text-transform:uppercase;letter-spacing:2px;font-size:12px;font-weight:650}h1{font-size:clamp(28px,4vw,48px);line-height:1.15;letter-spacing:-1px;margin:12px 0}h2{font-size:24px;line-height:1.3;margin:0 0 12px}.lede{color:#aaaab5;max-width:850px;margin:12px 0 0}nav{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:10px;margin:30px 0}.tile{background:#17171d;color:#c8c8d0;border:1px solid #33333d;text-align:left;border-radius:9px;padding:13px 15px;font:inherit;cursor:pointer;min-height:68px}.tile span{display:block;color:#9999a6;font-size:11px;letter-spacing:1px;margin-bottom:4px}.tile[aria-pressed=true]{border-color:#ef777b;color:white;background:#2a1a1e}.tile:focus-visible,button:focus-visible{outline:3px solid white;outline-offset:3px}.screen{width:100%;height:auto;display:block;border-radius:14px;background:#111;border:1px solid #363640}.caption{display:grid;grid-template-columns:1fr 2fr;gap:30px;margin:24px 0 44px}.caption p{margin:0;color:#b9b9c3}.caption small{color:#ef777b}.proof{border-top:1px solid #33333d;padding-top:35px}.proof-grid{display:grid;grid-template-columns:1.6fr 1fr;gap:30px}.proof img{width:100%;height:auto;border-radius:10px}.proof p{color:#b9b9c3}.tag{display:inline-block;padding:5px 10px;border-radius:5px;font-size:12px;background:#2a1b1d;color:#f2b7ba}.links{display:flex;gap:20px;flex-wrap:wrap}.key{font-size:13px;color:#92929e}footer{margin-top:40px;color:#9999a6;font-size:13px}@media(max-width:900px){main{padding:24px 20px}header{display:block}nav{grid-template-columns:repeat(2,minmax(0,1fr))}.caption,.proof-grid{grid-template-columns:1fr;gap:16px}}@media print{nav{display:none}.screen{break-inside:avoid}body{background:white;color:black}}
</style></head><body><main><header><div><div class="eyebrow">Noirr Cinema · Apple TV · October 15, 2026</div><h1>Navigation you can trust.</h1><p class="lede">29 findings and risks. Twelve proposed screens and acceptance plates. One native focus failure reproduced with 416 synthetic movies. These designs are not implemented fixes.</p></div><a href="../APPLE-TV-NAVIGATION-AUDIT.md">Read the full audit ↗</a></header><nav aria-label="Proposed screens">'''+cards+'''</nav><figure style="margin:0"><img class="screen" id="screen" src="renderings/library.svg" alt="Proposed movie library with reachable tools and retained position"><figcaption class="caption"><div><small id="rid">R01 · F04–F05 · F27</small><h2 id="title">Library tools within reach</h2><a id="full" href="renderings/library.svg" target="_blank" rel="noopener">Open full-size rendering ↗</a></div><p id="description" aria-live="polite">Tools stays beside the content. Left from the first column reaches it; Right restores the last movie. The current item survives detail, playback and view changes.</p></figcaption></figure><section class="proof"><div class="proof-grid"><div><span class="tag">ACTUAL NATIVE REPRODUCTION · NOT A MOCKUP</span><h2 style="margin-top:16px">The menu is visible. Focus is behind it.</h2><a href="evidence/modal-focus-behind.png"><img src="evidence/modal-focus-behind.png" alt="tvOS View menu with blank red controls; A Movie 10 remains focused behind it"></a></div><div><p>Opening View left native focus on the toolbar. Three Down presses selected A Movie 10 behind the panel. The menu's Rows, Grid and Close names exist in the accessibility tree, but their labels are not visually legible in this capture.</p><p>The harness uses the unchanged library UI with synthetic data and a direct entry. It does not establish a whole-app or physical-device pass.</p><div class="links"><a href="evidence/receipt.json">Evidence receipt</a><a href="evidence/focus-trace.txt">Focus trace</a><a href="evidence/synthetic-harness.patch">Reproduce</a></div><p><strong>Recommended first repair:</strong> native modal focus ownership, readable choices, and reporting inside player More. Then address browsing, secondary screens and the required remote-journey gate.</p></div></div></section><footer>Proposal baseline: 9eb7f6738647a7e963b17cfb3a2f77a594863c41. Abstract artwork and titles are illustrative. R12 is an example of the proposed qualification policy, not actual passing evidence.</footer></main><script>
const plates='''+json.dumps(data)+''';
function show(key){const p=plates.find(p=>p.key===key);if(!p)return;document.getElementById('screen').src='renderings/'+p.key+'.svg';document.getElementById('screen').alt=p.title+' — proposed rendering';document.getElementById('rid').textContent=p.id+' · '+p.findings;document.getElementById('title').textContent=p.title;document.getElementById('description').textContent=p.description;document.getElementById('full').href='renderings/'+p.key+'.svg';document.querySelectorAll('[data-key]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.key===key)));history.replaceState(null,'','#'+key);}
document.querySelectorAll('[data-key]').forEach(b=>b.addEventListener('click',()=>show(b.dataset.key)));if(location.hash)show(location.hash.slice(1));
</script></body></html>'''
(ROOT/'index.html').write_text(html)
