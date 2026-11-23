(() => {
    const popupSelector = '.editor-menu-popup:popover-open';
    const mac = /Mac|iPhone|iPad/.test(navigator.platform);
    const primary = mac ? 'Meta+' : 'Control+';
    const shortcuts = new Map([
        [primary + 'N', '.editor-new'],
        [primary + 'O', '.editor-open'],
        [primary + 'S', '.remote-editor-save'],
        [primary + 'Shift+S', '.editor-save-as'],
        ['F5', '.editor-reload'],
        [primary + 'Z', '.editor-edit-undo'],
        [primary + (mac ? 'Shift+Z' : 'Y'), '.editor-edit-redo'],
        [primary + 'X', '.editor-edit-cut'],
        [primary + 'C', '.editor-edit-copy'],
        [primary + 'V', '.editor-edit-paste'],
        [primary + 'A', '.editor-edit-selectAll'],
    ]);

    function showMenu(trigger) {
        const popup = trigger.nextElementSibling;
        popup.setAttribute('aria-label', trigger.textContent);
        popup.showPopover({source: trigger});
        const anchor = trigger.getBoundingClientRect();
        const left = Math.max(0, Math.min(anchor.left, innerWidth - popup.offsetWidth));
        popup.style.left = left + 'px';
        popup.style.top = (anchor.bottom + popup.offsetHeight <= innerHeight ? anchor.bottom : Math.max(0, anchor.top - popup.offsetHeight)) + 'px';
        popup.querySelector('button:enabled')?.focus();
    }

    function initMenus() {
        for (const popup of document.querySelectorAll('.editor-menu-popup:not([popover])')) {
            const trigger = popup.previousElementSibling;
            trigger.setAttribute('aria-haspopup', 'true');
            trigger.setAttribute('aria-expanded', 'false');
            popup.setAttribute('popover', 'auto');
            popup.setAttribute('role', 'group');
            trigger.popoverTargetElement = popup;
            for (const [chord, selector] of shortcuts) {
                const item = popup.querySelector(selector);
                if (!item) continue;
                item.setAttribute('aria-keyshortcuts', chord);
                item.dataset.shortcut = chord.replace('Control+', 'Ctrl+').replace('Meta+', 'Cmd+');
            }
            popup.addEventListener('toggle', () => trigger.setAttribute('aria-expanded', String(popup.matches(':popover-open'))));
        }
        const open = document.querySelector(popupSelector);
        if (open && open.closest('.hidden, [inert], .desktop-window:not(.active)')) open.hidePopover();
    }

    document.addEventListener('pointerover', event => {
        const trigger = event.target.closest?.('.editor-menu-trigger');
        if (!trigger || event.pointerType !== 'mouse') return;
        const open = trigger.closest('.editor-menubar').querySelector(popupSelector);
        if (open && open !== trigger.nextElementSibling) showMenu(trigger);
    });

    document.addEventListener('click', async event => {
        const button = event.target.closest?.('.editor-menu button');
        if (!button || button.disabled) return;
        if (button.matches('.editor-menu-trigger')) {
            event.preventDefault();
            const popup = button.nextElementSibling;
            if (popup.matches(':popover-open')) popup.hidePopover();
            else showMenu(button);
            return;
        }
        const popup = button.closest('.editor-menu-popup');
        const input = button.closest('.app-editor').querySelector('.editor-sheet textarea:enabled');
        button.closest('.editor-menubar').querySelector(popupSelector)?.hidePopover();
        const command = [...button.classList].find(name => name.startsWith('editor-edit-'))?.slice('editor-edit-'.length);
        if (!input || !command) return;
        input.focus();
        if (command === 'selectAll') input.select();
        else if (command === 'paste') {
            if (!document.execCommand('paste')) {
                try {
                    const text = await navigator.clipboard.readText();
                    if (input.isConnected && !input.disabled && document.activeElement === input)
                        document.execCommand('insertText', false, text);
                } catch {
                    if (input.isConnected && document.activeElement === input) {
                        const shortcut = /Mac/.test(navigator.platform) ? '⌘V' : 'Ctrl+V';
                        input.setCustomValidity(popup.querySelector('.editor-paste-help').textContent + ' (' + shortcut + ')');
                        input.reportValidity();
                        input.addEventListener('input', () => input.setCustomValidity(''), {once: true});
                    }
                }
            }
        } else document.execCommand(command);
    });

    document.addEventListener('keydown', event => {
        if (event.isComposing || event.target.closest?.('[inert]')) return;
        const input = event.target;
        if (event.key === 'Enter' && input.matches?.('textarea.remote-editor-content') &&
            !event.ctrlKey && !event.metaKey && !event.altKey) {
            // The shell handles Enter as Submit in capture. Let the textarea perform
            // the native edit, including selection replacement and undo.
            event.stopPropagation();
            const view = sheets.get(input.closest('.editor-sheet'));
            if (view?.mode && !input.readOnly && !input.disabled && !event.shiftKey &&
                event.keyCode !== 229 && input.selectionStart === input.selectionEnd) {
                const prefix = input.value.slice(0, input.selectionStart);
                const indent = /^[\t ]+/.exec(prefix.slice(prefix.lastIndexOf('\n') + 1));
                if (indent && document.execCommand('insertText', false, '\n' + indent[0])) event.preventDefault();
            }
            return;
        }
        const menu = event.target.closest?.('.editor-menu');
        if (menu && ['ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight', 'Escape'].includes(event.key)) {
            const popup = menu.querySelector('.editor-menu-popup');
            const trigger = popup.previousElementSibling;
            const opened = popup.matches(':popover-open');
            if (event.key === 'Escape') {
                if (!opened) return;
                popup.hidePopover();
                trigger.focus();
            } else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
                const menus = [...menu.parentElement.querySelectorAll('.editor-menu-trigger')];
                const index = menus.indexOf(trigger) + (event.key === 'ArrowRight' ? 1 : -1);
                const next = menus[(index + menus.length) % menus.length];
                if (opened) showMenu(next);
                else next.focus();
            } else if (opened) {
                const items = [...popup.querySelectorAll('button:enabled')];
                const index = items.indexOf(document.activeElement) + (event.key === 'ArrowDown' ? 1 : -1);
                items[(index + items.length) % items.length]?.focus();
            } else showMenu(trigger);
            event.preventDefault();
            event.stopImmediatePropagation();
            return;
        }
        if (event.altKey || event.getModifierState('AltGraph') || (event.ctrlKey && event.metaKey)) return;
        const modified = event.ctrlKey || event.metaKey;
        if (!modified && event.key !== 'F5') return;
        const chord = (modified ? primary : '') + (event.shiftKey ? 'Shift+' : '') + event.key.toUpperCase();
        const selector = shortcuts.get(chord);
        if (!selector) return;
        // The textarea owns native clipboard and undo shortcuts. Route them through
        // the menu only while it has focus, so its displayed commands also work there.
        if (selector.startsWith('.editor-edit-') && (!menu || !(mac ? event.metaKey : event.ctrlKey))) return;
        const editor = event.target.closest?.('.remote-editor-modal, .desktop-window.app-editor')
            || (event.target.matches?.('.desktop-canvas') && event.target.querySelector('.app-editor.active:not(.hidden)'));
        if (!editor || editor.closest('[inert]')) return;
        const action = editor.querySelector(selector);
        if (!action) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        if (!action.disabled && !event.repeat) action.click();
    }, true);

    const rendered = new WeakMap();
    function renderPreviews() {
        for (const host of document.querySelectorAll('.editor-preview-host')) {
            const preview = host.querySelector('.markdown-preview');
            const html = host.querySelector('.markdown-preview-source')?.textContent;
            if (!preview || html === undefined || rendered.get(preview) === html) continue;
            rendered.set(preview, html);
            // Rust emits Markdown markup with raw HTML, links and images escaped or stripped.
            const template = document.createElement('template');
            template.innerHTML = html;
            preview.replaceChildren(template.content);
        }
    }
    // The textarea owns editing, selection, undo and IME. Only visible lines are painted.
    const sheets = new Map();
    const checkpointSize = 64;
    const maxTokenLine = 16000;

    function decoration(className, parent) {
        const node = document.createElement('div');
        node.className = 'editor-decoration ' + className;
        node.setAttribute('aria-hidden', 'true');
        parent.append(node);
        return node;
    }

    function fileMode(editor) {
        const name = editor.closest('.desktop-editor')?.querySelector('.editor-tab-item.active .editor-tab-label')?.textContent || '';
        const syntax = CodeMirror.findModeByFileName(name)
            || (/^\.env(?:\.|$)/i.test(name) && CodeMirror.findModeByName('Shell'))
            || (/\.(?:service|socket|timer|ini|conf)$/i.test(name) && CodeMirror.findModeByMIME('text/x-ini'));
        return syntax && syntax.mode !== 'null' && CodeMirror.modes[syntax.mode] ? syntax : null;
    }

    function setText(node, value) {
        if (node && node.textContent !== value) node.textContent = value;
    }

    function lineAt(starts, offset) {
        let low = 0, high = starts.length;
        while (low + 1 < high) {
            const mid = (low + high) >>> 1;
            if (starts[mid] <= offset) low = mid;
            else high = mid;
        }
        return low;
    }

    class SourceView {
        constructor(host, input) {
            this.host = host;
            this.input = input;
            this.editor = host.closest('.remote-editor');
            this.active = decoration('editor-active-line', host);
            this.gutter = decoration('editor-gutter', host);
            this.numbers = decoration('editor-numbers', this.gutter);
            this.clip = decoration('editor-code-clip', host);
            this.code = decoration('editor-code-lines', this.clip);
            input.wrap = 'off';
            input.spellcheck = false;
            input.autocapitalize = 'off';
            input.addEventListener('input', () => this.schedule());
            input.addEventListener('scroll', () => this.schedule(), {passive: true});
            input.addEventListener('selectionchange', () => this.schedule());
            input.addEventListener('compositionstart', () => host.classList.add('editor-composing'));
            input.addEventListener('compositionend', () => {
                host.classList.remove('editor-composing');
                this.schedule();
            });
            this.resize = new ResizeObserver(() => this.schedule());
            this.resize.observe(input);
        }

        schedule() {
            if (!this.frame) this.frame = requestAnimationFrame(() => {
                this.frame = 0;
                if (this.host.isConnected) this.draw();
            });
        }

        setSyntax(syntax) {
            if (syntax === this.syntax) return;
            this.syntax = syntax;
            this.mode = syntax && CodeMirror.getMode({indentUnit: 4, tabSize: 4}, syntax.mime);
            this.value = undefined;
        }

        index(value) {
            if (this.value !== undefined) {
                let keep = 0;
                const end = Math.min(value.length, this.value.length);
                while (keep < end && value.charCodeAt(keep) === this.value.charCodeAt(keep)) ++keep;
                // Retain only complete, unchanged blocks; some modes look ahead a line.
                const stable = Math.max(0, lineAt(this.starts, keep) - 1);
                this.states.length = Math.min(this.states.length, 1 + Math.floor(stable / checkpointSize));
            } else {
                this.states = this.mode ? [CodeMirror.startState(this.mode)] : [];
            }
            this.starts = [0];
            this.longLine = false;
            let start = 0;
            for (let end = value.indexOf('\n'); end !== -1; end = value.indexOf('\n', start)) {
                if (end - start > maxTokenLine) this.longLine = true;
                start = end + 1;
                this.starts.push(start);
            }
            this.longLine ||= value.length - start > maxTokenLine;
            this.value = value;
            this.range = '';
            this.position = -1;
            this.host.style.setProperty('--editor-gutter-width', `calc(${Math.max(2, String(this.starts.length).length)}ch + 24px)`);
        }

        line(index) {
            return this.value.slice(this.starts[index], (this.starts[index + 1] ?? this.value.length + 1) - 1);
        }

        tokenize(index, state, row) {
            const line = this.line(index);
            if (!line && this.mode.blankLine) this.mode.blankLine(state);
            const stream = new CodeMirror.StringStream(line, 4, {
                lookAhead: n => index + n < this.starts.length ? this.line(index + n) : undefined,
                baseToken: () => {},
            });
            let retries = 0;
            while (!stream.eol()) {
                const style = this.mode.token(stream, state);
                // Mixed-language modes may switch lexers before consuming a character.
                if (stream.pos <= stream.start) {
                    if (++retries < 8) continue;
                    stream.next();
                }
                retries = 0;
                if (row) {
                    if (style) {
                        const token = document.createElement('span');
                        token.className = 'cm-' + style.replace(/ +/g, ' cm-');
                        token.textContent = stream.current();
                        row.append(token);
                    } else row.append(stream.current());
                }
                stream.start = stream.pos;
            }
        }

        paint(first, end) {
            const block = Math.floor(first / checkpointSize);
            const deadline = performance.now() + 6;
            // Jumping down a large file can need more lexer state than fits in one frame.
            while (this.states.length <= block) {
                const from = (this.states.length - 1) * checkpointSize;
                const state = CodeMirror.copyState(this.mode, this.states.at(-1));
                for (let i = from; i < from + checkpointSize; ++i) this.tokenize(i, state);
                this.states.push(state);
                if (performance.now() > deadline) return false;
            }
            const state = CodeMirror.copyState(this.mode, this.states[block]);
            const rows = document.createDocumentFragment();
            for (let i = block * checkpointSize; i < end; ++i) {
                const row = i >= first ? document.createElement('div') : null;
                this.tokenize(i, state, row);
                if (row) rows.append(row);
                if ((i + 1) % checkpointSize === 0 && this.states.length === (i + 1) / checkpointSize)
                    this.states.push(CodeMirror.copyState(this.mode, state));
            }
            this.code.replaceChildren(rows);
            return true;
        }

        draw() {
            const input = this.input;
            if (!input.clientHeight) return;
            if (this.value !== input.value) this.index(input.value);
            const css = getComputedStyle(input);
            const height = parseFloat(css.lineHeight);
            const first = Math.max(0, Math.min(this.starts.length - 1, Math.floor((input.scrollTop - parseFloat(css.paddingTop)) / height)));
            const end = Math.min(this.starts.length, first + Math.ceil(input.clientHeight / height) + 2);
            const y = parseFloat(css.paddingTop) + height * first - input.scrollTop;
            this.numbers.style.transform = `translateY(${y}px)`;
            this.clip.style.width = input.clientWidth + 'px';
            this.clip.style.height = this.gutter.style.height = input.clientHeight + 'px';
            this.code.style.transform = `translate(${parseFloat(css.paddingLeft) - input.scrollLeft}px, ${y}px)`;
            const key = first + '/' + end;
            if (this.range !== key) {
                this.numbers.textContent = Array.from({length: end - first}, (_, i) => first + i + 1).join('\n');
                const colored = this.mode && !this.longLine;
                const ready = !colored || this.paint(first, end);
                this.host.classList.toggle('editor-highlighted', Boolean(colored && ready));
                if (ready) this.range = key;
                else this.schedule();
            }
            const caret = input.selectionDirection === 'backward' ? input.selectionStart : input.selectionEnd;
            const line = lineAt(this.starts, caret);
            this.active.style.top = parseFloat(css.paddingTop) + line * height - input.scrollTop + 'px';
            this.active.hidden = line < first || line >= end || input.disabled || input.selectionStart !== input.selectionEnd;
            if (this.position !== caret || this.lineNode !== this.editor.querySelector('.editor-line-value')) {
                this.lineNode = this.editor.querySelector('.editor-line-value');
                setText(this.lineNode, String(line + 1));
                let col = 0;
                for (const ch of this.value.slice(this.starts[line], caret)) col += ch === '\t' ? 4 - col % 4 : 1;
                setText(this.editor.querySelector('.editor-column-value'), String(col + 1));
                this.position = caret;
            }
        }
    }

    function syncEditors() {
        initMenus();
        for (const [host, view] of sheets) {
            if (!host.isConnected) {
                cancelAnimationFrame(view.frame);
                view.resize.disconnect();
                sheets.delete(host);
            }
        }
        for (const editor of document.querySelectorAll('.desktop-editor .remote-editor')) {
            const host = editor.querySelector('.editor-sheet');
            const input = host?.querySelector('textarea');
            let view = sheets.get(host);
            if (input && !view) {
                view = new SourceView(host, input);
                sheets.set(host, view);
                if (!input.disabled && input.closest('.app-editor.active:not(.hidden)') && !input.closest('[inert]'))
                    input.focus({preventScroll: true});
            }
            const prefs = editor.closest('.desktop-editor')?.querySelector('.editor-preferences')?.textContent;
            if (view && prefs && view.preferences !== prefs) {
                const [size, numbers] = JSON.parse(prefs);
                host.style.setProperty('--font-code', size + 'px');
                host.style.setProperty('--editor-line-height', Math.round(size * 22 / 13) + 'px');
                host.classList.toggle('hide-line-numbers', !numbers);
                view.preferences = prefs;
            }
            const syntax = fileMode(editor);
            view?.setSyntax(syntax);
            setText(editor.querySelector('.editor-mode-name'), syntax?.name || '');
            view?.schedule();
        }
    }

    const observer = new MutationObserver(records => {
        if (records.every(record => {
            const target = record.target.nodeType === 1 ? record.target : record.target.parentElement;
            return target?.closest('.editor-decoration, .editor-mode-name, .editor-line-value, .editor-column-value, .markdown-preview');
        })) return;
        renderPreviews();
        syncEditors();
    });
    observer.observe(document, {childList: true, characterData: true, attributes: true, attributeFilter: ['disabled'], subtree: true});
    renderPreviews();
    syncEditors();
})();
