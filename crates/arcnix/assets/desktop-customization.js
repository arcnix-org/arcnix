(() => {
    const dragType = 'application/x-arcnix-desktop-icon';
    let drag, marker, focusedMenu, dockMenuAnchor;
    let clockViews = [];
    let clockOptions, clockTimer, clockSample, clockAnchor;
    let timeZonePicker;
    let themeSource, themeTokens;
    const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
    const wallpapers = new WeakMap();
    const sizeControls = new Map();
    const themed = new WeakMap();

    // Only a focus change can mark a launcher. A modifier pressed after a
    // pointer click must not turn the already-focused launcher into a tab stop cue.
    document.addEventListener('focusin', event => {
        const button = event.target.closest?.('.desktop-dock button');
        if (button) button.toggleAttribute('data-keyboard-focus',
            document.documentElement.classList.contains('keyboard-navigation'));
    });
    document.addEventListener('pointerdown', event => {
        event.target.closest?.('.desktop-dock button')?.removeAttribute('data-keyboard-focus');
    }, true);

    function updateWallpaper(desktop, epoch) {
        const art = desktop.querySelector('.desktop-wallpaper-art');
        if (!art) return;
        const source = desktop.querySelector('.desktop-wallpaper-source')?.textContent.trim() || '';
        const custom = /^\/wallpaper-cache\/[a-f0-9]{64}\/[a-f0-9]{64}\/[a-f0-9]{64}\.(png|jpg|jpeg|webp|gif|bmp)$/.test(source) ? source : '';
        const key = art.className + (custom || desktop.classList.contains('desktop-dark'));
        const previous = wallpapers.get(art);
        const reduced = reducedMotion.matches;
        if (previous?.key === key && previous.epoch === epoch && previous.reduced === reduced) return;

        const style = getComputedStyle(art);
        const next = {key, epoch, reduced, background: style.background};
        wallpapers.set(art, next);
        if (!previous || previous.epoch !== epoch || reduced || previous.reduced) {
            if (custom) {
                const image = new Image();
                image.alt = '';
                image.className = 'desktop-wallpaper-image';
                image.src = custom;
                art.replaceChildren(image);
            } else {
                art.replaceChildren();
            }
            return;
        }
        // The base already has the new CSS background. Keep the old frame opaque until decode finishes.
        if (!art.firstElementChild) {
            const base = document.createElement('div');
            base.className = 'wallpaper-transition-layer';
            base.style.background = previous.background;
            art.append(base);
        }
        const image = new Image();
        image.alt = '';
        image.className = custom ? 'wallpaper-transition-layer desktop-wallpaper-image' : 'wallpaper-transition-layer';
        image.style.objectPosition = style.backgroundPosition;
        image.src = custom || style.backgroundImage.slice(5, -2);
        image.decode().then(async () => {
            if (!art.isConnected || wallpapers.get(art) !== next) return;
            art.append(image);
            await Promise.allSettled(image.getAnimations().map(animation => animation.finished));
            // A newer selection may already be fading in above this frame.
            while (image.previousElementSibling) image.previousElementSibling.remove();
            if (!custom && wallpapers.get(art) === next) image.remove();
        }).catch(() => {
            if (wallpapers.get(art) === next) art.replaceChildren();
        });
    }

    reducedMotion.addEventListener('change', updateTheme);
    function updateTheme() {
        const data = document.querySelector('.server-desktop:not(.hidden) .desktop-theme-data');
        if (!data?.textContent) return;
        const source = data.textContent;
        if (source !== themeSource) {
            themeTokens = JSON.parse(source);
            themeSource = source;
        }
        const {colors, icons, iconFamily} = themeTokens;
        const desktop = data.closest('.server-desktop');
        updateWallpaper(desktop, themeTokens.epoch);
        const roots = [desktop, ...document.querySelectorAll('.desktop-modal-backdrop')];
        for (const root of roots) {
            const old = themed.get(root);
            if (old?.source === source) continue;
            for (const key of old?.keys || []) root.style.removeProperty(key);
            for (const [key, value] of colors) root.style.setProperty(key, value);
            root.dataset.desktopIconStyle = icons;
            root.dataset.desktopIconFamily = iconFamily;
            themed.set(root, {source, keys: colors.map(([key]) => key)});
        }
    }

    function canvasFor(target) {
        if (!(target instanceof Element) || target.closest('[inert],[hidden],.hidden')) return null;
        return target.closest('.desktop-canvas');
    }

    function epoch(canvas) {
        return JSON.parse(canvas.closest('.server-desktop').querySelector('.desktop-motion-state').textContent).epoch;
    }

    function send(canvas, payload) {
        const bridge = canvas.closest('.server-desktop')?.querySelector('.desktop-customization-input');
        if (!bridge) return;
        bridge.value = JSON.stringify({epoch: epoch(canvas), ...payload});
        bridge.dispatchEvent(new Event('input', {bubbles: true}));
    }

    function zone(target) {
        return target.closest('.desktop-dock') ? 'dock' : 'desktop';
    }

    function shortcut(slot) {
        const key = slot?.querySelector('.desktop-item-key');
        return key ? JSON.parse(key.textContent) : null;
    }

    // Pointer clicks select a desktop icon. Keep the native button activation for
    // keyboards, assistive technology and the system's double-click gesture.
    document.addEventListener('click', event => {
        const icon = event.target.closest?.('.desktop-shortcuts .desktop-shortcut');
        if (!icon || event.button !== 0 || event.detail === 0) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        icon.focus({preventScroll: true});
    }, true);

    document.addEventListener('dblclick', event => {
        const icon = event.target.closest?.('.desktop-shortcuts .desktop-shortcut');
        if (!icon || event.button !== 0) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        icon.click();
    }, true);

    document.addEventListener('click', event => {
        const button = event.target.closest?.('.desktop-dock-app.active.running');
        if (!button || event.button || event.detail !== 1 || event.altKey || event.ctrlKey ||
            event.metaKey || event.shiftKey) return;
        const canvas = canvasFor(button);
        const app = shortcut(button.closest('.desktop-icon-slot'));
        if (!canvas || app?.kind !== 'app') return;
        const state = JSON.parse(canvas.closest('.server-desktop').querySelector('.desktop-motion-state').textContent);
        if (!state.activeClickOverview) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        const exit = canvas.querySelector('.overview-exit');
        if (exit) exit.click();
        else desktopMotion.openOverview(() => send(canvas, {kind: 'dock_overview', app: app.id}));
    }, true);

    function openContextMenu(canvas, target, x, y) {
        const slot = target.closest('.desktop-icon-slot');
        const menuZone = zone(target);
        dockMenuAnchor = menuZone === 'dock'
            ? slot?.querySelector('.desktop-dock-app') ?? target.closest('.desktop-dock-home')
            : null;
        const frame = canvas.getBoundingClientRect();
        send(canvas, {kind: 'context', zone: menuZone, item: shortcut(slot),
            x: Math.round(x - frame.left), y: Math.round(y - frame.top)});
    }

    function clearMarker() {
        marker?.removeAttribute('data-desktop-drop');
        marker?.removeAttribute('data-files-item-target');
        marker = null;
    }

    function clearDrag() {
        clearMarker();
        drag?.slot.removeAttribute('data-desktop-dragging');
        drag = null;
    }

    function dropTarget(event) {
        const canvas = canvasFor(event.target);
        const slot = event.target.closest?.('.desktop-icon-slot');
        if (canvas !== drag.canvas || epoch(canvas) !== drag.epoch || !slot || slot === drag.slot || zone(slot) !== drag.zone) return null;
        const rect = slot.getBoundingClientRect();
        const row = slot.closest('.icons-by-row');
        const after = (drag.zone === 'desktop' && !row) || slot.closest('.dock-side-zone')
            ? event.clientY >= rect.top + rect.height / 2
            : row?.classList.contains('icons-from-right')
                ? event.clientX <= rect.left + rect.width / 2
                : event.clientX >= rect.left + rect.width / 2;
        return {slot, after};
    }

    function editorTarget(event) {
        const canvas = canvasFor(event.target);
        return drag.editorFile && canvas === drag.canvas && epoch(canvas) === drag.epoch
            ? event.target.closest('.editor-file-drop') : null;
    }

    document.addEventListener('dragstart', event => {
        const slot = event.target.closest?.('.desktop-icon-slot');
        const canvas = canvasFor(slot);
        if (!canvas || !event.dataTransfer) return;
        clearDrag();
        drag = {slot, canvas, epoch: epoch(canvas), zone: zone(slot), item: shortcut(slot)};
        drag.editorFile = drag.item?.kind === 'file' && !!slot.querySelector('.desktop-file.app-document');
        event.dataTransfer.effectAllowed = drag.editorFile ? 'copyMove' : 'move';
        event.dataTransfer.setData(dragType, String(drag.epoch));
        slot.setAttribute('data-desktop-dragging', '');
    }, true);

    document.addEventListener('dragover', event => {
        if (!drag) return;
        clearMarker();
        event.preventDefault();
        const editor = editorTarget(event);
        if (editor) {
            event.dataTransfer.dropEffect = 'copy';
            marker = editor;
            marker.setAttribute('data-files-item-target', '');
            return;
        }
        const target = dropTarget(event);
        event.dataTransfer.dropEffect = target ? 'move' : 'none';
        if (!target) return;
        marker = target.slot;
        marker.setAttribute('data-desktop-drop', target.after ? 'after' : 'before');
    }, true);

    document.addEventListener('drop', event => {
        if (!drag) return;
        event.preventDefault();
        const target = dropTarget(event);
        if (event.dataTransfer.getData(dragType) === String(drag.epoch)) {
            const editor = editorTarget(event);
            if (editor) {
                send(drag.canvas, {kind: 'open_in_text_editor', name: drag.item.id, window: Number(editor.querySelector('.editor-window-id').textContent)});
            } else if (target) {
                send(drag.canvas, {kind: 'reorder', zone: drag.zone, item: drag.item, anchor: shortcut(target.slot), after: target.after});
            }
        }
        clearDrag();
    }, true);
    document.addEventListener('dragend', clearDrag, true);
    window.addEventListener('blur', clearDrag);

    document.addEventListener('wheel', event => {
        const grid = event.target.closest?.('.desktop-shortcuts');
        if (!grid || event.ctrlKey || event.deltaX || !event.deltaY || grid.scrollWidth <= grid.clientWidth) return;
        const scale = event.deltaMode === 1 ? 20 : event.deltaMode === 2 ? grid.clientWidth : 1;
        grid.scrollLeft += event.deltaY * scale * (grid.classList.contains('icons-from-right') ? -1 : 1);
        event.preventDefault();
    }, {passive: false});

    document.addEventListener('contextmenu', event => {
        if (!(event.target instanceof Element) || !event.target.closest('.server-desktop,.desktop-modal-backdrop')) return;
        event.preventDefault();
        const canvas = canvasFor(event.target);
        if (!canvas || event.target.closest('.desktop-window,.desktop-context-menu')) return;
        const target = event.target.closest('.desktop-menu-backdrop')
            ? document.elementsFromPoint(event.clientX,event.clientY).find(element => element !== event.target && canvas.contains(element))
            : event.target;
        if (!target || target.closest('.desktop-window')) { send(canvas, {kind: 'dismiss'}); return; }
        openContextMenu(canvas, target, event.clientX, event.clientY);
    }, true);

    document.addEventListener('keydown', event => {
        const desktopFile = event.target.closest?.('.desktop-shortcut.desktop-file');
        if (desktopFile && event.key === 'Delete' && !event.ctrlKey && !event.metaKey && !event.altKey) {
            const canvas = canvasFor(desktopFile);
            const item = shortcut(desktopFile.closest('.desktop-icon-slot'));
            if (canvas && item?.kind === 'file') {
                event.preventDefault(); event.stopImmediatePropagation();
                if (!event.repeat) send(canvas, {kind:'remove_file', name:item.id, permanent:event.shiftKey});
                return;
            }
        }
        const menu = document.querySelector('.server-desktop:not(.hidden) .desktop-context-menu');
        if (!menu) {
            const slot = event.target.closest?.('.desktop-icon-slot');
            const canvas = canvasFor(slot);
            if (!canvas || event.ctrlKey || event.altKey || event.metaKey
                || (event.key !== 'ContextMenu' && !(event.shiftKey && event.key === 'F10'))) return;
            event.preventDefault();
            event.stopImmediatePropagation();
            if (event.repeat) return;
            const anchor = event.target.getBoundingClientRect();
            openContextMenu(canvas, event.target, anchor.left, anchor.bottom);
            return;
        }
        const palette = event.target.closest?.('.file-color-menu');
        const horizontal = palette && ['ArrowLeft', 'ArrowRight'].includes(event.key);
        if (event.key === 'Escape') {
            send(menu.closest('.desktop-canvas'), {kind: 'dismiss'});
        } else if (menu.contains(event.target) && (horizontal || ['ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key))) {
            const buttons = [...(horizontal ? palette : menu).querySelectorAll('button:not(:disabled)')];
            if (!buttons.length) return;
            const index = buttons.indexOf(document.activeElement);
            const back = event.key === 'ArrowUp' || event.key === 'ArrowLeft';
            const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1
                : (index + (back ? -1 : 1) + buttons.length) % buttons.length;
            buttons[next].focus();
        } else return;
        event.preventDefault();
        event.stopImmediatePropagation();
    }, true);

    function updateClock() {
        const views = document.hidden ? [] : [...document.querySelectorAll('.desktop-panel-clock')]
            .filter(canvasFor)
            .map(node => {
                const dock = node.closest('.desktop-dock');
                const compact = !!dock?.closest('.dock-side-zone');
                const smallDock = !!dock && parseFloat(getComputedStyle(dock).getPropertyValue('--dock-preferred-icon')) < 40;
                dock?.toggleAttribute('data-small-clock', smallDock);
                return {node, compact, smallDock, label: node.getAttribute('aria-label'),
                    preview: node.classList.contains('desktop-time-preview')};
            });
        const options = document.querySelector('.desktop-clock-options')?.textContent;
        const sample = document.querySelector('.desktop-clock-sample')?.textContent;
        const changed = sample !== clockSample;
        if (changed) {
            clockSample = sample;
            const [, seconds, offset, zoneName] = sample ? JSON.parse(sample) : [];
            const unixTime = seconds * 1000;
            clockAnchor = Number.isFinite(new Date(unixTime).getTime()) && Number.isFinite(offset)
                ? {unixTime, offset, zoneName, received: performance.now()}
                : null;
        }
        if (!changed && options === clockOptions && views.length === clockViews.length
            && views.every((view, i) => view.node === clockViews[i].node
                && view.compact === clockViews[i].compact && view.smallDock === clockViews[i].smallDock
                && view.label === clockViews[i].label)) return;

        clockOptions = options;
        clockViews = views;
        clearTimeout(clockTimer);
        if (!views.length) return;
        if (!clockAnchor) {
            for (const {node} of views) {
                node.querySelector('.desktop-panel-time').textContent = '—';
                node.querySelector('.desktop-panel-weekday').textContent = '';
                node.querySelector('.desktop-panel-date').textContent = '';
                node.querySelector('.desktop-clock-tooltip').textContent = '';
                node.removeAttribute('title');
            }
            return;
        }

        const [locale, hourCycle, showWeekday, showDate, showSeconds] =
            (options || 'en-US,h12,false,true,true').split(',');
        const {unixTime, offset, zoneName, received} = clockAnchor;
        const wallTime = unixTime + offset * 60_000;
        const hours = String(Math.floor(Math.abs(offset) / 60)).padStart(2, '0');
        const minutes = String(Math.abs(offset) % 60).padStart(2, '0');
        const zone = `UTC${offset < 0 ? '-' : '+'}${hours}:${minutes}`;
        const formatter = fields => new Intl.DateTimeFormat(locale, {timeZone: 'UTC', ...fields});
        const timeFields = {hour: 'numeric', minute: '2-digit', hourCycle};
        const fullTime = formatter({...timeFields, second: '2-digit'});
        const fullDate = formatter({year: 'numeric', month: 'numeric', day: 'numeric'});
        const detailDate = formatter({weekday: 'long', year: 'numeric', month: 'long', day: 'numeric'});
        const detailZone = zoneName ? `${zoneName} (${zone})` : zone;
        const dockTime = showSeconds === 'true' ? fullTime : formatter(timeFields);
        const compactTime = formatter({...timeFields, hour: '2-digit',
            second: showSeconds === 'true' ? '2-digit' : undefined});
        const compactDate = formatter({month: '2-digit', day: '2-digit'});
        const weekday = formatter({weekday: 'short'});
        const compactMinutes = formatter({...timeFields, hour: '2-digit'});
        const displays = views.map(({node, compact, smallDock, label, preview}) => {
            const minuteOnly = compact && smallDock;
            node.lang = locale;
            if (preview) node.title = `${label} (${zone})`;
            else node.removeAttribute('title');
            return {
                time: node.querySelector('.desktop-panel-time'),
                weekday: node.querySelector('.desktop-panel-weekday'),
                date: node.querySelector('.desktop-panel-date'),
                tooltip: preview ? null : node.querySelector('.desktop-clock-tooltip'),
                timeFormatter: preview ? fullTime : minuteOnly ? compactMinutes : compact ? compactTime : dockTime,
                detailTime: minuteOnly ? compactMinutes : fullTime,
                dateFormatter: preview || showDate === 'true' ? (compact ? compactDate : fullDate) : null,
                showWeekday: !preview && (compact || !smallDock) && showWeekday === 'true',
            };
        });
        function tick() {
            const now = new Date(wallTime + (performance.now() - received));
            for (const display of displays) {
                setClockText(display.time, display.timeFormatter.format(now));
                setClockText(display.weekday, display.showWeekday ? weekday.format(now) : '');
                setClockText(display.date, display.dateFormatter?.format(now) ?? '');
                if (display.tooltip) {
                    setClockText(display.tooltip, `${detailDate.format(now)}\n${display.detailTime.format(now)}\n${detailZone}`);
                }
            }
            clockTimer = setTimeout(tick, 1000 - now.getUTCMilliseconds());
        }
        tick();
    }

    function setClockText(node, value) {
        if (node.textContent !== value) node.textContent = value;
    }

    function mountTimeZonePicker(dialog, search, list) {
        const locale = clockOptions?.split(',')[0] || 'en-US';
        const countries = typeof Intl.DisplayNames === 'function'
            ? new Intl.DisplayNames([locale], {type: 'region'}) : null;
        const normalize = value => value.toLowerCase().normalize('NFD')
            .replace(/\p{Diacritic}/gu, '').replaceAll('_', ' ');
        const rows = [...list.querySelectorAll('.time-zone-option')].map(node => {
            const name = node.getAttribute('aria-label');
            const code = node.querySelector('.time-zone-country-code').textContent;
            const country = code ? countries?.of(code) ?? code : '';
            if (country) {
                const countryLabel = document.createElement('span');
                countryLabel.className = 'time-zone-country';
                countryLabel.textContent = country;
                node.querySelector('.time-zone-place').append(countryLabel);
            }
            const offset = document.createElement('span');
            offset.className = 'time-zone-offset';
            node.querySelector('.time-zone-detail').append(offset);
            return {node, searchText: normalize(`${name} ${country}`)};
        });
        const empty = list.querySelector('.time-zone-empty');
        const visible = new Map();
        function refresh() {
            const now = new Date(clockAnchor
                ? clockAnchor.unixTime + performance.now() - clockAnchor.received : Date.now());
            for (const [node, format] of visible) {
                const offset = format.offset.formatToParts(now).find(part => part.type === 'timeZoneName')?.value;
                const utc = offset?.replace(/^GMT/, 'UTC') || '';
                setClockText(node.querySelector('.time-zone-offset'), utc === 'UTC+00:00' ? 'UTC' : utc);
                setClockText(node.querySelector('.time-zone-local-time'), format.time.format(now));
            }
        }
        const observer = new IntersectionObserver(entries => {
            for (const {target, isIntersecting} of entries) {
                if (!isIntersecting) { visible.delete(target); continue; }
                const timeZone = target.getAttribute('aria-label');
                try {
                    visible.set(target, {
                        time: new Intl.DateTimeFormat(locale, {timeZone, weekday: 'short', hour: '2-digit',
                            minute: '2-digit', hourCycle: 'h23'}),
                        offset: new Intl.DateTimeFormat('en', {timeZone, timeZoneName: 'longOffset'}),
                    });
                } catch (_) { /* Older WebViews may not recognize every zone installed on the server. */ }
            }
            refresh();
        }, {root: list});
        for (const {node} of rows) observer.observe(node);

        search.placeholder = search.getAttribute('aria-label') || '';
        search.addEventListener('input', () => {
            const terms = normalize(search.value).trim().split(/\s+/);
            let count = 0;
            for (const {node, searchText} of rows) {
                node.hidden = !terms.every(term => searchText.includes(term));
                if (!node.hidden) count++;
            }
            empty.classList.toggle('hidden', count !== 0);
            list.scrollTop = 0;
        });
        dialog.addEventListener('keydown', event => {
            if (event.isComposing || !['ArrowDown', 'ArrowUp'].includes(event.key)
                || (event.target !== search && !event.target.matches('.time-zone-option'))) return;
            const options = rows.filter(({node}) => !node.hidden).map(({node}) => node);
            if (!options.length) return;
            const index = options.indexOf(event.target);
            const next = index < 0 ? options[0]
                : event.key === 'ArrowDown' ? options[index + 1] : options[index - 1] || search;
            event.preventDefault();
            next?.focus();
        });
        requestAnimationFrame(() => search.focus({preventScroll: true}));
        const timer = setInterval(refresh, 1000);
        return {dialog, stop: () => { clearInterval(timer); observer.disconnect(); }};
    }

    function updateTimeZonePicker() {
        const dialog = document.querySelector('.time-zone-dialog');
        if (timeZonePicker ? timeZonePicker.dialog === dialog : !dialog) return;
        if (timeZonePicker) {
            timeZonePicker.stop();
            timeZonePicker = null;
            if (!dialog) requestAnimationFrame(() => {
                const target = document.querySelector('.app-settings.active .desktop-time-zone-current');
                if (!target?.closest('[inert]')) target?.focus({preventScroll: true});
            });
        }
        const search = dialog?.querySelector('.time-zone-query');
        const list = dialog?.querySelector('.time-zone-options');
        if (search && list?.querySelector('.time-zone-empty')) timeZonePicker = mountTimeZonePicker(dialog, search, list);
    }

    function showSizeValue({slider, value}) {
        const size = slider.valueAsNumber;
        const min = Number(slider.min);
        const max = Number(slider.max);
        value.textContent = `${size} px`;
        slider.style.setProperty('--icon-size-progress', `${(size - min) / (max - min) * 100}%`);
    }

    function mountSizeControl(host, target, preview, bridge, min, max) {
        const slider = document.createElement('input');
        slider.className = 'icon-size-slider';
        slider.type = 'range';
        slider.min = min;
        slider.max = max;
        slider.step = 1;
        const value = document.createElement('span');
        value.className = 'icon-size-value';
        host.append(slider, value);

        const control = {host, target, bridge, slider, value};
        slider.addEventListener('input', () => {
            target.style.setProperty(preview, `${slider.value}px`);
            if (target.matches('.desktop-dock')) updateClock();
            showSizeValue(control);
        });
        slider.addEventListener('change', () => {
            if (slider.valueAsNumber === control.saved) {
                target.style.removeProperty(preview);
                return;
            }
            bridge.value = slider.value;
            bridge.dispatchEvent(new Event('input', {bubbles: true}));
        });
        slider.addEventListener('pointercancel', () => {
            slider.value = control.saved;
            showSizeValue(control);
            target.style.removeProperty(preview);
            if (target.matches('.desktop-dock')) updateClock();
        });
        return control;
    }

    function updateSizeControl(settings, selector, targetSelector, preview) {
        const host = settings?.querySelector(selector);
        const target = host?.closest(targetSelector) || canvasFor(host)?.querySelector(targetSelector);
        const bridge = host?.querySelector('.icon-size-bridge');
        const state = host?.querySelector('.icon-size-state');
        let control = sizeControls.get(selector);
        if (control && (control.host !== host || control.target !== target || control.bridge !== bridge)) {
            control.target.style.removeProperty(preview);
            control.slider.remove();
            control.value.remove();
            sizeControls.delete(selector);
            control = undefined;
        }
        if (!target || !bridge || !state) return;
        const [min, max, size] = state.textContent.split(',').map(Number);
        if (!control) {
            control = mountSizeControl(host, target, preview, bridge, min, max);
            sizeControls.set(selector, control);
        }
        const {slider} = control;
        const label = bridge.getAttribute('aria-label') || '';
        if (slider.getAttribute('aria-label') !== label) slider.setAttribute('aria-label', label);
        if (slider.disabled !== bridge.disabled) slider.disabled = bridge.disabled;
        if (control.saved !== size) {
            control.saved = size;
            slider.value = size;
            showSizeValue(control);
            target.style.removeProperty(preview);
        }
    }

    function placeDockMenu(menu, canvas, anchor) {
        const frame = canvas.getBoundingClientRect();
        const dock = anchor.closest('.desktop-dock').getBoundingClientRect();
        const icon = anchor.getBoundingClientRect();
        const margin = 8;
        const gap = 8;
        const limit = (value, low, high) => Math.max(low, Math.min(value, high));
        const above = dock.top - frame.top;
        const below = frame.bottom - dock.bottom;
        const left = dock.left - frame.left;
        const right = frame.right - dock.right;
        const horizontalDock = !anchor.closest('.dock-side-zone');
        const side = horizontalDock ? (above >= below ? 'above' : 'below') : (right >= left ? 'right' : 'left');
        menu.dataset.dockMenuSide = side;
        menu.style.setProperty('--dock-menu-max-height', `${Math.max(0, (horizontalDock ? Math.max(above, below) - gap : frame.height) - margin)}px`);
        menu.style.setProperty('--dock-menu-max-width', `${Math.max(0, (horizontalDock ? frame.width - margin : Math.max(left, right) - gap) - margin)}px`);
        const width = menu.offsetWidth;
        const height = menu.offsetHeight;
        const centerX = (icon.left + icon.right) / 2 - frame.left;
        const centerY = (icon.top + icon.bottom) / 2 - frame.top;
        const x = horizontalDock ? limit(centerX - width / 2, margin, frame.width - width - margin)
            : side === 'right' ? dock.right - frame.left + gap : left - gap - width;
        const y = horizontalDock ? (side === 'above' ? above - gap - height : dock.bottom - frame.top + gap)
            : limit(centerY - height / 2, margin, frame.height - height - margin);
        const arrow = horizontalDock ? limit(centerX - x, 14, width - 14) : limit(centerY - y, 14, height - 14);
        menu.style.left = `${x}px`;
        menu.style.top = `${y}px`;
        menu.style.setProperty('--dock-menu-arrow', `${arrow}px`);
    }

    function update() {
        updateTheme();
        updateTimeZonePicker();
        updateSizeControl(document, '.default-file-size', '.folder-defaults', '--files-icon-preview');
        const settings = document.querySelector('.desktop-window.app-settings.active');
        updateSizeControl(settings, '.dock-size-control', '.desktop-dock', '--dock-preview');
        updateClock();
        updateSizeControl(settings, '.desktop-size-control', '.desktop-shortcuts', '--desktop-icon-preview');
        for (const pixels of document.querySelectorAll('.server-desktop:not(.hidden) .files-icon-pixels')) {
            const pane = pixels.closest('.files-pane,.files-inactive-preview');
            const size = pixels.textContent + 'px';
            if (pane && pane.style.getPropertyValue('--saved-file-size') !== size) pane.style.setProperty('--saved-file-size', size);
        }
        const viewMenu = document.querySelector('.server-desktop:not(.hidden) .files-pane .file-actions-menu');
        updateSizeControl(viewMenu, '.files-size-control', '.files-pane', '--files-icon-preview');
        for (const slot of document.querySelectorAll('.desktop-icon-slot:not([draggable])')) slot.draggable = true;
        const menu = document.querySelector('.desktop-context-menu');
        const canvas = menu && canvasFor(menu);
        if (!canvas) { focusedMenu = null; return; }
        if (dockMenuAnchor?.isConnected && canvas.contains(dockMenuAnchor) && dockMenuAnchor.getClientRects().length) {
            placeDockMenu(menu, canvas, dockMenuAnchor);
        } else {
            delete menu.dataset.dockMenuSide;
            const [x, y] = menu.querySelector('.desktop-menu-position').textContent.split(',').map(Number);
            menu.style.left = `${Math.max(4, Math.min(x, canvas.clientWidth - menu.offsetWidth - 4))}px`;
            menu.style.top = `${Math.max(4, Math.min(y, canvas.clientHeight - menu.offsetHeight - 4))}px`;
        }
        if (focusedMenu !== menu) {
            focusedMenu = menu;
            menu.querySelector('button:not(:disabled)')?.focus({preventScroll: true});
        }
    }

    new MutationObserver(update).observe(document, {subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ['class', 'hidden', 'inert', 'disabled', 'aria-label']});
    document.addEventListener('visibilitychange', updateClock);
    window.addEventListener('resize', update);
})();
