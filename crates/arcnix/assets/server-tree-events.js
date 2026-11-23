// Route native context-menu gestures through the same Rust-owned More buttons.
(() => {
    let pointerMenu = null;
    let keyboardMenu = null;
    let scheduled = false;
    let drag = null;
    let hint = null;
    let dragSequence = 0;

    function markDrop(row, style) {
        if (hint?.row === row && hint?.style === style) return;
        if (hint) hint.row.classList.remove(hint.style);
        hint = row ? {row, style} : null;
        if (hint) row.classList.add(style);
    }

    function clearDrag() {
        markDrop(null);
        drag?.row.classList.remove('tree-drag-source');
        drag = null;
    }

    function groupId(folder) {
        return folder?.querySelector('.tree-group-id')?.textContent;
    }

    document.addEventListener('dragstart', event => {
        const button = event.target.closest?.('.group-button,.host-select');
        const tree = button?.closest('.server-tree');
        if (!tree || !button.draggable || tree.closest('.reorder-disabled,[inert]')) return;
        const row = button.closest('.tree-folder-row,.host-row');
        const server = button.matches('.host-select');
        const id = server ? row.querySelector('.tree-server-id')?.textContent : groupId(row);
        if (!id) return;
        drag = {row, tree, id, server};
        row.classList.add('tree-drag-source');
        event.dataTransfer.effectAllowed = 'move';
        event.dataTransfer.setData(server ? 'application/x-arcnix-server' : 'application/x-arcnix-group', id);
        event.dataTransfer.setDragImage(row, 20, row.offsetHeight / 2);
    }, true);

    function destination(event) {
        const target = event.target;
        const sidebar = drag?.tree.closest('.server-explorer');
        if (!sidebar || target.closest?.('.server-explorer') !== sidebar ||
            drag.tree.closest('.reorder-disabled,[inert]') || target.closest('.context-popover')) return;
        const folder = target.closest('.tree-folder');
        let row = folder?.querySelector(':scope > .tree-folder-row');
        if (drag.server) {
            const root = target.closest('.tree-root-row');
            if (!row && !root && target !== drag.tree) return;
            const id = groupId(row) ?? null;
            if (id === (groupId(drag.row.closest('.tree-folder')) ?? null)) return;
            return {row: row || sidebar.querySelector('.tree-root-row'), style: 'drop-inside',
                action: {kind: 'move_server', server: drag.id, target: id}};
        }
        if (!drag.tree.contains(target)) return;
        if (!row) {
            const rows = drag.tree.querySelectorAll('.tree-folder-row');
            const last = rows[rows.length - 1];
            if (last && event.clientY > last.getBoundingClientRect().bottom) row = last;
        }
        if (!row || groupId(row) === drag.id) return;
        const box = row.getBoundingClientRect();
        const after = event.clientY > box.top + box.height / 2;
        return {row, style: after ? 'drop-after' : 'drop-before',
            action: {kind: 'reorder', group: drag.id, target: groupId(row), after}};
    }

    document.addEventListener('dragover', event => {
        if (!drag) return;
        const dest = destination(event);
        markDrop(dest?.row, dest?.style);
        if (drag.tree.contains(event.target)) {
            const bounds = drag.tree.getBoundingClientRect();
            if (event.clientY < bounds.top + 28) drag.tree.scrollTop -= 12;
            else if (event.clientY > bounds.bottom - 28) drag.tree.scrollTop += 12;
        }
        if (!dest) return;
        event.preventDefault();
        event.dataTransfer.dropEffect = 'move';
    }, true);
    document.addEventListener('dragleave', event => {
        if (drag && !drag.tree.closest('.server-explorer')?.contains(event.relatedTarget)) markDrop(null);
    }, true);
    document.addEventListener('drop', event => {
        if (!drag) return;
        const dest = destination(event);
        const bridge = drag.tree.closest('.server-explorer')?.querySelector('.server-tree-input');
        clearDrag();
        if (!dest || !bridge) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        bridge.value = JSON.stringify({action: dest.action, sequence: ++dragSequence});
        bridge.dispatchEvent(new Event('input', {bubbles: true}));
    }, true);
    document.addEventListener('dragend', clearDrag, true);
    const sidebarFor = target => target instanceof Element ? target.closest('.server-explorer') : null;
    function openMenu(target, point) {
        const sidebar = sidebarFor(target);
        if (!sidebar || sidebar.closest('[inert]') || target.closest('input,textarea,.context-popover')) return false;
        const row = target.closest('.host-row,.group-row');
        const more = row?.querySelector('.host-more,.group-more') || sidebar.querySelector('.tree-more');
        if (!more || more.disabled) return false;
        const owner = row || sidebar.querySelector('.tree-root-row');
        pointerMenu = point ? {owner, ...point} : null;
        keyboardMenu = point ? null : owner;
        more.focus({preventScroll: true});
        if (!owner.classList.contains('menu-open')) more.click();
        schedule();
        return true;
    }
    document.addEventListener('contextmenu', event => {
        const target = event.target instanceof Element && event.target.matches('.menu-dismiss')
            ? document.elementsFromPoint(event.clientX, event.clientY).find(item => sidebarFor(item)) : event.target;
        if (openMenu(target, {x: event.clientX, y: event.clientY})) {
            event.preventDefault();
            event.stopImmediatePropagation();
        }
    }, true);
    document.addEventListener('pointerdown', event => {
        if (event.button === 0 && !(event.target instanceof Element && event.target.closest('.context-popover'))) {
            pointerMenu = null;
            schedule();
        }
    }, true);
    document.addEventListener('keydown', event => {
        const target = event.target;
        const sidebar = sidebarFor(target);
        if (!sidebar || sidebar.closest('[inert]')) return;
        if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
            if (openMenu(target, null)) { event.preventDefault(); event.stopImmediatePropagation(); }
            return;
        }
        const menu = target.closest('.context-popover');
        if (menu && ['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
            const items = [...menu.querySelectorAll('button:not(:disabled)')];
            const index = items.indexOf(target.closest('button'));
            const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (index + (event.key === 'ArrowUp' ? -1 : 1) + items.length) % items.length;
            items[next]?.focus(); event.preventDefault(); event.stopImmediatePropagation(); return;
        }
        if (target.closest('input,textarea,.context-popover') || event.altKey || event.ctrlKey || event.metaKey) return;
        const button = target.closest('.group-button,.host-select');
        if (!button) return;
        const folder = button.closest('.group-row');
        if (folder && (event.key === 'ArrowRight' || event.key === 'ArrowLeft')) {
            if ((event.key === 'ArrowRight') !== folder.classList.contains('expanded')) button.click();
        } else if (event.key === 'ArrowLeft' && button.closest('.tree-children')) {
            button.closest('.tree-folder')?.querySelector('.group-button')?.focus();
        } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
            const buttons = [...sidebar.querySelectorAll('.group-button,.host-select')].filter(item => !item.disabled && item.getClientRects().length);
            const index = buttons.indexOf(button);
            const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : Math.max(0, Math.min(buttons.length - 1, index + (event.key === 'ArrowUp' ? -1 : 1)));
            buttons[next]?.focus();
        } else return;
        event.preventDefault(); event.stopImmediatePropagation();
    }, true);
    function placeMenu() {
        scheduled = false;
        for (const button of document.querySelectorAll('.server-tree :is(.group-button,.host-select)')) {
            const draggable = !button.disabled && !button.closest('.reorder-disabled,[inert]');
            if(button.draggable!==draggable) button.draggable=draggable;
        }
        for (const menu of document.querySelectorAll('.server-explorer .context-popover')) {
            if (keyboardMenu?.contains(menu)) {
                menu.querySelector('button:not(:disabled)')?.focus(); keyboardMenu = null;
            }
            if (pointerMenu?.owner.isConnected && pointerMenu.owner.contains(menu)) {
                if (!menu.classList.contains('pointer-menu')) menu.classList.add('pointer-menu');
                const box = menu.getBoundingClientRect();
                menu.style.left = `${Math.max(8, Math.min(pointerMenu.x, innerWidth - box.width - 8))}px`;
                menu.style.top = `${Math.max(8, Math.min(pointerMenu.y, innerHeight - box.height - 8))}px`;
            } else if (menu.classList.contains('pointer-menu')) {
                menu.classList.remove('pointer-menu');
                menu.style.removeProperty('left'); menu.style.removeProperty('top');
            }
        }
    }
    function schedule() { if (!scheduled) { scheduled = true; requestAnimationFrame(placeMenu); } }
    new MutationObserver(schedule).observe(document, {subtree: true, childList: true, attributes: true, attributeFilter: ['class','disabled','inert']});
    // A menu's target can scroll away; dismiss via the normal Rust-owned scrim.
    document.addEventListener('scroll', event => {
        if (event.target instanceof Element && event.target.matches('.server-tree')) {
            document.querySelector('.menu-dismiss:not(.hidden)')?.click(); pointerMenu = null;
        }
    }, true);
})();
