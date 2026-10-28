-- Behavioral tests execute the shipped script; mpv's API is replaced at its boundary.
local script = arg[1] or 'plurx_player/scripts/cinema.lua'
local P, bindings, watchers, events, commands, timers, overlay = {}, {}, {}, {}, {}, {}, {}
local reads = {}
local now, mx, my, width, height = 0, 0, 0, 1280, 720
package.preload['mp'] = function() return {
    get_property_native=function(n) reads[n]=(reads[n] or 0)+1; return P[n] end,
    set_property=function(n,v) P[n]=v; commands[#commands+1]={'set',n,v} end,
    set_property_number=function(n,v) P[n]=v; commands[#commands+1]={'set',n,v} end,
    commandv=function(...) commands[#commands+1]={...} end,
    create_osd_overlay=function() overlay.update=function() overlay.removed=false end; overlay.remove=function() overlay.removed=true end; return overlay end,
    get_osd_size=function() return width,height end,
    get_mouse_pos=function() return mx,my end,
    get_time=function() return now end,
    add_forced_key_binding=function(k,n,f) bindings[n]=f end,
    add_key_binding=function(k,n,f) bindings[n]=f end,
    remove_key_binding=function(n) bindings[n]=nil end,
    observe_property=function(n,t,f) watchers[n]=watchers[n] or {}; table.insert(watchers[n],f) end,
    register_event=function(n,f) events[n]=f end,
    add_periodic_timer=function(t,f) timers[#timers+1]=f end,
} end
package.preload['mp.options'] = function() return {read_options=function() end} end
package.preload['mp.assdraw'] = function() return {ass_new=function()
    local a={text=''}
    a.new_event=function(self) self.text=self.text..'\n' end
    a.append=function(self,s) self.text=self.text..s end
    a.draw_start=function() end; a.draw_stop=function() end; a.rect_cw=function() end
    return a
end} end
P={['time-pos']=5,duration=30,pause=false,['user-data/osc/film']={origin=60,duration=7200},
   ['track-list']={{type='audio',id=1,lang='eng',selected=true},{type='audio',id=2,lang='fra'},
                  {type='sub',id=3,lang='eng',selected=true}},sid=3}
dofile(script)
local function press(n) assert(bindings['cinema-'..n],n)() end
local function move(x,y) mx=x;my=y;press('move') end
local function down(x,y) move(x,y); assert(bindings['cinema-click'],'click target')( {event='down'} ) end
local function up(x,y) move(x,y); assert(bindings['cinema-click'],'release capture')( {event='up'} ) end
local function click(x,y) down(x,y);up(x,y) end
local function reset() commands={}; events['file-loaded']() end
local function last() return commands[#commands] end
local function eq(a,b) assert(a==b,tostring(a)..' ~= '..tostring(b)) end
-- A live film position uses origin + time; duration stays the full two hours.
assert(overlay.data:find('1:05',1,true)); assert(overlay.data:find('2:00:00',1,true))
-- Scrubbing previews only, then delegates an unavailable film position on release.
down(640,634); eq(#commands,0); assert(overlay.data:find('Enter to seek',1,true))
up(640,634); eq(last()[1],'script-message'); eq(last()[2],'osc-film-seek'); eq(tonumber(last()[3]),3600)
-- A target inside the held range seeks on the local stream, without adding origin twice.
reset(); local x=90+1100*72/7200; down(x,634);up(x,634)
eq(last()[1],'seek'); assert(math.abs(last()[2]-12)<0.001);eq(last()[3],'absolute+exact')
-- Cancel an unfinished scrub without issuing a seek.
reset();down(640,634);press('escape');up(640,634);eq(#commands,0)
-- Relative transport buttons also cross the live stream's origin via the launcher.
reset();click(98,681);eq(last()[1],'script-message');eq(tonumber(last()[3]),55)
-- Keyboard seeking previews until Enter. Escape discards it.
reset();press('tab');press('tab');press('tab'); -- timeline is third after header buttons
press('right');eq(#commands,0);press('escape');eq(#commands,0)
press('right');press('enter');eq(last()[1],'seek');eq(last()[2],15)
-- Without timeline focus, arrows seek immediately, even when chrome was hidden.
reset();press('right');eq(last()[1],'seek');eq(last()[2],15)
-- Track selection uses actual mpv IDs; subtitles can be switched off.
reset();click(780,681);press('tab');press('enter');eq(P.aid,2)
reset();click(855,681);press('enter');eq(P.sid,'no')
-- Long lists remain navigable by keyboard rather than being silently truncated.
P['track-list']={};for i=1,20 do table.insert(P['track-list'],{type='audio',id=i,lang='eng'}) end
reset();click(780,681);for i=1,19 do press('down') end;press('enter');eq(P.aid,20)
-- Settings don't change any video or colour properties.
reset();click(1035,681);press('down');press('down');press('down');press('enter');eq(P.speed,1.25)
-- Four-second auto-hide, held open while paused or during a scrub.
reset();now=5;timers[1]();eq(overlay.removed,true)
press('right');eq(overlay.removed,false);eq(last()[1],'seek')
-- Ordinary keyboard focus still permits auto-hide.
press('tab');now=10;timers[1]();eq(overlay.removed,true)
P.pause=true;reset();now=20;timers[1]();eq(overlay.removed,false)
P.pause=false;reset();down(640,634);now=30;timers[1]();eq(overlay.removed,false);press('escape')
-- Capability-bearing URLs are never shown as a title; ASS markup stays literal.
P['force-media-title']='https://media1/secret-session/master.m3u8';reset();assert(not overlay.data:find('secret-session',1,true))
P['force-media-title']='Movie {\\bord99}';reset();assert(overlay.data:find('Movie \\{',1,true))
-- Narrow windows wrap tools above the seek row and retain all transport actions.
width=560;reset();assert(overlay.data:find('Fullscreen',1,true)); click(155,681);eq(last()[1],'cycle');eq(last()[2],'pause')
-- Synthetic accessibility/IPC clicks arrive as press rather than down/up.
width=1280;reset();move(780,681);bindings['cinema-click']({event='press'})
assert(overlay.data:find('Audio tracks',1,true))
-- Complete VOD streams use local time, without the live-session offset.
P['user-data/osc/film']=nil;P.duration=7200;P['time-pos']=100
reset();click(222,681);eq(last()[1],'seek');eq(last()[2],110)
-- Native application data is observed, including while playback is paused.
local function update(name,value)
    P[name]=value
    for _,callback in ipairs(watchers[name] or {}) do callback(name,value) end
end
local function context(extra)
    local value={metadata={title='Pilot {\\bord99}',series_title='The Expedition',kind='episode',
        overview='A remote crew crosses the ridge.',year=2026,season_number=1,episode_number=2,runtime_ms=2520000},
        next={file_id=8,item_id=80,metadata={title='Across the Valley'}},
        quality={selected='auto',choices={{id='auto',label='Auto'},{id='original',label='Original'},{id='1080',label='1080p'}}},
        autoplay_next=true,busy=false}
    for k,v in pairs(extra or {}) do value[k]=v end
    return value
end
width=1280;reset();update('user-data/cinema',context())
assert(overlay.data:find('The Expedition · S01 E02 · 2026 · 42 min',1,true))
assert(overlay.data:find('Pilot \\{',1,true));assert(overlay.data:find('Across the Valley',1,true))
-- Requested 1080p never masquerades as the currently decoded 2160p picture.
update('video-params',{w=3840,h=2160})
local c=context();c.quality.selected='1080';update('user-data/cinema',c)
assert(overlay.data:find('Manual · 2160p',1,true));assert(not overlay.data:find('Manual · 1080p',1,true))
-- Only the supplied quality catalogue can produce an actionable quality row.
reset();click(930,681);press('down');press('enter')
eq(last()[1],'script-message');eq(last()[2],'cinema-quality');eq(last()[3],'original')
-- Duplicate transitions are suppressed before Python has even published busy.
local count=#commands;click(1070,560);eq(#commands,count)
update('user-data/cinema',context({busy=true}));click(1070,560);eq(#commands,count)
assert(overlay.data:find('Updating playback',1,true))
update('user-data/cinema',context());click(1070,560)
eq(last()[1],'script-message');eq(last()[2],'cinema-next');eq(#last(),2)
-- No arbitrary target ID is sent by the overlay: the server resolves next.
update('user-data/cinema',context({next=false}));assert(not overlay.data:find('Next episode',1,true))
-- Autoplay appears in Settings and is explicitly changed through the launcher.
reset();update('user-data/cinema',context());click(1035,681)
for i=1,9 do press('down') end;press('enter')
eq(last()[2],'cinema-autoplay');eq(last()[3],'no')
-- An authoritative empty candidate array cannot invent manual quality options.
reset();update('user-data/cinema',context({quality={selected='auto',choices={}}}));click(930,681)
assert(overlay.data:find('Quality choices are unavailable',1,true));local count_before=#commands
press('enter');eq(#commands,count_before)
-- Both Info entry points show playback observations, including changes without
-- a property notification. This executes the shipped script, not a row replica.
local function test_info_buttons_report_live_observations()
    local function contents()
        local pieces={overlay.data}
        for i=1,60 do press('down');pieces[#pieces+1]=overlay.data end
        return table.concat(pieces,'\n')
    end
    local function contains(value,part) assert(value:find(part,1,true),part..' missing') end
    reset();update('user-data/cinema',context({playback={mode='transcode',transport='hls',session='live'}}))
    P['video-codec']='HEVC';P['video-format']='hevc';P['idle-active']=false
    P['video-dec-params']={w=3840,h=2160}
    P['video-params']={w=3840,h=2160,pixelformat='cuda',['hw-pixelformat']='p010',
        gamma='pq',primaries='bt.2020',colormatrix='dolbyvision',colorlevels='limited'}
    P['osd-dimensions']={w=2560,h=1600,ml=0,mr=0,mt=80,mb=80}
    P['video-target-params']={gamma='pq',primaries='bt.2020',['max-luma']=465}
    P['current-vo']='gpu-next';P['current-gpu-context']='waylandvk';P['hwdec-current']='nvdec'
    P['estimated-vf-fps']=23.976;P['video-bitrate']=24500000
    P['frame-drop-count']=0;P['decoder-frame-drop-count']=2
    P['track-list']={{type='audio',selected=true,codec='eac3',lang='eng'}}
    P['audio-params']={channels='5.1',samplerate=48000,format='floatp'}
    P['audio-out-params']={channels='stereo',samplerate=48000,format='float'}
    P['current-ao']='pipewire';P['audio-bitrate']=768000;P['avsync']=-0.012;P['audio-delay']=0.05
    P['demuxer-cache-state']={['cache-duration']=17.25,['raw-input-rate']=4000000,['fw-bytes']=1234567}
    local previous=reads['hwdec-current'] or 0
    timers[1]();eq(reads['hwdec-current'] or 0,previous)
    click(1125,681)
    contains(overlay.data,'Playback info');assert(not overlay.data:find('A remote crew',1,true))
    local value=contents()
    for _,part in ipairs({'Method: Transcode','Segmented HLS','Live HLS session',
            'Stream frame (mpv decoder): 3840×2160','Player window size: 2560×1600','Video display area: 2560×1440',
            'cuda · p010','Dolby Vision (mpv matrix)','bt.2020 · pq','465 nits',
            'gpu-next · waylandvk','Decoder: nvdec','23.98 fps','24 Mb/s',
            'Dropped frames (renderer): 0','Dropped frames (decoder): 2','eac3 · eng',
            '5.1 · 48000 Hz · floatp','pipewire · stereo','768 kb/s','-12 ms','+50 ms',
            '17.2 s','32 Mb/s','1234567 B'}) do contains(value,part) end
    -- Paused samples still refresh; neither a notification nor transport command is needed.
    for i=1,60 do press('up') end
    for i=1,60 do
        if overlay.data:find('Decoder: nvdec',1,true) then break end
        press('down')
    end
    contains(overlay.data,'Decoder: nvdec')
    P.pause=true;P['frame-drop-count']=11;P['hwdec-current']='no'
    P['demuxer-cache-state']={['cache-duration']=2.5}
    timers[1]();contains(overlay.data,'Decoder: Software (mpv)')
    for i=1,60 do press('up') end
    value=contents();contains(value,'Software (mpv)');contains(value,'Dropped frames (renderer): 11')
    contains(value,'2.5 s');assert(not value:find('17.2 s',1,true))
    -- Late unavailable/malformed values replace old facts, never become zero or requested options.
    P['hwdec-current']=nil;P['frame-drop-count']=nil;P['video-dec-params']='unavailable'
    P['osd-dimensions']={w=1920};P['demuxer-cache-state']=nil
    P.hwdec='auto-safe';P.vo='gpu-next';P['current-vo']=nil;P['current-gpu-context']=nil
    P['video-codec']='https://media1/private?token=secret';P['video-format']='/private/media.mkv'
    P['video-target-params']={['max-luma']=0};P['estimated-vf-fps']=0/0
    timers[1]();for i=1,60 do press('up') end
    value=contents()
    for _,part in ipairs({'Stream frame (mpv decoder): Unavailable','Player window size: Unavailable','Video display area: Unavailable',
            'Decoder: Unavailable','Renderer: Unavailable','Dropped frames (renderer): Unavailable',
            'Frame rate (estimated): Unavailable','VO target peak: Unavailable',
            'Stream format: Unavailable','Demuxer buffered bytes: Unavailable'}) do contains(value,part) end
    assert(not value:find('token=secret',1,true));assert(not value:find('/private',1,true))
    assert(not value:find('auto-safe',1,true))
    press('escape');previous=reads['hwdec-current'];timers[1]();eq(reads['hwdec-current'],previous)
    click(1100,34);contains(overlay.data,'Playback info');contains(contents(),'Decoder: Unavailable')
    press('escape');P.pause=false
    -- A narrow panel wraps every diagnostic line; all rows remain reachable.
    width=560;reset();click(400,575);value=contents();contains(value,'Demuxer buffered bytes')
    contains(value,'Unavailable');assert(not value:find('A remote crew',1,true))
    width=1280;reset()
end
test_info_buttons_report_live_observations()
-- Open menus win input over the underlying Next card at the same coordinates.
reset();update('user-data/cinema',context());click(930,681)
click(1070,565);assert(not last() or last()[2]~='cinema-next')
-- Compact controls occupy separate rows; next does not intercept transport.
width=560;reset();update('user-data/cinema',context());click(155,681)
eq(last()[1],'cycle');eq(last()[2],'pause')
click(250,575);press('down');press('enter');eq(last()[2],'cinema-quality')
-- Errors are escaped and visible while real transport remains usable.
reset();update('user-data/cinema',context({error='Quality refused {\\bord99}'}))
assert(overlay.data:find('Quality refused \\{',1,true));click(155,681);eq(last()[2],'pause')
-- An early EOF exposes recovery without advancing or issuing duplicate requests.
width=1280;reset();update('user-data/cinema',context());click(1125,681)
assert(overlay.data:find('Playback info',1,true))
update('user-data/cinema',context({retryable=true,error='Playback ended early.'}))
assert(not overlay.data:find('Method: ',1,true))
assert(overlay.data:find('Retry',1,true));click(640,150)
eq(last()[1],'script-message');eq(last()[2],'cinema-retry');eq(#last(),2)
local retry_commands=#commands;click(640,150);eq(#commands,retry_commands)
update('user-data/cinema',context({retryable=true,error='Playback ended early.',busy=true}))
click(640,150);eq(#commands,retry_commands)
update('user-data/cinema',context({retryable=false}));assert(not overlay.data:find('Retry',1,true))
-- Selecting the current Auto row is a no-op and never fences a later Next.
reset();update('user-data/cinema',context());click(930,681);press('enter')
eq(#commands,0);assert(not overlay.data:find('Playback quality',1,true))
click(1070,560);eq(last()[2],'cinema-next')
-- The same applies to selected Original; subsequent autoplay still reaches native.
reset();local original=context();original.quality.selected='original'
update('user-data/cinema',original);click(930,681);press('down');press('enter')
eq(#commands,0);assert(not overlay.data:find('Playback quality',1,true))
click(1035,681);for i=1,9 do press('down') end;press('enter')
eq(last()[2],'cinema-autoplay');eq(last()[3],'no')
print('Cinema overlay: 32 control scenarios and live playback Info regression passed')
