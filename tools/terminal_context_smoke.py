"""Owned WebView/SSH checks for the terminal menu and direct multi-tab controls."""
import hashlib
import json
import re
import time

from desktop_fixture import Desktop, Peer, ROOT, eventually, expect


def main():
    output = ROOT / 'artifacts/terminal-context-ui' / f'run-{time.time_ns()}'
    output.mkdir(parents=True)
    local = output / 'local'
    local.mkdir()
    checks = []
    geometry = []

    with Peer(output / 'peer') as peer:
        config = peer.config([peer.profile('Terminal context menu')], local)
        with Desktop(output / 'desktop', config) as app:
            page = app.page
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            app.connect()
            app.application('Terminal')
            surface = page.locator('.terminal-surface')
            reader = surface.locator('.xterm-accessibility-tree')
            keyboard = surface.locator('.xterm-helper-textarea')
            tabs = page.locator('.terminal-tabs .terminal-tab')
            new_tab = page.locator('.terminal-new-tab')
            menu = page.locator('.terminal-context-menu')
            status = page.locator('.terminal-status')

            def metadata():
                return json.loads(page.locator('.terminal-state').text_content())

            def running():
                expect(status).to_have_text('Interactive SSH')
                expect(reader).to_contain_text('Arcnix terminal fixture')
                expect(keyboard).to_be_visible()

            def command(value):
                keyboard.focus()
                expect(keyboard).to_be_focused()
                page.keyboard.type(value)
                page.keyboard.press('Enter')

            def exact_lines(value):
                return sum(line.strip() == value for line in reader.inner_text().splitlines())

            def open_menu(target=None, bottom=False):
                target = target if target is not None else surface
                box = target.bounding_box()
                assert box and box['width'] > 0 and box['height'] > 0
                # Keep the point inside the terminal, clear of the window resize handle.
                position = dict(x=box['width'] - 14, y=box['height'] - 14) if bottom else dict(
                    x=min(180, box['width'] / 2), y=min(120, box['height'] / 2),
                )
                target.click(button='right', position=position)
                expect(menu).to_be_visible()

            def action(name):
                menu.get_by_role('button', name=name, exact=True).click()
                expect(menu).not_to_be_visible()

            def menu_bounds(label):
                viewport = page.viewport_size
                def placed():
                    box = menu.bounding_box()
                    return box and box['x'] >= 7 and box['y'] >= 7 and (
                        box['x'] + box['width'] <= viewport['width'] - 7
                        and box['y'] + box['height'] <= viewport['height'] - 7
                    )
                eventually(placed)
                box = menu.bounding_box()
                assert box and viewport
                assert box['x'] >= 7 and box['y'] >= 7, (label, box)
                assert box['x'] + box['width'] <= viewport['width'] - 7, (label, box)
                assert box['y'] + box['height'] <= viewport['height'] - 7, (label, box)
                expect(menu).to_have_css('position', 'fixed')
                background = menu.evaluate('element => getComputedStyle(element).backgroundColor')
                assert background not in ['transparent', 'rgba(0, 0, 0, 0)'], (label, background)
                geometry.append(dict(label=label, viewport=viewport, menu=box, background=background))
                return background

            def select_output(value):
                # Select the actual terminal-rendered output using the pointer,
                # rather than manufacturing selection state in the bridge.
                info = reader.evaluate('''(element, value) => ({
                    count: element.children.length,
                    index: [...element.children].findIndex(row => row.textContent.trim() === value)
                })''', value)
                assert info['index'] >= 0, 'Expected fixture output row is absent'
                box = surface.locator('.xterm-screen').bounding_box()
                cell = surface.locator('.xterm').evaluate('''element => {
                    const canvas = document.createElement('canvas');
                    const context = canvas.getContext('2d');
                    const style = getComputedStyle(element);
                    context.font = style.fontSize + ' ' + style.fontFamily;
                    return context.measureText('W').width;
                }''')
                y = box['y'] + (info['index'] + .5) * box['height'] / info['count']
                page.mouse.move(box['x'] + 1, y)
                page.mouse.down()
                page.mouse.move(box['x'] + cell * len(value) + 1, y, steps=10)
                page.mouse.up()
                open_menu()
                expect(menu.get_by_role('button', name='Copy', exact=True)).to_be_enabled()
                page.keyboard.press('Escape')
                expect(menu).not_to_be_visible()

            running()
            keyboard.focus()
            page.keyboard.type('checksum key-')
            cdp = page.context.new_cdp_session(page)
            cdp.send('Input.insertText', {'text': 'abc'})
            process_key = {'key': 'Process', 'windowsVirtualKeyCode': 229}
            cdp.send('Input.dispatchKeyEvent', {'type': 'rawKeyDown', **process_key})
            cdp.send('Input.insertText', {'text': 'xyz'})
            cdp.send('Input.dispatchKeyEvent', {'type': 'keyUp', **process_key})
            for commit in ('pin', '拼', None):
                cdp.send('Input.imeSetComposition', {'text': 'pin', 'selectionStart': 3, 'selectionEnd': 3})
                expect(surface.locator('.composition-view')).to_have_text('pin')
                if commit is None:
                    cdp.send('Input.imeSetComposition', {'text': '', 'selectionStart': 0, 'selectionEnd': 0})
                else:
                    cdp.send('Input.insertText', {'text': commit})
                expect(surface.locator('.composition-view')).to_be_hidden()
            cdp.detach()
            page.keyboard.type('-end')
            page.keyboard.press('Enter')
            digest = hashlib.sha256('key-abcxyzpin拼-end'.encode()).hexdigest()
            expect(reader).to_contain_text(digest)
            checks.append('The remote PTY receives ordinary keys, direct text and committed IME text exactly once, and receives no cancelled preedit')
            page.keyboard.type('checksum /')
            page.keyboard.press('Control+/')
            page.keyboard.type('42')
            page.keyboard.press('Enter')
            expect(reader).to_contain_text(hashlib.sha256(b'/\x1f42').hexdigest())
            checks.append('The remote PTY receives a literal slash, Ctrl+/ as one Ctrl+_, and the following line number')
            expect(page.locator('.terminal-controls .help-trigger')).to_have_count(0)
            command('burst')
            expect(reader).to_contain_text('BURST COMPLETE', timeout=30000)
            scrollbar = surface.locator('.scrollbar.vertical')
            page.mouse.move(1, 1)
            expect(scrollbar).to_have_class(re.compile(r'\binvisible\b'))
            expect(scrollbar).to_have_css('opacity', '1')
            thumb = scrollbar.locator('.slider').bounding_box()
            track = scrollbar.bounding_box()
            x = thumb['x'] + thumb['width'] / 2
            page.mouse.move(x, thumb['y'] + thumb['height'] / 2)
            page.mouse.down()
            page.mouse.move(x, track['y'] + thumb['height'] / 2, steps=8)
            page.mouse.up()
            expect(reader).not_to_contain_text('BURST COMPLETE')
            checks.append('The terminal title bar has no help button and the idle scrollbar can be dragged to earlier output')
            expect(tabs).to_have_count(1)
            expect(new_tab).to_be_visible()
            expect(new_tab).to_be_enabled()
            original_id = metadata()['id']
            command('echo ORIGINAL_TAB_MARKER')
            expect(reader).to_contain_text('ORIGINAL_TAB_MARKER')
            open_menu()
            for name in ['Copy', 'Paste', 'Select all', 'Clear display', 'New terminal tab', 'Rename tab', 'Close tab']:
                expect(menu.get_by_role('button', name=name, exact=True)).to_be_visible()
            expect(menu.get_by_role('button', name='Copy', exact=True)).to_be_disabled()
            expect(menu.get_by_role('button', name='Paste', exact=True)).to_be_enabled()
            shortcuts = re.sub(r'\s+', '', menu.inner_text())
            for shortcut in ['Ctrl+Shift+C', 'Ctrl+Shift+V', 'Ctrl+Shift+T']:
                assert shortcut in shortcuts, shortcut
            light_background = menu_bounds('light-wide')
            app.screenshot('terminal-menu-light.png')
            page.keyboard.press('Escape')
            expect(menu).not_to_be_visible()
            expect(keyboard).to_be_focused()
            checks.append('The terminal uses a styled application context menu with live action availability and visible shortcut hints; Escape restores terminal focus')

            before_interrupt = reader.inner_text().count('^C')
            keyboard.focus()
            page.keyboard.press('Control+c')
            eventually(lambda: reader.inner_text().count('^C') > before_interrupt)
            expect(menu).not_to_be_visible()
            checks.append('Ctrl+C without a selection reaches the remote PTY as an interrupt')

            token = 'CONTEXT_COPY_TOKEN'
            command('echo ' + token)
            eventually(lambda: exact_lines(token) == 1)
            select_output(token)
            open_menu()
            expect(menu.get_by_role('button', name='Copy', exact=True)).to_be_enabled()
            action('Copy')
            keyboard.focus()
            page.keyboard.type('echo ')
            open_menu()
            action('Paste')
            expect(reader).to_contain_text('echo ' + token)
            page.wait_for_timeout(250)
            assert exact_lines(token) == 1, 'Context paste submitted input without Enter'
            keyboard.focus()
            page.keyboard.press('Enter')
            eventually(lambda: exact_lines(token) == 2)
            shortcut_token = 'SHORTCUT_COPY_TOKEN'
            command('echo ' + shortcut_token)
            eventually(lambda: exact_lines(shortcut_token) == 1)
            select_output(shortcut_token)
            keyboard.focus()
            page.keyboard.press('Control+Shift+c')
            keyboard.focus()
            page.keyboard.type('echo ')
            page.keyboard.press('Control+Shift+v')
            expect(reader).to_contain_text('echo ' + shortcut_token)
            page.wait_for_timeout(250)
            assert exact_lines(shortcut_token) == 1, 'Shortcut paste submitted input without Enter'
            page.keyboard.press('Enter')
            eventually(lambda: exact_lines(shortcut_token) == 2)
            checks.append('Pointer selection copies through the menu and Ctrl+Shift+C; menu and shortcut paste insert exact clipboard text without sending Enter')

            open_menu()
            action('Select all')
            open_menu()
            expect(menu.get_by_role('button', name='Copy', exact=True)).to_be_enabled()
            page.keyboard.press('Escape')
            expect(menu).not_to_be_visible()
            before_interrupt = reader.inner_text().count('^C')
            keyboard.focus()
            page.keyboard.press('Control+c')
            page.wait_for_timeout(200)
            assert reader.inner_text().count('^C') == before_interrupt, 'Selected Ctrl+C interrupted the remote process'
            open_menu()
            action('Clear display')
            expect(reader).not_to_contain_text('ORIGINAL_TAB_MARKER')
            command('echo FIRST_SESSION_STILL_ALIVE')
            expect(reader).to_contain_text('FIRST_SESSION_STILL_ALIVE')
            checks.append('Select all and selected Ctrl+C copy locally, while Clear display preserves the live SSH session')

            new_tab.click()
            expect(tabs).to_have_count(2)
            running()
            items = page.locator('.terminal-tab-item')
            transparent = 'rgba(0, 0, 0, 0)'
            expect(items.last.locator('.terminal-tab-close')).to_be_visible()
            expect(items.last).not_to_have_css('background-color', transparent)
            expect(items.first).to_have_css('background-color', transparent)
            second_id = metadata()['id']
            assert second_id != original_id
            command('echo SECOND_TAB_MARKER')
            expect(reader).to_contain_text('SECOND_TAB_MARKER')
            tabs.first.click()
            expect(reader).to_contain_text('FIRST_SESSION_STILL_ALIVE')
            expect(items.first).not_to_have_css('background-color', transparent)
            expect(items.last).to_have_css('background-color', transparent)
            expect(reader).not_to_contain_text('SECOND_TAB_MARKER')
            assert metadata()['id'] == original_id
            keyboard.focus()
            page.keyboard.press('Control+Tab')
            eventually(lambda: metadata()['id'] == second_id)
            expect(reader).to_contain_text('SECOND_TAB_MARKER')
            page.keyboard.press('Control+Shift+Tab')
            eventually(lambda: metadata()['id'] == original_id)
            expect(reader).to_contain_text('FIRST_SESSION_STILL_ALIVE')
            page.keyboard.press('Control+Shift+t')
            expect(tabs).to_have_count(3)
            running()
            third_id = metadata()['id']
            assert third_id not in [original_id, second_id]
            command('echo THIRD_TAB_MARKER')
            expect(reader).to_contain_text('THIRD_TAB_MARKER')
            page.keyboard.press('Control+Shift+w')
            expect(tabs).to_have_count(2)
            eventually(lambda: metadata()['id'] in [original_id, second_id])
            expect(reader).not_to_contain_text('THIRD_TAB_MARKER')
            checks.append('The direct new-tab button and Ctrl+Shift+T create independent PTYs; Ctrl+Tab/Ctrl+Shift+Tab switch retained history and Ctrl+Shift+W closes only the active tab')

            tabs.first.click()
            original_title = tabs.first.inner_text()
            open_menu(tabs.nth(1))
            assert metadata()['id'] == original_id, 'A background tab menu changed the active terminal'
            action('Rename tab')
            rename = page.get_by_role('dialog', name='Rename terminal tab', exact=True)
            expect(rename).to_be_visible()
            rename.get_by_role('textbox', name='Tab name', exact=True).fill('自定义 worker tab')
            rename.get_by_role('button', name='Rename', exact=True).click()
            expect(rename).not_to_be_visible()
            expect(tabs.nth(1)).to_have_text('自定义 worker tab')
            expect(page.locator('.terminal-tab.active')).to_have_text(original_title)
            assert metadata()['id'] == original_id
            tabs.nth(1).click()
            expect(reader).to_contain_text('SECOND_TAB_MARKER')
            expect(tabs).to_have_count(2)
            tabs.first.click()
            open_menu(tabs.nth(1))
            action('Close tab')
            expect(tabs).to_have_count(1)
            assert metadata()['id'] == original_id
            expect(reader).to_contain_text('FIRST_SESSION_STILL_ALIVE')
            checks.append('A background tab can be renamed and closed through its own menu without redirecting actions to or restarting the active terminal')

            keyboard.focus()
            page.keyboard.press('Shift+F10')
            expect(menu).to_be_visible()
            focused = menu.locator('button:focus')
            expect(focused).to_be_visible()
            expect(focused).to_be_enabled()
            page.keyboard.press('End')
            expect(menu.get_by_role('button', name='Close tab', exact=True)).to_be_focused()
            page.keyboard.press('Home')
            expect(menu.locator('button:focus')).to_be_enabled()
            page.keyboard.press('ArrowDown')
            expect(menu.locator('button:focus')).to_be_enabled()
            page.keyboard.press('Escape')
            expect(menu).not_to_be_visible()
            expect(keyboard).to_be_focused()
            keyboard.focus()
            page.keyboard.press('Control+Shift+a')
            open_menu()
            expect(menu.get_by_role('button', name='Copy', exact=True)).to_be_enabled()
            page.keyboard.press('Escape')
            expect(menu).not_to_be_visible()
            expect(keyboard).to_be_focused()
            page.keyboard.press('Control+Shift+l')
            expect(reader).not_to_contain_text('FIRST_SESSION_STILL_ALIVE')
            command('echo AFTER_SHORTCUT_CLEAR')
            expect(reader).to_contain_text('AFTER_SHORTCUT_CLEAR')
            checks.append('Shift+F10 opens a keyboard-navigable terminal menu, Escape restores focus, and select-all/clear shortcuts retain the interactive session')

            page.set_viewport_size(dict(width=1000, height=700))
            open_menu(bottom=True)
            menu_bounds('light-narrow')
            app.screenshot('terminal-menu-narrow.png')
            page.keyboard.press('Escape')
            app.settings('General')
            page.locator('.setting-theme:visible').get_by_role('radio', name='Dark', exact=True).check()
            app.servers()
            app.application('Terminal')
            expect(reader).to_contain_text('AFTER_SHORTCUT_CLEAR')
            open_menu(bottom=True)
            dark_background = menu_bounds('dark-narrow')
            assert dark_background != light_background, 'Terminal menu ignored the selected theme'
            assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
            app.screenshot('terminal-menu-dark.png')
            page.keyboard.press('Escape')
            checks.append('The same terminal menu follows light/dark themes and stays inside a narrow viewport without horizontal overflow')

            open_menu()
            action('Close tab')
            expect(tabs).to_have_count(0)
            expect(page.locator('.desktop-window.app-terminal:visible')).to_have_count(0)
            expect(page.locator('.desktop-dock .app-terminal.running')).to_have_count(0)
            app.application('Terminal')
            expect(tabs).to_have_count(1)
            running()
            expect(reader).not_to_contain_text('AFTER_SHORTCUT_CLEAR')
            command('pwd')
            expect(reader).to_contain_text('/fixture')
            assert not errors, errors
            checks.append('Closing the last tab closes Terminal and reopening it starts a fresh shell')

    result = dict(passed=len(checks), checks=checks, geometry=geometry, output=str(output))
    (output / 'results.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(result, ensure_ascii=False), flush=True)


if __name__ == '__main__':
    main()
