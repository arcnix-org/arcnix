"""Open and edit remote text and Markdown in the Windows desktop workspace."""
import json
import time

from desktop_fixture import Desktop, Peer, ROOT, expect, eventually


def check_modal_resize(page, dialog):
    expect(dialog.locator('.desktop-resize-handle')).to_have_count(8)
    header = dialog.locator('.modal-header')
    expect(header.get_by_role('button')).to_have_count(1)
    expect(header.get_by_role('button', name='Close dialog', exact=True)).to_be_visible()
    area = page.locator('.desktop-canvas')

    def drag_to(edge, x, y):
        dialog.locator('.edge-' + edge).hover()
        page.mouse.down()
        page.mouse.move(x, y, steps=5)
        page.mouse.up()

    def dimensions():
        frame, box = area.bounding_box(), dialog.bounding_box()
        for axis in ('width', 'height'):
            assert box[axis] <= .8 * frame[axis] + 1, (frame, box)
        return frame, box

    frame, original = dimensions()
    drag_to('nw', original['x'] + 45, original['y'] + 35)
    _, smaller = dimensions()
    assert smaller['height'] < original['height'] - 10
    assert abs(smaller['x'] + smaller['width'] - original['x'] - original['width']) < 2
    header.hover(position={'x': 25, 'y': 18})
    page.mouse.down()
    page.mouse.move(frame['x'] + 26, frame['y'] + 19, steps=5)
    page.mouse.up()
    drag_to('se', frame['x'] + frame['width'] - 2, frame['y'] + frame['height'] - 2)
    _, largest = dimensions()
    assert all(abs(largest[axis] - .8 * frame[axis]) < 2 for axis in ('width', 'height')), (largest, frame)
    viewport = page.viewport_size
    page.set_viewport_size(dict(width=viewport['width'] - 150, height=viewport['height'] - 100))
    eventually(lambda: dimensions()[1]['width'] < largest['width'])
    page.set_viewport_size(viewport)


