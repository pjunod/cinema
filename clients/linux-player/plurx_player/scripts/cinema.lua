-- Cinema's playback chrome, drawn inside mpv's existing video surface.
-- No video, colour, decoder or server-session options belong in this script.
local mp = require 'mp'
local assdraw = require 'mp.assdraw'
local opts = {accent = '6EA8FE', hide_ms = 4000}
require('mp.options').read_options(opts, 'cinema')
local overlay = mp.create_osd_overlay('ass-events')
local S = {shown = true, touched = 0, buttons = {}, focus = nil, menu = nil,
           row = 1, preview = nil, dragging = false, hover = nil, mouse_bound = false}
local W, H = 1280, 720
local render, activity, mouse, click, key, play_pause
local function number(v) return type(v) == 'number' and v == v and math.abs(v) < math.huge end
local function clamp(v, lo, hi) return math.max(lo, math.min(hi, v)) end
local function prop(name, fallback) local v = mp.get_property_native(name); if v == nil then return fallback end; return v end
local function bgr(rgb) return rgb:sub(5,6)..rgb:sub(3,4)..rgb:sub(1,2) end
local accent = opts.accent:match('^%x%x%x%x%x%x$') and bgr(opts.accent) or 'FEA86E'
local function escape(s)
    -- Break ASS's backslash escapes with an invisible separator; braces are literal.
    return tostring(s or ''):gsub('\\', '\\\239\187\191'):gsub('{', '\\{'):gsub('\n', '\\N')
end
local function cinema()
    local value = prop('user-data/cinema', {})
    return type(value) == 'table' and value or {}
end
local function metadata()
    local value = cinema().metadata
    return type(value) == 'table' and value or {}
end
local function clean(value)
    return type(value) == 'string' and value or ''
end
local function title()
    if clean(metadata().title) ~= '' then return metadata().title end
    local value = prop('force-media-title', '')
    if value == '' then value = prop('metadata/by-key/title', 'Cinema') end
    -- Never display capability-bearing playback URLs as the movie's title.
    if value:match('^%a[%w+.-]*://') then return 'Cinema' end
    return value
end
local function time(s)
    if not number(s) then return '--:--' end
    s = math.floor(math.max(0, s))
    if s >= 3600 then return string.format('%d:%02d:%02d', math.floor(s/3600), math.floor(s/60)%60, s%60) end
    return string.format('%d:%02d', math.floor(s/60), s%60)
end
local function film()
    local f = prop('user-data/osc/film')
    if type(f) == 'table' and number(f.origin) and number(f.duration) and f.duration > 0 then return f end
end
local function duration() local f = film(); return f and f.duration or prop('duration', 0) end
local function position() local f = film(); return (f and f.origin or 0) + prop('time-pos', 0) end
local function seek(target)
    local d, f = duration(), film()
    if not number(target) or not number(d) or d <= 0 then return end
    target = clamp(target, 0, math.max(0, d - 0.05))
    local local_target = target - (f and f.origin or 0)
    local held = prop('duration', 0)
    if f and (local_target < 0 or local_target > held) then
        mp.commandv('script-message', 'osc-film-seek', tostring(target))
    else
        mp.commandv('seek', local_target, 'absolute+exact')
    end
