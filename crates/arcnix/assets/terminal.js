// Locally bundled xterm owns only this surface's child DOM. Rust owns all SSH,
// session identities and queues. No remote scripts, websockets or CDN requests.
(() => {
    let engine = null, scheduled = false, token = 0, pending = null, queue = [];
    const engines = new Map();
    const handledKeys = new WeakSet();
    let terminalMenu = null;
    const palettes = {
        light: {
            background:'#edf1f5', foreground:'#253341', cursor:'#245e94', cursorAccent:'#edf1f5', selectionBackground:'#457bb74d',
            black:'#283340', red:'#a23435', green:'#286438', yellow:'#805711', blue:'#275f9e', magenta:'#823e88', cyan:'#216771', white:'#a8b1ba',
            brightBlack:'#526170', brightRed:'#b4323a', brightGreen:'#237545', brightYellow:'#916415', brightBlue:'#256db3', brightMagenta:'#954b9b', brightCyan:'#147484', brightWhite:'#dce2e8'
        },
        dark: {
            background:'#19212b', foreground:'#dce4ec', cursor:'#8cc4ff', cursorAccent:'#19212b', selectionBackground:'#78a9db55',
            black:'#323e4c', red:'#ee8080', green:'#87c997', yellow:'#dfbc76', blue:'#81ade3', magenta:'#c799d4', cyan:'#79c4ca', white:'#c1ccd6',
            brightBlack:'#8796a6', brightRed:'#ffa2a0', brightGreen:'#a7deb1', brightYellow:'#f3d49a', brightBlue:'#a4caff', brightMagenta:'#dfb6e9', brightCyan:'#9adee2', brightWhite:'#edf3fa'
        }
    };
    function applyPalette(current, surface, dark) {
        const scheme = dark ? 'dark' : 'light';
        const palette = palettes[scheme];
        if (surface.dataset.terminalScheme !== scheme) {
            surface.dataset.terminalScheme = scheme;
            surface.style.setProperty('--terminal-background', palette.background);
            surface.style.setProperty('--terminal-foreground', palette.foreground);
        }
        if (current.theme !== scheme) {
            current.term.options.theme = palette;
            current.theme = scheme;
        }
    }
    function metadata() {
        try { return JSON.parse(document.querySelector(".terminal-state")?.textContent || "null"); }
        catch (_) { return null; }
    }
    function dispatch() {
        const state=metadata();
        if(!state)return;
        if(pending && pending.epoch===state.epoch && pending.session===state.id && state.input_ack>=pending.token)pending=null;
        if(!pending)pending=queue.shift()||null;
        if(!pending)return;
        if(pending.epoch!==state.epoch||pending.session!==state.id)return;
        const input=document.querySelector(".terminal-input-bridge");
        if(!input)return;
        input.value=JSON.stringify(pending);
        input.dispatchEvent(new Event("input",{bubbles:true}));
    }
    function emit(value) {
        const state=metadata();
        if(!state)return;
        const epoch=engine?.epoch ?? state.epoch, session=engine?.id ?? state.id;
        token=Math.max(token,state.input_ack||0);
        const previous=queue[queue.length-1];
        if(value.kind==="data"&&previous?.kind==="data"&&previous.id===value.id&&previous.epoch===epoch&&previous.data.length+value.data.length<=2048){previous.data+=value.data;return;}
        queue.push({...value,epoch,session,token:++token});
        if(!pending)dispatch();
    }
    setInterval(()=>{if(pending||queue.length)dispatch();},80);
    function data(text, id) {
        // Do not split surrogate pairs or exceed the native input packet budget.
        let chunk = "";
        for (const character of text) {
            if (chunk.length + character.length > 2048) { emit({kind:"data",id,data:chunk}); chunk=""; }
            chunk += character;
        }
        if (chunk) emit({kind:"data",id,data:chunk});
    }
    function fit() {
        if (engine?.surface.getClientRects().length) {
            try { engine.fit.fit(); } catch (_) { /* zero-sized view during layout */ }
        }
    }
    function detach() {
        if (!engine) { pending=null;queue=[];return; }
        // Only raw terminal bytes wait for their original tab to become active
        // again. UI operations must not replay after switching sessions.
        engine.outbound.push(...[pending,...queue].filter(packet=>packet &&
            packet.epoch===engine.epoch && packet.session===engine.id &&
            (packet.kind==="data" || packet.kind==="binary")));
        pending=null;queue=[];
        engine.resize.disconnect();
        engine.wrapper.remove();
        engine.visible=false;
        engine.active=false;
        // A consumption acknowledgement may still have been queued when the
        // tab changed. Repeating it on return is safe; dropping it is not.
        engine.sentAckPacket=0;
        engine.commandAck=0;
        engine=null;
    }
    function dispose(current) {
        if (engine===current) detach();
        current.resize.disconnect();
        current.term.dispose();
        current.wrapper.remove();
        engines.delete(current.key);
    }
    function consumed(current) {
        if (engine!==current || current.ackPacket<=current.sentAckPacket) return;
        current.sentAckPacket=current.ackPacket;
        emit({kind:"consumed",id:current.id,packet:current.ackPacket});
    }
    function make(metadata, surface) {
        const wrapper = document.createElement("div");
        wrapper.className="terminal-owned-surface";
        surface.appendChild(wrapper);
        const term = new Terminal({fontFamily:metadata.font_family,fontSize:metadata.font_size,cursorStyle:metadata.cursor_style,cursorBlink:metadata.cursor_blink,scrollback:5000,convertEol:false,screenReaderMode:true,allowProposedApi:false,disableStdin:true,rightClickSelectsWord:false,linkHandler:{activate:()=>{}}});
        const addon = new FitAddon.FitAddon();
        term.loadAddon(addon);
        term.open(wrapper);
        term.onRender(() => {
            wrapper.classList.toggle("has-scrollback", term.buffer.active.baseY > 0);
        });
        term.parser.registerOscHandler(52,()=>true); // Remote output cannot write the clipboard.
        const id=metadata.id, epoch=metadata.epoch,key=`${epoch}:${id}`;
        const current={id,epoch,key,term,fit:addon,wrapper,surface,lastPacket:0,ackPacket:0,sentAckPacket:0,clear:metadata.clear,paste:0,visible:false,status:null,selected:false,outbound:[],commandRevision:0,commandAck:0};
        current.resize=new ResizeObserver(()=>{if(engine?.key===key)fit();});
        current.resize.observe(surface);
        engines.set(key,current);
        term.onData(text=>{if(engine?.key===key && engine.status==="running") data(text,id);});
        // In screen-reader mode xterm handles keyboard and composition events,
        // but skips text committed by the input service without a key event.
        let keyProducedInput = false;
        term.onKey(() => { keyProducedInput = true; });
        for (const type of ['keydown', 'keyup', 'blur']) {
            wrapper.addEventListener(type, () => { keyProducedInput = false; }, true);
        }
        term.textarea.addEventListener('beforeinput', event => {
            if (event.inputType !== 'insertText' || event.isComposing || !event.data ||
                !event.cancelable || engine !== current || current.status !== 'running') return;
            if (keyProducedInput) {
                keyProducedInput = false;
                return;
            }
            // Prevent a second delivery from xterm's deferred keyCode 229 textarea check.
            event.preventDefault();
            term.input(event.data);
        });
        term.onBinary(text=>{if(engine?.key===key && engine.status==="running") emit({kind:"binary",id,data:btoa(text)});});
        term.onResize(size=>{if(engine?.key===key)emit({kind:"resize",id,columns:size.cols,rows:size.rows});});
        term.onSelectionChange(()=>{
            if (engine?.key!==key) return;
            const selected=term.hasSelection();
            if(selected!==engine.selected){engine.selected=selected;emit({kind:"selection",id,selected});}
        });
        term.attachCustomKeyEventHandler(event=>{
            if(engine?.key!==key)return false;
            if(event.type!=="keydown" || event.isComposing || event.keyCode===229) return true;
            if(applicationKey(event))return false;
            if (arcnixKeys.matches(event, 'terminal_copy', 'Ctrl+Shift+C')) {
                event.preventDefault(); copy(); return false;
            }
            if (arcnixKeys.matches(event, 'terminal_interrupt', 'Ctrl+C')) {
                event.preventDefault();
                if (term.hasSelection()) copy();
                else term.input('\x03');
                return false;
            }
            if (matchesPaste(event)) {
                event.preventDefault(); emit({kind:'paste', id}); return false;
            }
            // Ctrl+/ is a terminal alias for Ctrl+_ (ASCII unit separator).
            if (event.key === '/' && event.ctrlKey && !event.altKey && !event.metaKey && !event.getModifierState('AltGraph')) {
                event.preventDefault();
                term.input('\x1f');
                return false;
            }
            return true;
        });
        // Escape, Ctrl+K etc. belong to the shell, not Glory's app shortcuts.
        for(const name of ["keydown","keypress","keyup","input","paste"])
            wrapper.addEventListener(name,event=>event.stopPropagation());
        return current;
    }
    function activate(metadata,surface) {
        const key=`${metadata.epoch}:${metadata.id}`;
        if(engine?.key!==key){
            detach();
            engine=engines.get(key)||null;
            if(!engine){
                engine=make(metadata,surface);
                fit();
                emit({kind:"ready",columns:engine.term.cols,rows:engine.term.rows});
            } else if(engine.outbound.length) {
                queue.push(...engine.outbound);
                engine.outbound=[];
            }
        }
        if(engine.surface!==surface || engine.wrapper.parentElement!==surface){
            engine.resize.disconnect();
            engine.surface=surface;
            surface.appendChild(engine.wrapper);
            engine.resize.observe(surface);
            engine.visible=false;
            engine.active=false;
        }
    }
    function copy() {
        if(engine?.term.hasSelection()) emit({kind:"copy",id:engine.id,text:engine.term.getSelection()});
    }
    function matchesPaste(event) {
        if (navigator.platform === 'Win32' && window.arcnixChordMatches(event, 'Ctrl+V')) {
            const layout = arcnixKeys.state();
            const bindings = layout?.bindings;
            // The Windows alias follows the default. Explicit terminal bindings take priority.
            if (!bindings || (layout.preset === 'windows' && bindings.terminal_paste[0] === 'Ctrl+Shift+V'
                && !Object.entries(bindings).some(([id, key]) => id.startsWith('terminal_') && key.includes('Ctrl+V')))) {
                return true;
            }
        }
        return arcnixKeys.matches(event, 'terminal_paste', 'Ctrl+Shift+V');
    }
    function terminalModule(target) {
        const module=target instanceof Element ? (target.closest('.terminal-module') || (target.closest('.terminal-controls') ? document.querySelector('.terminal-module:not(.hidden)') : null)) : null;
        return module && module.getClientRects().length && !module.closest('[inert],[hidden]') ? module : null;
    }
    function terminalTarget(target) {
        if(!(target instanceof Element) || !terminalModule(target))return null;
        return target.closest('.terminal-tab-item') || target.closest('.terminal-surface');
    }
    function openContext(target,event,keyboard=false) {
        const origin=terminalTarget(target),state=metadata();
        if(!origin || !state)return false;
        const tab=origin.matches('.terminal-tab-item');
        const id=tab ? Number(origin.querySelector('.terminal-tab-id')?.textContent) : state.id;
        if(!Number.isSafeInteger(id) || id<0)return false;
        const selection=!tab && engine?.key===`${state.epoch}:${state.id}` ? engine.term.getSelection() : '';
        const tooLarge=new TextEncoder().encode(selection).length>65536;
        const box=origin.getBoundingClientRect();
        let x=keyboard ? box.left+16 : event.clientX, y=keyboard ? box.bottom : event.clientY;
        if(keyboard && !tab && engine){
            const screen=origin.querySelector('.xterm-screen')?.getBoundingClientRect() || box;
            const cursor=engine.term.buffer.active;
            x=screen.left+(cursor.cursorX+.5)*screen.width/engine.term.cols;
            y=screen.top+(cursor.cursorY+1)*screen.height/engine.term.rows;
        }
        terminalMenu={x,y,origin:tab ? origin.querySelector('.terminal-tab') : engine?.term.textarea,tab,epoch:state.epoch,sourceId:state.id,id,opened:false,focus:true,restore:true};
        emit({kind:'context',id,selection:tooLarge?'':selection,too_large:tooLarge,tab});
        schedule();
        return true;
    }
    function dismissContext(restore=true) {
        if(terminalMenu)terminalMenu.restore=restore;
        document.querySelector('.terminal-menu-dismiss')?.click();
        schedule();
    }
    function restoreContextFocus(previous) {
        if(!previous?.restore || document.querySelector('[role=dialog]'))return;
        const state=metadata();
        if(!state || state.epoch!==previous.epoch)return;
        if(previous.tab && previous.sourceId===state.id && previous.origin?.isConnected){
            previous.origin.focus({preventScroll:true});
        } else if(engine?.surface.getClientRects().length){
            engine.term.focus();
        }
    }
    function placeContext() {
        if(!terminalMenu)return;
        const menu=document.querySelector('.terminal-context-menu');
        if(!menu || !menu.getClientRects().length){
            if(terminalMenu.opened){const previous=terminalMenu;terminalMenu=null;restoreContextFocus(previous);}
            return;
        }
        terminalMenu.opened=true;
        const frame=desktopFrame.boundsFor(menu);
        const box=menu.getBoundingClientRect();
        menu.style.left=`${Math.max(frame.left+8,Math.min(terminalMenu.x,frame.right-box.width-8))}px`;
        menu.style.top=`${Math.max(frame.top+8,Math.min(terminalMenu.y,frame.bottom-box.height-8))}px`;
        if(terminalMenu.focus){
            const first=menu.querySelector('.terminal-menu-action:not(:disabled)');
            if(first)first.focus({preventScroll:true});else menu.focus({preventScroll:true});
            terminalMenu.focus=false;
        }
    }
    function applicationKey(event) {
        if(handledKeys.has(event))return true;
        if(event.type!=='keydown' || event.isComposing || event.keyCode===229 || !(event.target instanceof Element))return false;
        if(terminalMenu && event.key==='Escape' && document.querySelector('.terminal-context-menu')){
            dismissContext(true);
            handledKeys.add(event);event.preventDefault();event.stopImmediatePropagation();return true;
        }
        // Glory's capture listener also handles Dismiss. Deliver Escape before
        // it reaches that listener when the remote terminal owns the focus.
        if(event.key==='Escape' && !event.ctrlKey && !event.metaKey && !event.altKey
            && !event.shiftKey && engine?.wrapper.contains(event.target)){
            engine.term.input('\x1b');
            handledKeys.add(event);event.preventDefault();event.stopImmediatePropagation();return true;
        }
        if(event.target.closest('[role=dialog]'))return false;
        const menu=event.target.closest('.terminal-context-menu');
        if(!menu && !terminalModule(event.target))return false;
        const copyKey = arcnixKeys.matches(event, 'terminal_copy', 'Ctrl+Shift+C');
        const pasteKey = matchesPaste(event);
        const newTab = arcnixKeys.matches(event, 'terminal_new_tab', 'Ctrl+Shift+T');
        const closeTab = arcnixKeys.matches(event, 'terminal_close_tab', 'Ctrl+Shift+W');
        const context = event.key === 'ContextMenu' || arcnixKeys.matches(event, 'terminal_context', 'Shift+F10');
        if(menu){
            if(!event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey && ['ArrowDown','ArrowUp','Home','End'].includes(event.key)){
                const items=[...menu.querySelectorAll('.terminal-menu-action:not(:disabled)')].filter(item=>item.getClientRects().length);
                const index=items.indexOf(event.target.closest('.terminal-menu-action'));
                const next=event.key==='Home'?0:event.key==='End'?items.length-1:index<0?(event.key==='ArrowUp'?items.length-1:0):(index+(event.key==='ArrowUp'?-1:1)+items.length)%items.length;
                items[next]?.focus({preventScroll:true});
            } else if(event.key==='Escape'||event.key==='Tab'){
                dismissContext(true);
            } else if(copyKey || pasteKey){
                menu.querySelector(copyKey?'.terminal-menu-copy:not(:disabled)':'.terminal-menu-paste:not(:disabled)')?.click();
            } else if(newTab || closeTab){
                menu.querySelector(newTab?'.terminal-menu-new-tab:not(:disabled)':'.terminal-menu-close-tab:not(:disabled)')?.click();
            } else if(context){
                // The existing application menu already owns this gesture.
            } else {
                return shortcutKey(event);
            }
        } else if(context){
            if(!openContext(event.target,event,true))return false;
        } else {
            return shortcutKey(event);
        }
        handledKeys.add(event);event.preventDefault();event.stopImmediatePropagation();return true;
    }
    const terminalCommands = [
        ['terminal_next_tab', 'Ctrl+Tab', 'next_tab'],
        ['terminal_previous_tab', 'Ctrl+Shift+Tab', 'previous_tab'],
        ['terminal_new_tab', 'Ctrl+Shift+T', 'new_tab'],
        ['terminal_close_tab', 'Ctrl+Shift+W', 'close_tab'],
        ['terminal_select_all', 'Ctrl+Shift+A', 'select_all'],
        ['terminal_clear', 'Ctrl+Shift+L', 'clear'],
    ];
    function shortcutKey(event) {
        const action = terminalCommands.find(([id, fallback]) => arcnixKeys.matches(event, id, fallback))?.[2];
        if(!action)return false;
        const state=metadata();
        if(!state)return false;
        emit({kind:'shortcut',id:state.id,action});
        handledKeys.add(event);event.preventDefault();event.stopImmediatePropagation();return true;
    }
    document.addEventListener('contextmenu',event=>{
        let target=event.target;
        if(target instanceof Element && target.closest('.terminal-menu-dismiss')){
            target=document.elementsFromPoint(event.clientX,event.clientY).find(item=>terminalTarget(item));
        }
        if(terminalTarget(target)){
            event.preventDefault();event.stopImmediatePropagation();
            openContext(target,event);
        } else if(target instanceof Element && target.closest('.terminal-context-menu')){
            event.preventDefault();event.stopImmediatePropagation();
        }
    },true);
    // Keep right-click from changing xterm's selection or sending a mouse event
    // to the remote foreground program before the custom menu snapshots it.
    document.addEventListener('mousedown',event=>{
        if(event.button===2 && terminalTarget(event.target)){
            event.preventDefault();event.stopImmediatePropagation();
        }
    },true);
    document.addEventListener('keydown',applicationKey,true);
    document.addEventListener('wheel',event=>{
        if(terminalMenu?.opened && terminalTarget(event.target))dismissContext(false);
    },true);
    window.addEventListener('resize',()=>{if(terminalMenu?.opened)dismissContext(false);});
    document.addEventListener("click",event=>{
        if(!(event.target instanceof Element))return;
        const overflow=event.target.closest('.terminal-overflow');
        if(overflow){
            const surface=document.querySelector('.terminal-surface');
            const box=overflow.getBoundingClientRect();
            if(surface && openContext(surface,{clientX:box.right,clientY:box.bottom},false)){event.preventDefault();event.stopImmediatePropagation();}
            return;
        }
        const copyButton=event.target.closest(".terminal-copy"),pasteButton=event.target.closest(".terminal-paste");
        if(copyButton && !copyButton.disabled){event.preventDefault();event.stopImmediatePropagation();copy();engine?.term.focus();}
        if(pasteButton && !pasteButton.disabled && engine){event.preventDefault();event.stopImmediatePropagation();emit({kind:"paste",id:engine.id});engine.term.focus();}
    },true);
    function renderPackets(current, module, acknowledge) {
        for (const element of module.querySelectorAll('.terminal-packet')) {
            const packet = JSON.parse(element.textContent);
            if (packet.id <= current.lastPacket) continue;
            current.lastPacket = packet.id;
            const bytes = Uint8Array.from(atob(packet.data), c => c.charCodeAt(0));
            current.term.write(bytes, () => {
                if (engines.get(current.key) !== current) return;
                current.ackPacket = Math.max(current.ackPacket, packet.id);
                acknowledge(current);
            });
        }
        acknowledge(current);
    }
    function scan() {
        scheduled=false;
        const source=document.querySelector(".terminal-state"),surface=document.querySelector(".terminal-surface");
        const epochs=JSON.parse(document.querySelector(".terminal-workspace-epochs")?.textContent || "[]");
        for(const current of engines.values())if(!epochs.includes(current.epoch))dispose(current);
        if(!source||!surface){detach();return;}
        let metadata;
        try{metadata=JSON.parse(source.textContent);}catch(_){return;}
        token=Math.max(token,metadata.input_ack||0);
        const sessions=Array.isArray(metadata.sessions)?metadata.sessions:[metadata.id];
        const valid=new Set(sessions.map(id=>`${metadata.epoch}:${id}`));
        for(const current of engines.values())if(current.epoch===metadata.epoch && !valid.has(current.key))dispose(current);
        if(!metadata.id){
            detach();
            placeContext();
            return;
        }
        const visible=!!surface.getClientRects().length;
        if(!visible && !engines.has(`${metadata.epoch}:${metadata.id}`)){detach();return;}
        activate(metadata,surface);
        const options = engine.term.options;
        if (options.fontSize !== metadata.font_size || options.fontFamily !== metadata.font_family) {
            options.fontFamily = metadata.font_family;
            options.fontSize = metadata.font_size;
            fit();
        }
        if (options.cursorStyle !== metadata.cursor_style) options.cursorStyle = metadata.cursor_style;
        if (options.cursorBlink !== metadata.cursor_blink) options.cursorBlink = metadata.cursor_blink;
        for (const parked of document.querySelectorAll('.terminal-parked')) {
            const cached = engines.get(parked.querySelector('.terminal-parked-key').textContent);
            const surface = parked.querySelector('.terminal-parked-surface');
            if (!cached || cached === engine || !parked.getClientRects().length) continue;
            if (cached.wrapper.parentElement !== surface) surface.replaceChildren(cached.wrapper);
            applyPalette(cached, surface, metadata.dark);
            const input = parked.querySelector('.terminal-output-ack');
            renderPackets(cached, parked, current => {
                if (!input.isConnected || current.ackPacket <= current.sentAckPacket) return;
                current.sentAckPacket = current.ackPacket;
                input.value = JSON.stringify([current.epoch, current.id, current.ackPacket]);
                input.dispatchEvent(new Event('input', {bubbles: true}));
            });
        }
        const term=engine.term;
        applyPalette(engine, surface, metadata.dark);
        if(engine.status!==metadata.status){
            engine.status=metadata.status;
            term.options.disableStdin=metadata.status!=="running";
            if(metadata.status==="running"){fit();emit({kind:"resize",id:metadata.id,columns:term.cols,rows:term.rows});}
        }
        const active=visible && Boolean(surface.closest('.desktop-window.active'));
        const focused=document.activeElement;
        if(visible&&!engine.visible)fit();
        if(active&&!engine.active
            && (!engine.visible && !focused?.matches('.desktop-canvas')
                || focused?.closest('.desktop-dock-app.app-terminal,.desktop-shortcut.app-terminal')))term.focus();
        engine.visible=visible;
        engine.active=active;
        if(engine.clear!==metadata.clear){engine.clear=metadata.clear;term.clear();}
        if(metadata.paste_revision>engine.paste){
            engine.paste=metadata.paste_revision;
            if(metadata.paste&&metadata.status==="running")term.paste(metadata.paste);
            emit({kind:"pasted",id:metadata.id,revision:metadata.paste_revision});
        }
        const command=metadata.command;
        if(command && command.id===engine.id && ['select_all','focus'].includes(command.kind)){
            if(command.revision>engine.commandRevision){
                engine.commandRevision=command.revision;
                if(command.kind==='select_all')term.selectAll();
                if(visible && !document.querySelector('[role=dialog]'))term.focus();
            }
            if(command.revision>engine.commandAck){
                engine.commandAck=command.revision;
                emit({kind:'command_done',id:engine.id,revision:command.revision});
            }
        }
        renderPackets(engine, source.closest('.terminal-module'), consumed);
        if(pending&&pending.epoch===metadata.epoch&&pending.session===metadata.id&&metadata.input_ack>=pending.token){pending=null;dispatch();}
        placeContext();
    }
    const schedule=()=>{if(!scheduled){scheduled=true;requestAnimationFrame(scan);}};
    new MutationObserver(records=>{
        if(records.some(record=>!(record.target instanceof Element ? record.target : record.target.parentElement)?.closest(".xterm")))schedule();
    }).observe(document,{subtree:true,childList:true,characterData:true,attributes:true,attributeFilter:["class"]});
    window.addEventListener("resize",schedule);
    schedule();
})();