def main():
    out = ROOT / 'artifacts/desktop-editor' / f'run-{time.time_ns()}'
    local = out / 'local'
    local.mkdir(parents=True)
    with Peer(out / 'peer', io_delay=.06) as peer:
        home_desktop = peer.root / 'home/arcnix/Desktop'
        home_desktop.mkdir(parents=True, exist_ok=True)
        notes = home_desktop / 'Notes.md'
        source = ('# Notes\n\n**Bold text**\n\n'
                  '| Name | Value |\n| --- | --- |\n| port | 22 |\n\n'
                  '```sh\nls -l\n```\n\n'
                  '<img src="https://invalid.example/image.png">\n')
        notes.write_bytes(b'\xef\xbb\xbf' + source.replace('\n', '\r\n').encode())
        for number in range(28):
            (home_desktop / f'sample-{number:02}.txt').write_text('sample', encoding='utf-8')
        (peer.root / 'plain.txt').write_text('plain text from SSH', encoding='utf-8')
        python_source = ('    answer = 42\n"""\n' + 'documentation\n' * 300 +
                         'last comment\n"""\nvalue = "' + 'x' * 600 + '"')
        (peer.root / 'example.py').write_text(python_source, encoding='utf-8')
        (peer.root / 'drag.html').write_text('<p>dragged from Files</p>', encoding='utf-8')
        (home_desktop / 'drop.html').write_text('<p>dragged from Desktop</p>', encoding='utf-8')
        with Desktop(out / 'desktop', peer.config([peer.profile('Editor fixture')], local)) as app:
            p = app.page
            errors = []
            p.on('pageerror', lambda error: errors.append(str(error)))
            app.connect()
            app.workspace('Desktop')
            p.set_viewport_size(dict(width=900, height=620))
            shortcuts = p.locator('.desktop-shortcuts')
            apps = app.apps()
            search_apps = apps.get_by_role('textbox', name='Search apps', exact=True)
            search_apps.focus()
            page_last_app = apps.locator('.desktop-app-catalog-item').last
            search_apps.press('Shift+Tab')
            expect(page_last_app).to_be_focused()
            page_last_app.press('Tab')
            expect(search_apps).to_be_focused()
            expect(apps.locator('.modal-footer')).to_be_hidden()
            assert apps.evaluate('''dialog => dialog.getBoundingClientRect().bottom
                - dialog.querySelector('.desktop-app-catalog').getBoundingClientRect().bottom < 2''')
            check_modal_resize(p, apps)
            expect(apps.locator('.modal-footer')).to_be_hidden()
            launcher = apps.get_by_role('button', name='Text Editor', exact=True)
            editor_icon = launcher.locator('.desktop-app-icon')
            assert editor_icon.evaluate("icon => getComputedStyle(icon, '::before').maskImage") != 'none'
            p.keyboard.press('Escape')
            expect(shortcuts.get_by_role('button', name='sample-27.txt', exact=True)).to_have_count(1)
            layout = shortcuts.evaluate('''grid => {
                const slots = [...grid.querySelectorAll('.desktop-icon-slot')];
                const next = slots.find(slot => slot.offsetLeft > slots[0].offsetLeft);
                return {
                    columns: new Set(slots.map(slot => slot.offsetLeft)).size,
                    firstDown: slots[1].offsetTop > slots[0].offsetTop,
                    nextStartsAtTop: next?.offsetTop === slots[0].offsetTop,
                    verticalOverflow: grid.scrollHeight - grid.clientHeight,
                    horizontalOverflow: grid.scrollWidth - grid.clientWidth,
                    scrollbar: getComputedStyle(grid).scrollbarWidth,
                };
            }''')
            assert layout['columns'] > 2 and layout['firstDown'] and layout['nextStartsAtTop'], layout
            assert layout['verticalOverflow'] <= 1 and layout['horizontalOverflow'] > 0 and layout['scrollbar'] == 'none', layout
            shortcuts.hover(position={'x': 180, 'y': 30})
            p.mouse.wheel(0, 450)
            eventually(lambda: shortcuts.evaluate('el => el.scrollLeft') > 0)
            shortcuts.evaluate('el => el.scrollLeft = 0')
            app.screenshot('desktop-icon-columns.png')
            p.set_viewport_size(dict(width=1440, height=980))
            app.application('Text Editor')
            window = p.locator('.desktop-window.app-editor')
            titlebar = window.locator('.desktop-window-titlebar')
            editor = p.locator('.desktop-editor')
            contents = editor.get_by_role('textbox', name='File contents', exact=True)
            picker = p.get_by_role('dialog', name='Open file', exact=True)
            save_picker = p.get_by_role('dialog', name='Save as…', exact=True)
            strip = editor.locator('.editor-tab-strip')

            def command(menu, label):
                titlebar.get_by_role('button', name=menu, exact=True).click()
                titlebar.locator('.editor-menu-popup:popover-open').get_by_role('button', name=label, exact=True).click()

            def check_shortcuts(bindings):
                popup = titlebar.locator('.editor-menu-popup:popover-open')
                for name, chord in bindings.items():
                    item = popup.get_by_role('button', name=name, exact=True)
                    expect(item).to_have_attribute('aria-keyshortcuts', chord.replace('Ctrl+', 'Control+'))
                    assert chord in item.evaluate("el => getComputedStyle(el, '::after').content")

            def choose_file(name, *, double=False, filesystem=True):
                command('File', 'Open file…')
                expect(picker.get_by_role('button', name='File types', exact=True)).to_have_text('All supported text files')
                confirm = picker.get_by_role('button', name='Open', exact=True)
                expect(confirm).to_be_disabled()
                if filesystem:
                    picker.get_by_role('button', name='Filesystem', exact=True).click()
                entry = picker.get_by_role('button', name=name, exact=True)
                if double:
                    entry.dblclick()
                else:
                    entry.click()
                    expect(confirm).to_be_enabled()
                    confirm.click()
                expect(picker).to_have_count(0)

            expect(editor.get_by_text('Drop files here', exact=True)).to_be_visible()
            file_menu = titlebar.get_by_role('button', name='File', exact=True)
            file_menu.click()
            expect(titlebar.get_by_text('Save as…', exact=True)).to_be_visible()
            check_shortcuts({'New document': 'Ctrl+N', 'Open file…': 'Ctrl+O', 'Save': 'Ctrl+S',
                             'Save as…': 'Ctrl+Shift+S', 'Reload from server': 'F5'})
            file_menu.click()
            expect(titlebar.locator('.editor-menu-popup:popover-open')).to_have_count(0)
            file_menu.press('Control+o')
            check_modal_resize(p, picker)
            picker.get_by_role('button', name='Cancel', exact=True).click()
            expect(picker).to_have_count(0)
            file_menu.press('Control+n')
            expect(contents).to_be_enabled()
            expect(contents).to_have_value('')
            expect(contents).to_be_focused()
            contents.press('Enter')
            contents.press('Enter')
            expect(contents).to_have_value('\n\n')
            contents.press('Shift+ArrowLeft')
            contents.press('Shift+ArrowLeft')
            contents.press('Enter')
            expect(contents).to_have_value('\n')
            contents.fill('')
            contents.press_sequentially('draft')
            titlebar.get_by_role('button', name='Edit', exact=True).click()
            check_shortcuts({'Undo': 'Ctrl+Z', 'Redo': 'Ctrl+Y', 'Cut': 'Ctrl+X', 'Copy': 'Ctrl+C',
                             'Paste': 'Ctrl+V', 'Select all': 'Ctrl+A'})
            p.keyboard.press('Control+a')
            expect(contents).to_be_focused()
            assert contents.evaluate('e => e.selectionEnd - e.selectionStart') == 5
            p.keyboard.insert_text('test')
            command('Edit', 'Undo')
            expect(contents).to_have_value('draft')
            command('Edit', 'Redo')
            expect(contents).to_have_value('test')
            contents.fill('draft')
            contents.press('Control+s')
            filename = save_picker.get_by_role('textbox', name='File name', exact=True)
            save_picker.get_by_role('button', name='Filesystem', exact=True).click()
            filename.fill('新 文档.txt')
            save_picker.get_by_role('button', name='Save', exact=True).click()
            eventually(lambda: (peer.root / '新 文档.txt').read_bytes() == b'draft')
            expect(strip.locator('.editor-tab-label')).to_have_text('新 文档.txt')
            assert strip.bounding_box()['y'] >= titlebar.bounding_box()['y'] + titlebar.bounding_box()['height'] - 1
            expect(editor.locator('.editor-path, .editor-toolbar')).to_have_count(0)
            command('File', 'Save as…')
            expect(filename).to_have_value('新 文档.txt')
            filename.fill('copy.md')
            save_picker.get_by_role('button', name='Save', exact=True).click()
            eventually(lambda: (peer.root / 'copy.md').read_bytes() == b'draft')
            expect(strip.locator('.editor-tab-label')).to_have_text('copy.md')
            (peer.root / 'copy.md').write_bytes(b'external edit')
            contents.press('F5')
            expect(contents).to_have_value('external edit')
            contents.fill('second draft')
            contents.press('F5')
            expect(contents).to_have_value('second draft')
            command('File', 'Save as…')
            filename.fill('race.txt')
            expect(save_picker.get_by_role('button', name='Save', exact=True)).to_be_enabled()
            (peer.root / 'race.txt').write_bytes(b'appeared after listing')
            save_picker.get_by_role('button', name='Save', exact=True).click()
            expect(editor.locator('.editor-feedback')).to_contain_text('already exists')
            expect(strip.locator('.editor-tab-label')).to_have_text('copy.md')
            expect(contents).to_have_value('second draft')
            assert (peer.root / 'race.txt').read_bytes() == b'appeared after listing'
            contents.press('Control+Shift+s')
            filename.fill('新 文档.txt')
            save_picker.get_by_role('button', name='Save', exact=True).click()
            expect(save_picker.get_by_text('Replace existing file 新 文档.txt?', exact=True)).to_be_visible()
            save_picker.get_by_role('button', name='Cancel', exact=True).click()
            expect(save_picker).to_have_count(0)
            expect(contents).to_have_value('second draft')
            assert (peer.root / '新 文档.txt').read_bytes() == b'draft'
            command('File', 'Save as…')
            filename.fill('新 文档.txt')
            save_picker.get_by_role('button', name='Save', exact=True).click()
            save_picker.get_by_role('button', name='Replace', exact=True).click()
            eventually(lambda: (peer.root / '新 文档.txt').read_bytes() == b'second draft')
            assert (peer.root / 'copy.md').read_bytes() == b'external edit'
            command('File', 'Close tab')
            choose_file('drag.html', double=True)
            expect(contents).to_have_value('<p>dragged from Files</p>')
            paint = editor.locator('.editor-code-lines')
            expect(paint.locator('.cm-tag:not(.cm-bracket)').first).to_have_text('p')
            expect(paint.locator('p')).to_have_count(0)
            expect(editor.locator('.editor-mode-name')).to_have_text('HTML')
            expect(p.locator('.desktop-window.app-browser')).to_have_count(0)
            strip.get_by_role('button', name='Close file tab drag.html', exact=True).click()
            choose_file('example.py')
            expect(contents).to_have_value(python_source)
            expect(contents).to_be_enabled()
            expect(editor.locator('.editor-mode-name')).to_have_text('Python')
            contents.press('Control+Home')
            contents.press('End')
            contents.press('Enter')
            expect(contents).to_have_value(python_source.replace('42\n', '42\n    \n', 1))
            contents.press('Control+z')
            expect(contents).to_have_value(python_source)
            contents.press('Control+End')
            expect(editor.locator('.editor-line-value')).to_have_text('305')
            expect(paint.locator('.cm-string').filter(has_text='last comment')).to_have_count(1)
            assert contents.evaluate('el => el.scrollTop > 0 && el.scrollLeft > 0')
            strip.get_by_role('button', name='Close file tab example.py', exact=True).click()
            choose_file('plain.txt')
            expect(contents).to_have_value('plain text from SSH')
            expect(editor.locator('.editor-plain-label')).to_be_visible()
            expect(paint).not_to_be_visible()
            p.locator('.desktop-window.active .window-minimize').click()
            app.show_desktop()
            p.locator('.desktop-shortcuts').get_by_role('button', name='Notes.md', exact=True).click()
            expect(p.locator('.desktop-window.active .desktop-window-title')).to_have_text('Text Editor')
            preview = editor.locator('.markdown-preview')
            actions = titlebar.locator('.desktop-window-actions button')
            assert actions.evaluate_all("nodes => nodes.map(node => node.getAttribute('aria-label'))") == [
                'Settings', 'Minimize window', 'Toggle maximize', 'Close application',
            ]
            plain_tab = strip.get_by_role('button', name='/plain.txt', exact=True)
            notes_tab = strip.get_by_role('button', name='/home/arcnix/Desktop/Notes.md', exact=True)
            expect(strip.locator('.editor-tab')).to_have_count(2)
            expect(preview.get_by_role('heading', name='Notes')).to_be_visible()
            expect(preview.locator('strong')).to_have_text('Bold text')
            expect(preview.locator('td').last).to_have_text('22')
            expect(preview.locator('pre code')).to_have_text('ls -l\n')
            expect(preview.locator('img')).to_have_count(0)
            expect(preview).to_contain_text('<img src=')
            app.screenshot('markdown-preview.png')

            command('View', 'Edit source')
            expect(contents).to_have_value(source)
            expect(editor.locator('.editor-format')).to_have_text('UTF-8 BOM · CRLF')
            contents.press('Control+Home')
            contents.press('ArrowDown')
            expect(editor.locator('.editor-line-value')).to_have_text('2')
            edited = source.replace('# Notes', '# Notes\nSaved notes')
            contents.press('Control+Home')
            contents.press('End')
            contents.press('Enter')
            contents.press_sequentially('Saved notes')
            expect(contents).to_have_value(edited)
            command('View', 'Preview')
            expect(preview.get_by_text('Saved notes', exact=True)).to_be_visible()
            plain_tab.click()
            expect(contents).to_have_value('plain text from SSH')
            contents.fill('unsaved plain draft')
            notes_tab.click()
            expect(preview.get_by_text('Saved notes', exact=True)).to_be_visible()
            p.locator('.desktop-window.active .window-minimize').click()
            app.application('Text Editor')
            expect(preview.get_by_text('Saved notes', exact=True)).to_be_visible()
            command('View', 'Edit source')
            contents.press('Control+s')
            plain_tab.click()
            expect(contents).to_have_value('unsaved plain draft')
            eventually(lambda: notes.read_bytes() == b'\xef\xbb\xbf' + edited.replace('\n', '\r\n').encode())
            notes_tab.click()
            expect(editor.locator('.editor-feedback')).to_have_text('Remote file saved')

            choose_file('plain.txt')
            expect(strip.locator('.editor-tab')).to_have_count(2)
            expect(contents).to_have_value('unsaved plain draft')
            notes_tab.click()
            close_plain = strip.get_by_role('button', name='Close file tab plain.txt', exact=True)
            close_plain.click()
            expect(editor.get_by_text('Discard unsaved changes?')).to_be_visible()
            editor.get_by_role('button', name='Keep editing', exact=True).click()
            expect(contents).to_have_value('unsaved plain draft')
            close_plain.click()
            editor.get_by_role('button', name='Discard changes', exact=True).click()
            expect(plain_tab).to_have_count(0)
            expect(notes_tab).to_have_attribute('aria-pressed', 'true')
            assert (peer.root / 'plain.txt').read_text(encoding='utf-8') == 'plain text from SSH'

            contents.fill('# Unsaved draft\n')
            p.locator('.desktop-window.active .window-close').click()
            expect(editor.get_by_text('Discard unsaved changes in all open files?')).to_be_visible()
            editor.get_by_role('button', name='Keep editing', exact=True).click()
            expect(contents).to_have_value('# Unsaved draft\n')
            p.locator('.desktop-window.active .window-close').click()
            editor.get_by_role('button', name='Discard changes', exact=True).click()
            expect(p.locator('.desktop-window.app-editor')).to_have_count(0)
            assert b'Unsaved draft' not in notes.read_bytes()

            app.application('Text Editor')
            app.application('Files')
            # Keep both drop surfaces and the desktop shortcuts exposed at this viewport.
            placement = p.add_style_tag(content="""
                .desktop-window.app-editor, .desktop-window.app-files {
                    top: 60px !important; width: 36% !important; height: 55% !important;
                }
                .desktop-window.app-editor { left: 24% !important; }
                .desktop-window.app-files { left: 62% !important; }
            """)
            files_source = p.locator('.desktop-window.app-files:visible .file-row[aria-label="/drag.html"]')
            files_source.drag_to(editor, source_position={'x': 25, 'y': 12},
                                 target_position={'x': 110, 'y': 160})
            expect(contents).to_have_value('<p>dragged from Files</p>')
            expect(p.locator('.desktop-window.app-browser')).to_have_count(0)
            strip.get_by_role('button', name='Close file tab drag.html', exact=True).click()
            shortcuts.get_by_role('button', name='drop.html', exact=True).drag_to(
                editor, target_position={'x': 150, 'y': 160})
            expect(contents).to_have_value('<p>dragged from Desktop</p>')
            expect(p.locator('.desktop-window.app-browser')).to_have_count(0)
            assert (peer.root / 'drag.html').is_file() and (home_desktop / 'drop.html').is_file()
            placement.evaluate('el => el.remove()')
            titlebar.get_by_role('button', name='Toggle maximize', exact=True).click()
            for number in range(12):
                choose_file(f'sample-{number:02}.txt', double=True, filesystem=False)
                expect(contents).to_be_enabled()
            p.set_viewport_size(dict(width=720, height=650))
            more = editor.get_by_role('button', name='All tabs', exact=True)
            more.click()
            tabs = editor.get_by_role('group', name='All tabs', exact=True)
            expect(tabs).to_be_visible()
            expect(tabs.get_by_role('button')).to_have_count(13)
            menu_height = tabs.evaluate('menu => [menu.scrollHeight, menu.clientHeight]')
            assert menu_height[0] > menu_height[1], menu_height
            tabs.get_by_role('button', name='drop.html', exact=True).click()
            expect(tabs).to_be_hidden()
            expect(contents).to_have_value('<p>dragged from Desktop</p>')
            assert not errors, errors
    print(json.dumps(dict(output=str(out), checks=[
        'Desktop icons fill a column before growing right, with no vertical scroll or visible scrollbar',
        'The shared picker confirms selection and opens HTML directly in Text Editor',
        'The source view tracks the caret, both scroll axes, multiline tokens and undoable auto-indent',
        'Enter inserts blank lines, replaces a selection, and saves typed line breaks with the original BOM and CRLF',
        'File, Edit and View menus work above the tab strip, with a scrollable tab picker when space runs out',
        'New and Save as write the selected draft and confirm replacement before overwriting',
        'Dragging files from Files and Desktop opens them directly without moving their originals',
        'Markdown renders headings, emphasis, tables and code offline with raw HTML shown as text',
        'Source editing, preview, minimize and Ctrl+S retain the draft and source encoding',
        'Switching tabs preserves each draft and preview mode, and saving targets the original tab',
        'Closing a dirty tab or the editor window prompts before discarding',
    ]), indent=2))


if __name__ == '__main__':
    main()