end
local function label(track)
    local parts = {}
    if track.title and track.title ~= '' then parts[#parts+1] = track.title end
    if track.lang then parts[#parts+1] = track.lang end
    if track.codec then parts[#parts+1] = track.codec:upper() end
    if #parts == 0 then parts[1] = 'Track '..tostring(track.id) end
    return table.concat(parts, ' · ')
end
-- Metadata is prose, not ASS. Wrap by Unicode codepoint so long titles,
-- unbroken words and non-ASCII synopsis text remain readable in a clipped panel.
local function wrapped(value, columns)
    local lines, line, count = {}, '', 0
    local function flush()
        if line ~= '' then lines[#lines+1] = line; line=''; count=0 end
    end
    for paragraph in (clean(value):sub(1,16384)..'\n'):gmatch('(.-)\n') do
        for word in paragraph:gmatch('%S+') do
            local chars = {}
            for c in word:gmatch('[^\128-\191][\128-\191]*') do chars[#chars+1]=c end
            if count > 0 then
                if count+1+#chars > columns then flush()
                else line=line..' '; count=count+1 end
            end
            for _,c in ipairs(chars) do
                if count == columns then flush() end
                line=line..c; count=count+1
            end
        end
        flush()
    end
    return lines
end
local function metadata_details(m)
    local parts = {}
    if clean(m.series_title) ~= '' then parts[#parts+1]=m.series_title end
    if number(m.season_number) and number(m.episode_number) then
        parts[#parts+1]=string.format('S%02d E%02d',m.season_number,m.episode_number)
    end
    if number(m.year) then parts[#parts+1]=tostring(m.year) end
    if number(m.runtime_ms) and m.runtime_ms > 0 then
        parts[#parts+1]=string.format('%d min',math.floor(m.runtime_ms/60000+0.5))
    end
    if #parts == 0 and clean(m.air_date) ~= '' then parts[1]=m.air_date end
    return table.concat(parts,' · ')
end
local function busy() return cinema().busy == true or S.request_pending ~= nil end
local function request(name,value)
    if busy() then return end
    S.request_pending=name
    if value then mp.commandv('script-message',name,value)
    else mp.commandv('script-message',name) end
    activity()
end
local function next_episode()
    local next = cinema().next
    return type(next) == 'table' and type(next.metadata) == 'table' and next or nil
end
-- Read only documented mpv observations. Missing values stay unknown, including
-- after a file/quality transition; requested options never stand in for output.
local function observed(value)
    if type(value) ~= 'string' or value == '' or value:find('://',1,true)
            or value:match('^[/~]') or value:find('token=',1,true) then return nil end
    return value:gsub('[%c]',' '):sub(1,300)
end
local function measurement(value, format)
    if number(value) and value >= 0 then return string.format(format,value) end
end
local function dimensions(p, x, y)
    if number(p[x]) and p[x]>0 and number(p[y]) and p[y]>0 then
        return string.format('%d×%d',p[x],p[y])
    end
end
local function joined(...)
    local values={}
    for i=1,select('#',...) do
        local value=select(i,...)
        if value then values[#values+1]=value end
    end
    if #values>0 then return table.concat(values,' · ') end
end
local function bitrate(value)
    if not number(value) or value<0 then return nil end
    if value<1000000 then return string.format('%.0f kb/s',value/1000) end
    return string.format(value<10000000 and '%.1f Mb/s' or '%.0f Mb/s',value/1000000)
end
local function audio_format(p)
    return joined(observed(p['hr-channels']) or observed(p.channels),
        measurement(p.samplerate,'%.0f Hz'),observed(p.format))
end
local function playback_info()
    local r={}
    local columns=math.max(20,math.floor((math.min(680,W-48)-32)/9))
    local function row(label,value)
        for _,line in ipairs(wrapped(label..': '..(value or 'Unavailable'),columns)) do
            r[#r+1]={text=line}
        end
    end
    local function table_prop(name)
        local value=prop(name)
        return type(value)=='table' and value or {}
    end
    local p=table_prop('video-params')
    local decoded=table_prop('video-dec-params')
    local target=table_prop('video-target-params')
    local osd=table_prop('osd-dimensions')
    local cache=table_prop('demuxer-cache-state')
    local facts=cinema().playback
    facts=type(facts)=='table' and facts or {}
    local methods={direct='Direct play',remux='Remux',transcode='Transcode'}
    local transports={progressive='Progressive file · range requests',hls='Segmented HLS'}
    local sessions={none='No server-side session',live='Live HLS session',vod='VOD HLS session'}
    row('Method',methods[facts.mode])
    row('Transport',transports[facts.transport])
    row('Session',sessions[facts.session])
    local state
    if prop('eof-reached')==true then state='Ended'
    elseif prop('paused-for-cache')==true then state='Buffering'
    elseif prop('pause')==true then state='Paused'
    elseif prop('pause')==false and prop('idle-active')==false then state='Playing' end
    row('Player state',state)
    local total=duration()
    row('Position',number(prop('time-pos')) and (time(position())..' / '..
        (number(total) and total>0 and time(total) or 'Unavailable')) or nil)
    row('Stream format',joined(observed(prop('video-format')),observed(prop('video-codec'))))
    row('Stream frame (mpv decoder)',dimensions(decoded,'w','h'))
    row('Player window size',dimensions(osd,'w','h'))
    if number(osd.w) and number(osd.h) and number(osd.ml) and number(osd.mr)
            and number(osd.mt) and number(osd.mb) then
        row('Video display area',dimensions({w=osd.w-osd.ml-osd.mr,
            h=osd.h-osd.mt-osd.mb},'w','h'))
    else row('Video display area') end
    row('Pixel format',joined(observed(p.pixelformat),observed(p['hw-pixelformat'])))
    local range=observed(p.colormatrix)
    if range=='dolbyvision' then range='Dolby Vision (mpv matrix)'
    elseif p.gamma=='pq' then range='PQ HDR'
    elseif p.gamma=='hlg' then range='HLG HDR' end
    row('Dynamic range',range)
    row('Input color',joined(observed(p.primaries),observed(p.gamma),observed(p.colormatrix),observed(p['colorlevels'])))
    row('VO target color',joined(observed(target.primaries),observed(target.gamma)))
    row('VO target peak',number(target['max-luma']) and target['max-luma']>0
        and measurement(target['max-luma'],'%.0f nits') or nil)
    row('Renderer',joined(observed(prop('current-vo')),observed(prop('current-gpu-context'))))
    local hw=observed(prop('hwdec-current'))
    row('Decoder',hw=='no' and 'Software (mpv)' or hw)
    row('Frame rate (estimated)',measurement(prop('estimated-vf-fps'),'%.2f fps'))
    row('Video bitrate (packets)',bitrate(prop('video-bitrate')))
    row('Dropped frames (renderer)',measurement(prop('frame-drop-count'),'%.0f'))
    row('Dropped frames (decoder)',measurement(prop('decoder-frame-drop-count'),'%.0f'))
    local selected
    for _,track in ipairs(prop('track-list',{})) do
        if track.type=='audio' and track.selected then selected=track; break end
    end
    row('Stream audio track',selected and joined(observed(selected.codec),observed(selected.lang)))
    row('Decoded audio',audio_format(table_prop('audio-params')))
    row('Device audio output',joined(observed(prop('current-ao')),audio_format(table_prop('audio-out-params'))))
    row('Audio bitrate (packets)',bitrate(prop('audio-bitrate')))
    row('AV sync difference',number(prop('avsync')) and string.format('%+.0f ms',prop('avsync')*1000) or nil)
    row('Audio delay (applied)',number(prop('audio-delay')) and string.format('%+.0f ms',prop('audio-delay')*1000) or nil)
    row('Buffered on device (demuxer estimate)',measurement(cache['cache-duration'],'%.1f s'))
    row('Observed input rate (mpv estimate)',number(cache['raw-input-rate']) and bitrate(cache['raw-input-rate']*8) or nil)
    row('Demuxer buffered bytes',measurement(cache['fw-bytes'],'%.0f B'))
    return r
end
local function rows()
    local r = {}
    if S.menu == 'audio' or S.menu == 'sub' then
        local property = S.menu == 'audio' and 'aid' or 'sid'
        if S.menu == 'sub' then r[#r+1] = {text = 'Off', selected = prop('sid') == false or prop('sid') == 'no', action = function() mp.set_property(property, 'no') end} end
        for _, track in ipairs(prop('track-list', {})) do
            if track.type == S.menu then
                local id = track.id
                r[#r+1] = {text = label(track), selected = track.selected,
                    action = function() mp.set_property_number(property, id) end}
            end
        end
        if #r == 0 then r[1] = {text = 'No tracks in this stream'} end
    elseif S.menu == 'quality' then
        local q=cinema().quality
        local choices=type(q)=='table' and q.choices or nil
        for _,choice in ipairs(type(choices)=='table' and choices or {}) do
            if type(choice)=='table' and type(choice.id)=='string' and type(choice.label)=='string' then
                local id=choice.id
                local selected=q.selected==id
                r[#r+1]={text=choice.label,selected=selected,
                    action=function()
                        -- A no-op selection closes the menu without opening a
                        -- request fence that would need a native acknowledgement.
                        if not selected then request('cinema-quality',id) end
                    end}
            end
        end
        if #r==0 then r[1]={text='Quality choices are unavailable'} end
    elseif S.menu == 'settings' then
        for _, speed in ipairs({0.5, 0.75, 1, 1.25, 1.5, 2}) do
            local value = speed
            r[#r+1] = {text = string.format('Speed  %g×', value), selected = prop('speed', 1) == value,
                action = function() mp.set_property_number('speed', value) end}
        end
        for _, delta in ipairs({-0.1, 0.1}) do
            local value = delta
            r[#r+1] = {text = string.format('Audio sync  %+.1f s', value), action = function() mp.commandv('add', 'audio-delay', value) end}
        end
        r[#r+1] = {text = 'Reset audio sync', action = function() mp.set_property_number('audio-delay', 0) end}
        r[#r+1] = {text = 'Autoplay next episode', selected = cinema().autoplay_next == true,
            action = function() request('cinema-autoplay', cinema().autoplay_next == true and 'no' or 'yes') end}
    elseif S.menu == 'info' then
        r = playback_info()
    end
    return r
end
play_pause = function()
    if S.preview then
        local target = S.preview
        S.preview=nil; S.dragging=false
        seek(target)
    end
    mp.commandv('cycle','pause')
    activity()
end
local function toggle(menu)
    S.menu = S.menu ~= menu and menu or nil
    S.row = 1
    activity()
end
local function draw_rect(a, x, y, w, h, colour, alpha)
    a:new_event(); a:append(string.format('{\\an7\\pos(0,0)\\bord0\\shad0\\1c&H%s&\\1a&H%02X&}', colour or 'FFFFFF', alpha or 0))
    a:draw_start(); a:rect_cw(x, y, x+w, y+h); a:draw_stop()
end
local function text(a, x, y, value, size, colour, align)
    a:new_event(); a:append(string.format('{\\an%d\\pos(%g,%g)\\fnDejaVu Sans\\fs%d\\bord0\\shad0\\1c&H%s&}', align or 7, x, y, size or 18, colour or 'FFFFFF'))
    a:append(escape(value))
end
local function button(a, id, x, y, w, title, action, active)
    local hot = S.hover == id or S.focus == id
    if hot or active then draw_rect(a, x, y, w, 36, hot and '3C322E' or '1A1412', 45) end
    if S.focus == id then draw_rect(a, x, y+34, w, 2, accent) end
    text(a, x+w/2, y+18, title, 17, (hot or active) and accent or 'FFFFFF', 5)
    S.buttons[#S.buttons+1] = {id=id, x=x, y=y, w=w, h=36, action=action}
end
local function hit(x, y)
    -- Menus paint last and must also win hit-testing over metadata/next controls.
    for i=#S.buttons,1,-1 do
        local b=S.buttons[i]
        if x >= b.x and x <= b.x+b.w and y >= b.y and y <= b.y+b.h then return b end
    end
end
local function coords()
    local x, y = mp.get_mouse_pos()
    local w, h = mp.get_osd_size()
    if not w or not h or w <= 0 or h <= 0 then return -1, -1 end
    return x*W/w, y*H/h
end
local function bind_mouse(wanted)
    if wanted == S.mouse_bound then return end
    S.mouse_bound = wanted
    if wanted then
        mp.add_forced_key_binding('MBTN_LEFT', 'cinema-click', click, {complex=true})
        mp.add_forced_key_binding('MBTN_LEFT_DBL', 'cinema-double', function() end)
    else
        mp.remove_key_binding('cinema-click'); mp.remove_key_binding('cinema-double')
    end
end
render = function()
    local w, h = mp.get_osd_size()
    if not w or not h or w <= 0 or h <= 0 then return end
    -- Fixed logical height, with a minimum width so controls wrap on narrow windows.
    H = 720; W = math.max(560, H*w/h)
    overlay.res_x, overlay.res_y, overlay.z = W, H, 1000
    S.buttons = {}
    if not S.shown then overlay:remove(); bind_mouse(false); return end
    local a = assdraw.ass_new()
    for n=0,23 do
        draw_rect(a, 0, n*4, W, 4, '000000', math.floor(65+n*190/24))
        draw_rect(a, 0, H-(n+1)*6, W, 6, '000000', math.floor(40+n*215/24))
    end
    local movie_title = title()
    -- Clip long or hostile file titles to the title region (ASS escapes above).
    a:new_event(); a:append(string.format('{\\an7\\pos(24,22)\\fnDejaVu Sans\\fs20\\bord0\\shad0\\clip(24,0,%g,65)}', W-235)); a:append(escape(movie_title))
    button(a, 'info', W-216, 16, 148, 'Playback info', function() toggle('info') end, S.menu=='info')
    button(a, 'close', W-60, 16, 40, '×', function() mp.commandv('quit') end)
    local d, pos = duration(), S.preview or position()
    local left, right, y = 90, W-90, H-86
    text(a, 24, y, time(pos), 16, nil, 4); text(a, W-24, y, time(d), 16, nil, 6)
    draw_rect(a, left, y-2, right-left, 4, 'FFFFFF', 170)
    local f = film(); local origin = f and f.origin or 0
    local cache = prop('demuxer-cache-state', {})
    if d > 0 then
        for _, range in ipairs(cache['seekable-ranges'] or {}) do
            if number(range.start) and number(range['end']) then
                local x1 = left+(right-left)*clamp((origin+range.start)/d,0,1)
                local x2 = left+(right-left)*clamp((origin+range['end'])/d,0,1)
                draw_rect(a, x1, y-2, math.max(0,x2-x1), 4, 'FFFFFF', 95)
            end
        end
        local px = left+(right-left)*clamp(pos/d,0,1)
        draw_rect(a, left, y-2, px-left, 4, accent)
        draw_rect(a, px-5, y-6, 10, 12, 'FFFFFF')
        S.buttons[#S.buttons+1] = {id='timeline', x=left, y=y-16, w=right-left, h=32}
    end
    local by = H-57
    button(a, 'back30', 20, by, 48, '−30', function() seek(position()-30) end)
    button(a, 'back10', 74, by, 48, '−10', function() seek(position()-10) end)
    button(a, 'play', 128, by, 64, prop('pause',false) and '▶' or 'Ⅱ', play_pause)
    button(a, 'forward10', 198, by, 48, '+10', function() seek(position()+10) end)
    button(a, 'forward30', 252, by, 48, '+30', function() seek(position()+30) end)
    local compact = W < 900
    local tools_y = compact and H-145 or by
    local x = W-542
    button(a, 'audio', x, tools_y, 86, 'Audio', function() toggle('audio') end, S.menu=='audio')
    button(a, 'sub', x+94, tools_y, 46, 'CC', function() toggle('sub') end, prop('sid') ~= false and prop('sid') ~= 'no' and prop('sid') ~= nil)
    button(a, 'quality', x+148, tools_y, 88, 'Quality', function() toggle('quality') end, S.menu=='quality')
    -- The mode is the requested selection; resolution comes only from mpv's
    -- decoded video properties, never from the requested encoding rung.
    local q=cinema().quality
    local selection=type(q)=='table' and q.selected or 'auto'
    local mode=selection=='auto' and 'Auto' or selection=='original' and 'Original' or 'Manual'
    local p=prop('video-params',{})
    local actual=number(p.h) and p.h>0 and (tostring(p.h)..'p') or '—'
    text(a,x+192,tools_y-9,mode..' · '..actual,12,'D4D4D4',2)
    button(a, 'settings', x+244, tools_y, 102, 'Settings', function() toggle('settings') end, S.menu=='settings')
    button(a, 'toolbar-info', x+354, tools_y, 64, 'Info', function() toggle('info') end, S.menu=='info')
    button(a, 'fullscreen', x+426, tools_y, 96, 'Fullscreen', function() mp.commandv('cycle','fullscreen') end)
    local m=metadata()
    local strip_y=compact and H-302 or H-180
    local strip_right=compact and W-24 or math.max(200,W-390)
    a:new_event(); a:append(string.format('{\\an7\\pos(24,%g)\\fnDejaVu Sans\\fs23\\bord0\\shad0\\clip(24,%g,%g,%g)}',strip_y,strip_y,strip_right,strip_y+32)); a:append(escape(title()))
    a:new_event(); a:append(string.format('{\\an7\\pos(24,%g)\\fnDejaVu Sans\\fs15\\bord0\\shad0\\1c&HD4D4D4&\\clip(24,%g,%g,%g)}',strip_y+34,strip_y+34,strip_right,strip_y+60)); a:append(escape(metadata_details(m)))
    local next=next_episode()
    if next then
        local nx,ny,nw=W-344,compact and H-220 or H-180,320
        local hot=S.hover=='next' or S.focus=='next'
        draw_rect(a,nx,ny,nw,58,hot and '3C322E' or '1A1412',35)
        text(a,nx+14,ny+8,busy() and 'Updating playback…' or 'Next episode',14,accent)
        a:new_event(); a:append(string.format('{\\an7\\pos(%g,%g)\\fnDejaVu Sans\\fs16\\bord0\\shad0\\clip(%g,%g,%g,%g)}',nx+14,ny+30,nx+12,ny+28,nx+nw-12,ny+56)); a:append(escape(clean(next.metadata.title)))
        if S.focus=='next' then draw_rect(a,nx,ny+56,nw,2,accent) end
        S.buttons[#S.buttons+1]={id='next',x=nx,y=ny,w=nw,h=58,action=function() request('cinema-next') end}
    end
    local status=clean(cinema().error)
    local retryable=cinema().retryable == true
    if retryable and status=='' then status='Playback ended before the film finished.' end
    if busy() then status='Updating playback…' end
    if status~='' then
        local sw=math.min(660,W-48)
        local lines=wrapped(status,math.floor((sw-24)/9))
        local sh=20+math.min(#lines,3)*24
        draw_rect(a,(W-sw)/2,82,sw,sh,'1A1412',12)
        for i=1,math.min(#lines,3) do text(a,(W-sw)/2+12,90+(i-1)*24,lines[i],16) end
        if retryable then
            button(a,'retry',(W-112)/2,82+sh+8,112,'Retry',function() request('cinema-retry') end,true)
        end
    end
    if S.hover then
        for _, b in ipairs(S.buttons) do
            if b.id == S.hover and b.id ~= 'timeline' then
                local hints = {back30='Back 30 seconds',back10='Back 10 seconds',play='Play / pause',forward10='Forward 10 seconds',forward30='Forward 30 seconds'}
                if hints[b.id] then text(a,b.x+b.w/2,b.y-14,hints[b.id],14,'FFFFFF',2) end
            end
        end
    end
    if S.preview then text(a, W/2, H-117, time(S.preview)..'  ·  Enter to seek / Esc to cancel', 16, nil, 2) end
    if S.menu then
        local items = rows()
        S.row = clamp(S.row, 1, math.max(1,#items))
        local visible = math.min(#items, 9)
        local first = clamp(S.row-4,1,math.max(1,#items-visible+1))
        local mw, mh = math.min(S.menu=='info' and 680 or 460,W-48), 62+visible*36
        local mx, my = W-mw-24, math.min(tools_y-16,H-116)-mh
        draw_rect(a,mx,my,mw,mh,'1A1412',12)
        S.buttons[#S.buttons+1]={id='menu-panel',x=mx,y=my,w=mw,h=mh,action=function() end}
        local titles = {audio='Audio tracks',sub='Subtitles',quality='Playback quality',settings='Playback settings',info='Playback info'}
        text(a,mx+16,my+15,titles[S.menu],20)
        for n=first,first+visible-1 do
            local row, ry = items[n], my+54+(n-first)*36
            local id = 'row'..n
            if n == S.row then draw_rect(a,mx+8,ry,mw-16,34,'3C322E',20) end
            -- A row is clipped to its panel so long track names cannot cover controls.
            a:new_event(); a:append(string.format('{\\an7\\pos(%g,%g)\\fnDejaVu Sans\\fs16\\bord0\\shad0\\1c&H%s&\\clip(%g,%g,%g,%g)}',mx+16,ry+7,row.selected and accent or 'FFFFFF',mx+12,ry,mx+mw-12,ry+34)); a:append(escape((row.selected and '✓  ' or '')..row.text))
            S.buttons[#S.buttons+1] = {id=id,x=mx,y=ry,w=mw,h=34,action=function() if row.action then row.action(); S.menu=nil end; activity() end}
        end
        if #items > visible then text(a,mx+mw-14,my+22,string.format('%d / %d',S.row,#items),13,nil,6) end
    end
    overlay.data = a.text; overlay:update()
    local mx,my = coords(); bind_mouse(S.dragging or hit(mx,my) ~= nil)
end
activity = function()
    S.shown=true; S.touched=mp.get_time(); render()
end
local function preview_pointer()
    local x = coords()
    if duration() > 0 then S.preview=clamp((x-90)/(W-180),0,1)*duration() end
end
mouse = function()
    local x,y = coords()
    if S.dragging then preview_pointer() end
    S.hover=nil
    activity()
    local b = hit(x,y)
    if b then S.hover=b.id end
    render()
end
click = function(event)
    local x,y = coords(); local b = hit(x,y)
    if event.canceled then
        S.dragging=false; S.preview=nil; S.pressed=nil
    elseif event.event == 'press' then
        if b and b.id == 'timeline' then
            preview_pointer(); local target=S.preview; S.preview=nil; seek(target)
        elseif b and b.action then b.action() end
    elseif event.event == 'down' then
        if b and b.id == 'timeline' then S.dragging=true; preview_pointer()
        else S.pressed = b and b.id end
    elseif event.event == 'up' then
        if S.dragging then
            preview_pointer(); local target=S.preview; S.dragging=false; S.preview=nil; seek(target)
        elseif b and b.id == S.pressed and b.action then b.action() end
        S.pressed=nil
    end
    activity()
end
key = function(name)
    if name == 'escape' then
        if S.preview then S.preview=nil; S.dragging=false
        elseif S.menu then S.menu=nil
        elseif prop('fullscreen',false) then mp.commandv('set','fullscreen','no')
        else mp.commandv('quit') end
    elseif name == 'tab' or name == 'backtab' then
        if S.menu then
            local n = #rows()
            if n > 0 then S.row=(S.row+(name=='tab' and 0 or -2))%n+1 end
            activity(); return
        end
        local i = 0
        for n,b in ipairs(S.buttons) do if b.id == S.focus then i=n end end
        local n=#S.buttons
        if n>0 then S.focus=S.buttons[(i+(name=='tab' and 0 or -2))%n+1].id end
    elseif name == 'enter' then
        if S.preview then local target=S.preview; S.preview=nil; seek(target)
        elseif S.menu then local r=rows()[S.row]; if r and r.action then r.action(); S.menu=nil end
        elseif S.focus == 'timeline' then play_pause()
        else for _,b in ipairs(S.buttons) do if b.id==S.focus and b.action then b.action(); break end end end
    elseif name=='up' or name=='down' then
        if S.menu then S.row=clamp(S.row+(name=='up' and -1 or 1),1,#rows())
        end
    elseif name=='left' or name=='right' then
        if S.menu then
            S.row=clamp(S.row+(name=='left' and -1 or 1),1,#rows())
        elseif S.focus == 'timeline' then
            S.preview=clamp((S.preview or position())+(name=='left' and -10 or 10),0,math.max(0,duration()))
        else
            seek(position()+(name=='left' and -10 or 10))
        end
    end
    activity()
end
mp.add_key_binding('MBTN_LEFT','cinema-surface',function()
    if S.menu then S.menu=nil; activity() else play_pause() end
end)
mp.add_forced_key_binding('SPACE','cinema-pause',play_pause)
mp.add_key_binding('mouse_move','cinema-move',mouse)
mp.add_key_binding('mouse_leave','cinema-leave',function() S.hover=nil; if not S.dragging then bind_mouse(false) end end)
for _, pair in ipairs({{'TAB','tab'},{'Shift+TAB','backtab'},{'ENTER','enter'},{'ESC','escape'},{'LEFT','left'},{'RIGHT','right'},{'UP','up'},{'DOWN','down'}}) do
    local name=pair[2]
    mp.add_forced_key_binding(pair[1],'cinema-'..name,function() key(name) end,{repeatable=name=='left' or name=='right' or name=='up' or name=='down'})
end
mp.add_forced_key_binding('WHEEL_UP','cinema-wheel-up',function() if S.menu then key('up') else mp.commandv('add','volume',5); activity() end end)
mp.add_forced_key_binding('WHEEL_DOWN','cinema-wheel-down',function() if S.menu then key('down') else mp.commandv('add','volume',-5); activity() end end)
for _, name in ipairs({'time-pos','duration','pause','track-list','sid','aid','fullscreen','osd-dimensions','user-data/osc/film','force-media-title','demuxer-cache-state','speed','audio-delay','video-params','video-target-params'}) do
    mp.observe_property(name,'native',function() render() end)
end
mp.observe_property('user-data/cinema','native',function()
    local retryable=cinema().retryable == true
    -- A stale popup must not cover the recovery action, especially in a
    -- narrow window with a multi-line error. Further updates keep normal input.
    if retryable and not S.retryable then S.menu=nil end
    S.retryable=retryable
    S.request_pending=nil
    activity()
end)
mp.observe_property('pause','bool',function() activity() end)
mp.register_event('file-loaded',function() S.preview=nil; S.dragging=false; S.menu=nil; S.focus=nil; activity() end)
mp.add_periodic_timer(0.2,function()
    -- No diagnostic polling while the panel is closed. Read fresh properties
    -- while open even when playback is paused or notifications are throttled.
    if S.menu=='info' then render() end
    if S.shown and not prop('pause',false) and not S.menu and not S.preview and not S.dragging
            and not busy() and mp.get_time()-S.touched >= opts.hide_ms/1000 then
        S.shown=false; S.focus=nil; render()
    end
end)
mp.register_event('shutdown',function() overlay:remove() end)
activity()
