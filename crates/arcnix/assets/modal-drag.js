// Dialogs open centered. Move and resize gestures share the backdrop's local coordinates.
(() => {
    const controls = 'button, a, input, textarea, select, label, [contenteditable], [role="button"], [role="tab"], .desktop-app-catalog-search';
    let drag;

    document.addEventListener('pointerdown', event => {
        const handle = event.target.closest?.('.modal-header, .desktop-resize-handle');
        const card = handle?.closest('.modal-card');
        if (!card || drag || !event.isPrimary || event.button !== 0 || event.target.closest(controls)) return;
        const edgeClass = [...handle.classList].find(name => name.startsWith('edge-'));
        const host = card.parentElement;
        const box = card.getBoundingClientRect();
        const frame = host.getBoundingClientRect();
        const style = getComputedStyle(host);
        drag = {
            handle, card, host, edge: edgeClass?.slice(5), pointer: event.pointerId, x: event.clientX, y: event.clientY,
            box, frame, wasSized: host.hasAttribute('data-modal-sized'),
            previousWidth: card.style.width, previousHeight: card.style.height,
            left: box.left - frame.left - parseFloat(style.paddingLeft),
            top: box.top - frame.top - parseFloat(style.paddingTop),
            previousX: host.style.getPropertyValue('--modal-x'),
            previousY: host.style.getPropertyValue('--modal-y'),
        };
        event.preventDefault();
        handle.setPointerCapture(event.pointerId);
    }, true);

    function applyPointer(event) {
        const dx = event.clientX - drag.x;
        const dy = event.clientY - drag.y;
        if (!drag.moved && Math.max(Math.abs(dx), Math.abs(dy)) < 4) return;
        drag.moved = true;
        const {handle, card, host, edge, box, frame} = drag;
        let {left, top} = drag;
        if (edge) {
            host.setAttribute('data-modal-sized', '');
            const west = edge.includes('w'), north = edge.includes('n');
            const width = west ? Math.min(box.right - frame.left, box.width - dx)
                : edge.includes('e') ? Math.min(frame.right - box.left, box.width + dx) : box.width;
            const height = north ? Math.min(box.bottom - frame.top, box.height - dy)
                : edge.includes('s') ? Math.min(frame.bottom - box.top, box.height + dy) : box.height;
            card.style.width = Math.max(0, width) + 'px';
            card.style.height = Math.max(0, height) + 'px';
            const resized = card.getBoundingClientRect();
            if (west) left += box.width - resized.width;
            if (north) top += box.height - resized.height;
        } else {
            handle.setAttribute('data-dragging', '');
            left += dx;
            top += dy;
        }
        host.style.setProperty('--modal-x', left + 'px');
        host.style.setProperty('--modal-y', top + 'px');
        host.setAttribute('data-modal-moved', '');
    }

    function finish(cancel) {
        if (!drag) return;
        const {handle, card, host, pointer, moved, previousX, previousY, previousWidth, previousHeight, wasSized} = drag;
        drag = null;
        if (cancel && moved) {
            host.style.setProperty('--modal-x', previousX);
            host.style.setProperty('--modal-y', previousY);
            host.toggleAttribute('data-modal-moved', Boolean(previousX));
            card.style.width = previousWidth;
            card.style.height = previousHeight;
            host.toggleAttribute('data-modal-sized', wasSized);
        }
        handle.removeAttribute('data-dragging');
        if (handle.hasPointerCapture(pointer)) handle.releasePointerCapture(pointer);
    }

    document.addEventListener('pointermove', event => {
        if (event.pointerId !== drag?.pointer) return;
        if (!event.buttons) { finish(true); return; }
        applyPointer(event);
    });
    document.addEventListener('pointerup', event => {
        if (event.pointerId !== drag?.pointer) return;
        applyPointer(event);
        finish(false);
    });
    for (const name of ['pointercancel', 'lostpointercapture']) {
        document.addEventListener(name, event => { if (event.pointerId === drag?.pointer) finish(true); });
    }
    window.addEventListener('blur', () => finish(true));
    window.addEventListener('keydown', event => {
        if (event.key !== 'Escape' || !drag?.moved) return;
        finish(true);
        event.preventDefault();
        event.stopImmediatePropagation();
    }, true);
})();
