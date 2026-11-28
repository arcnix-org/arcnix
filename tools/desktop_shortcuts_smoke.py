"""Exercise desktop shortcut routing and preference writes in the native WebView."""
import re
import json
import time

from file_context_smoke import copied
from server_apps_fixture import read_desktop_settings
from desktop_fixture import Desktop, Peer, ROOT, eventually, expect


def check_search(panel):
    search = panel.get_by_role('textbox', name='Search shortcuts', exact=True)
    results = panel.locator('.desktop-key-binding:visible')
    groups = panel.locator('.desktop-shortcut-group:visible')
    expect(groups).to_have_count(4)
    search.fill('tErMiNaL copy')
    expect(groups).to_have_count(1)
    expect(results).to_have_count(2)
    search.fill('ctrl + tab')
    expect(results).to_have_count(2)
    expect(results.first).to_contain_text('Switch to next app')
    search.fill('zzzz-no-shortcut')
    expect(groups).to_have_count(0)
    expect(panel.get_by_text('No matching shortcuts.', exact=True)).to_be_visible()
    panel.get_by_role('button', name='Clear search', exact=True).click()
    expect(search).to_be_focused()
    expect(search).to_have_value('')
    expect(groups).to_have_count(4)


def main():
    out = ROOT / 'artifacts' / 'desktop-shortcuts' / f'run-{time.time_ns()}'
    out.mkdir(parents=True)
    with Peer(out / 'peer') as peer, Desktop(
        out / 'desktop', peer.config([peer.profile('Shortcut test')], out,
                                     file_associations={'txt': {'default': 'code'}}),
    ) as app:
        (peer.root / '.shortcut-example').touch()
        (peer.root / 'shortcut-test.txt').write_text('initial', encoding='utf-8')
        page = app.page
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        preferences = peer.root / 'home/arcnix/.arcnix/desktop.json'

        def bindings():
            return read_desktop_settings(preferences).get('shortcuts', {}).get('windows', {}).get('overrides', {})

        def disabled_shortcuts():
            return read_desktop_settings(preferences).get('shortcuts', {}).get('windows', {}).get('disabled', [])

        def settings():
            app.application('Settings')
            page.locator('.desktop-settings-tabs').get_by_role(
                'button', name='Keyboard shortcuts', exact=True,
            ).click()
            return page.locator('.desktop-custom-keys')

        def record(action, chord, operation='Change'):
            button = page.get_by_role('button', name=f'{operation} shortcut: {action}', exact=True)
            button.click()
            expect(button).to_have_attribute('aria-pressed', 'true')
            page.keyboard.press(chord)
            expect(button).not_to_have_attribute('aria-pressed', 'true')
            return button

        def title(label):
            expect(page.locator('.desktop-window.active .desktop-window-title')).to_have_text(label)

        app.connect()
        app.application('Terminal')
        terminal_input = page.locator('.terminal-surface .xterm-helper-textarea')
        expect(terminal_input).to_be_visible()
        monitor = page.locator('.desktop-dock-app.app-monitor')
        monitor.hover()
        monitor.evaluate("""button => button.addEventListener('pointerdown', () => {
            button.centerError = new Promise(resolve => {
                const start = performance.now(), icon = button.querySelector('.desktop-app-icon');
                let worst = 0;
                function sample() {
                    const a = button.getBoundingClientRect(), b = icon.getBoundingClientRect();
                    worst = Math.max(worst, Math.abs((a.left+a.right-b.left-b.right)/2),
                        Math.abs((a.top+a.bottom-b.top-b.bottom)/2));
                    if (performance.now() - start < 400) requestAnimationFrame(sample);
                    else resolve(worst);
                }
                sample();
            });
        }, {once: true})""")
        page.mouse.down()
        expect(monitor).to_have_css('outline-style', 'none')
        page.mouse.up()
        title('Monitor')
        assert monitor.evaluate("button => button.centerError") < 1
        page.keyboard.press('Shift')
        expect(monitor).to_have_css('outline-style', 'none')
        page.mouse.move(25, 100)
        expect(monitor.locator('.desktop-app-label')).to_be_hidden()
        monitor.press('Shift+Tab')
        keyboard_launcher = page.locator('.desktop-dock button:focus-visible')
        expect(keyboard_launcher).to_be_focused()
        expect(keyboard_launcher).to_have_css('outline-style', 'none')
        expect(keyboard_launcher.locator('.desktop-app-label')).to_be_visible()
        page.locator('.host-select').focus()
        page.keyboard.press('Control+Shift+Tab')
        title('Terminal')
        expect(page.locator('.desktop-canvas')).to_be_focused()
        page.keyboard.press('Control+Tab')
        title('Monitor')
        page.keyboard.press('Control+Shift+ArrowUp')
        expect(page.locator('.desktop-window.active')).to_have_class(re.compile(r'\bmaximized\b'))
        page.keyboard.press('Control+Shift+Tab')
        title('Terminal')
        page.keyboard.press('Control+Tab')
        title('Monitor')
        expect(page.locator('.desktop-window.active')).to_have_class(re.compile(r'\bmaximized\b'))
        page.keyboard.press('Control+Shift+ArrowUp')
        expect(page.locator('.desktop-window.active')).not_to_have_class(re.compile(r'\bmaximized\b'))
        page.keyboard.press('Control+Shift+ArrowDown')
        expect(page.locator('.desktop-window.app-monitor')).to_be_hidden()
        title('Terminal')
        page.keyboard.press('Control+Tab')
        title('Monitor')
        page.keyboard.press('Control+Shift+Tab')
        title('Terminal')
        expect(page.locator('.desktop-canvas')).to_be_focused()
        page.keyboard.press('Enter')
        expect(terminal_input).to_be_focused()

        panel = settings()
        check_search(panel)
        tabs = page.locator('.desktop-settings-tabs')
        capture = panel.get_by_role('checkbox', name='Use Windows shortcuts on the desktop')
        if capture.count():
            expect(capture).to_be_checked()
            capture.click()
            eventually(lambda: not json.loads(app.config.read_text(encoding='utf-8'))['config']['settings']['capture_desktop_shortcuts'])
            capture.click()
            expect(capture).to_be_checked()
        expect(tabs.get_by_role('button').last).to_have_text('About')
        tabs.get_by_role('button', name='About', exact=True).click()
        about = page.locator('.desktop-settings-page .about-details')
        expect(about).to_contain_text(re.compile(r'Version \d+\.\d+\.\d+'))
        expect(about.get_by_role('button', name='Project on GitHub', exact=True)).to_be_visible()
        expect(page.locator('.desktop-settings-page .update-panel')).to_have_count(0)
        expect(page.locator('.desktop-settings-page .device-info-value')).to_have_count(0)
        tabs.get_by_role('button', name='General', exact=True).click()
        def detail(label):
            return page.locator('.desktop-settings-page .desktop-setting-row').filter(
                has=page.get_by_text(label, exact=True)).locator('.device-info-value')
        expect(detail('Operating system')).to_have_text('Fixture Linux')
        expect(detail('OS version')).to_have_text('42')
        expect(detail('CPU model')).to_have_text('Fixture CPU')
        expect(detail('Physical cores')).to_have_text('4')
        expect(detail('Logical processors')).to_have_text('8')
        expect(detail('Memory')).to_have_text('8.6 GB')
        expect(detail('GPU model')).to_have_text('Unavailable')
        tabs.get_by_role('button', name='Dock', exact=True).click()
        page.get_by_role('slider', name='Dock icon size').focus()
        page.keyboard.press('Control+Tab')
        title('Terminal')
        page.keyboard.press('Control+Shift+Tab')
        title('Settings')
        tabs.get_by_role('button', name='Keyboard shortcuts', exact=True).click()
        fullscreen = 'Toggle desktop fullscreen'
        button = record(fullscreen, 'Control+Tab')
        expect(panel.locator('.desktop-shortcut-error')).to_have_text('This shortcut is already in use.')
        expect(button).to_contain_text('Ctrl+Shift+F')
        search = panel.get_by_role('textbox', name='Search shortcuts')
        search.fill('Ctrl+Shift+F')
        button = record(fullscreen, 'Control+l')
        expect(button).to_be_hidden()
        search.fill('Ctrl+L')
        expect(button).to_be_visible()
        search.fill('')
        expect(button).to_contain_text('Ctrl+L')
        eventually(lambda: bindings().get('fullscreen') == 'Ctrl+L')
        previous_app = panel.get_by_role('checkbox', name='Switch to previous app', exact=True)
        previous_app.uncheck()
        eventually(lambda: 'previous_app' in disabled_shortcuts())
        expect(previous_app).not_to_be_checked()
        expect(panel.get_by_role('button', name='Change shortcut: Switch to previous app', exact=True)).to_contain_text('Ctrl+Shift+Tab')
        page.keyboard.press('Control+Shift+Tab')
        title('Settings')
        page.locator('.desktop-canvas').focus()
        page.keyboard.press('Control+l')
        exit_fullscreen = page.locator('.desktop-fullscreen-exit')
        expect(exit_fullscreen).to_be_visible()
        for name in ('Terminal', 'Monitor', 'Settings'):
            page.keyboard.press('Control+Tab')
            title(name)
        expect(exit_fullscreen).to_be_visible()
        record(fullscreen, 'Escape')
        expect(button).to_contain_text('Ctrl+L')
        expect(exit_fullscreen).to_be_visible()
        page.keyboard.press('Control+l')
        expect(exit_fullscreen).to_have_count(0)

        app.application('Terminal')
        expect(page.locator('.terminal-status')).to_have_text('Interactive SSH')
        terminal_input.focus()
        page.keyboard.press('Control+Tab')
        title('Terminal')
        terminal_input.focus()
        page.evaluate("""() => {
            window.remoteControlL = false;
            document.querySelector('.terminal-input-bridge').addEventListener('input', event => {
                const packet = JSON.parse(event.target.value);
                if (packet.kind === 'data' && packet.data.includes(String.fromCharCode(12))) {
                    window.remoteControlL = true;
                }
            });
        }""")
        page.keyboard.press('Control+l')
        eventually(lambda: page.evaluate('window.remoteControlL'))
        expect(exit_fullscreen).to_have_count(0)
        expect(terminal_input).to_be_focused()
        page.locator('.desktop-window.active .desktop-window-mark').click()
        page.keyboard.press('Control+Tab')
        title('Monitor')
        app.application('Terminal')
        page.locator('.desktop-window.active .window-maximize').focus()
        page.keyboard.press('Control+l')
        expect(exit_fullscreen).to_be_visible()
        page.keyboard.press('Control+l')
        expect(exit_fullscreen).to_have_count(0)

        def restore_group(panel, name):
            group = panel.locator('.desktop-shortcut-group').filter(has=page.get_by_text(name, exact=True))
            return group.locator('.settings-card-heading').get_by_role('button', name=f'Restore default shortcuts for {name}', exact=True)

        app.stop()
        app.launch()
        page = app.page
        exit_fullscreen = page.locator('.desktop-fullscreen-exit')
        page.on('pageerror', lambda error: errors.append(str(error)))
        app.connect()
        panel = settings()
        expect(panel.get_by_role('button', name=f'Change shortcut: {fullscreen}', exact=True)).to_contain_text('Ctrl+L')
        previous_app = panel.get_by_role('checkbox', name='Switch to previous app', exact=True)
        expect(previous_app).not_to_be_checked()
        restore_fullscreen = panel.get_by_role('button', name=f'Restore shortcut: {fullscreen} (Default: Ctrl+Shift+F)', exact=True)
        fullscreen_row = panel.locator('.desktop-key-binding').filter(has=page.get_by_role('checkbox', name=fullscreen, exact=True))
        expect(fullscreen_row.get_by_text('Customized', exact=True)).to_be_visible()
        page.keyboard.press('Control+l')
        expect(exit_fullscreen).to_be_visible()
        restore_fullscreen.click()
        eventually(lambda: not bindings())
        expect(restore_fullscreen).to_have_count(0)
        expect(previous_app).not_to_be_checked()
        expect(panel.get_by_role('button', name=f'Change shortcut: {fullscreen}', exact=True)).to_contain_text('Ctrl+Shift+F')
        restore_group(panel, 'Desktop').click()
        eventually(lambda: not disabled_shortcuts())
        expect(previous_app).to_be_checked()
        page.keyboard.press('Control+Shift+F')
        expect(exit_fullscreen).to_have_count(0)
        expect(page.get_by_text('Saved on server', exact=True)).to_have_count(0)
        focus = record('Move focus forward', 'Control+Alt+j')
        shortcut_state = page.locator('.desktop-shortcut-state')
        eventually(lambda: json.loads(shortcut_state.text_content())['bindings']['focus_next'] == ['Ctrl+Alt+J'])
        expect(focus).to_be_focused()
        page.keyboard.press('Tab')
        expect(focus).to_be_focused()
        page.keyboard.press('Control+Alt+j')
        expect(focus).not_to_be_focused()
        restore_group(panel, 'Desktop').click()
        record('Minimize window', 'Meta+l')
        expect(panel.locator('.desktop-shortcut-error')).to_have_text('This shortcut is reserved by Windows.')
        record('Open tab', 'Control+Shift+w')
        expect(panel.locator('.desktop-shortcut-error')).to_have_text('This shortcut is already in use.')
        record('Open tab', 'Control+Shift+y')
        eventually(lambda: bindings().get('terminal_new_tab') == 'Ctrl+Shift+Y')
        app.application('Terminal')
        terminal_input = page.locator('.terminal-surface .xterm-helper-textarea')
        terminal_input.focus()
        terminal_tabs = page.locator('.terminal-tab')
        count = terminal_tabs.count()
        page.keyboard.press('Control+Shift+y')
        expect(terminal_tabs).to_have_count(count + 1)
        panel = settings()
        restore_group(panel, 'Terminal').click()
        eventually(lambda: 'terminal_new_tab' not in bindings())
        expect(panel.get_by_role('button', name='Change shortcut: Open tab')).to_contain_text('Ctrl+Shift+T')
        expect(restore_group(panel, 'Terminal')).to_be_disabled()
        record('Toggle hidden files', 'Control+Shift+h')
        eventually(lambda: bindings().get('files_hidden') == 'Ctrl+Shift+H')
        app.application('Files')
        hidden = app.file('.shortcut-example')
        expect(hidden).to_be_visible()
        page.keyboard.press('Control+Shift+h')
        expect(hidden).to_have_count(0)
        page.keyboard.press('Control+Shift+h')
        expect(hidden).to_be_visible()
        settings()
        record('Save', 'Control+Alt+s')
        eventually(lambda: bindings().get('code_save') == 'Ctrl+Alt+S')
        app.application('Files')
        app.file('shortcut-test.txt').dblclick()
        editor = app.replace_code('saved using a remapped shortcut\n')
        editor.press('Control+s')
        page.wait_for_timeout(300)
        assert (peer.root / 'shortcut-test.txt').read_text(encoding='utf-8') == 'initial'
        editor.press('Control+Alt+s')
        eventually(lambda: (peer.root / 'shortcut-test.txt').read_text(encoding='utf-8') == 'saved using a remapped shortcut\n')
        record_button = settings()
        restore_group(record_button, 'Code Editor').click()
        eventually(lambda: 'code_save' not in bindings())
        assert bindings().get('files_hidden') == 'Ctrl+Shift+H'
        restore_group(record_button, 'Files').click()
        eventually(lambda: not bindings())
        panel = settings()
        scheme = panel.locator('.setting-shortcut-scheme:visible')
        save = panel.get_by_role('button', name='Change shortcut: Save', exact=True)
        expect(scheme.get_by_role('radio', checked=True)).to_have_accessible_name('Auto')
        expect(save).to_contain_text('Ctrl+S')
        scheme.get_by_role('radio', name='Windows', exact=True).check()
        record('Toggle desktop fullscreen', 'F7', 'Add')
        scheme.get_by_role('radio', name='macOS', exact=True).check()
        expect(save).to_contain_text('Cmd+S')
        expect(panel.get_by_role('button', name='Change shortcut: New tab', exact=True)).to_contain_text('Cmd+T')
        app.application('Files')
        files_tabs = page.locator('.app-files.active .files-tab')
        expect(files_tabs.first).to_be_visible()
        count = files_tabs.count()
        page.keyboard.press('Control+t')
        expect(files_tabs).to_have_count(count)
        page.keyboard.press('Meta+t')
        expect(files_tabs).to_have_count(count + 1)
        sample = app.file('shortcut-test.txt')
        sample.click(button='right')
        menu = page.locator('.file-context-menu:visible')
        for label, chord in [('Copy selected paths', 'Option+Cmd+C'), ('Properties', 'Cmd+I'),
                             ('Rename', 'Return'), ('Move to Trash', 'Cmd+Delete')]:
            expect(menu.get_by_role('button', name=label, exact=True).locator('.menu-key-hint')).to_have_text(chord)
        page.keyboard.press('Escape')
        sample.click()
        sample.press('Alt+Meta+c')
        copied('/shortcut-test.txt')
        sample.press('Meta+i')
        info = page.get_by_role('dialog', name='Properties', exact=True)
        expect(info).to_contain_text('shortcut-test.txt')
        info.get_by_role('button', name='Close dialog', exact=True).click()
        expect(info).to_be_hidden()
        sample.click()
        sample.press('Enter')
        expect(page.locator('.file-inline-name:visible')).to_have_value('shortcut-test.txt')
        page.keyboard.press('Escape')
        panel = settings()
        expect(panel.get_by_text('F7', exact=True)).to_have_count(0)
        expect(panel.get_by_role('button', name='Change shortcut: Show desktop', exact=True)).to_contain_text('Ctrl+Shift+D')
        page.locator('.desktop-canvas').focus()
        page.keyboard.press('Control+Shift+d')
        expect(page.locator('.desktop-window:visible')).to_have_count(0)
        page.keyboard.press('Control+Shift+d')
        title('Settings')
        record('Toggle desktop fullscreen', 'F8', 'Add')
        record('Toggle desktop fullscreen (F8)', 'F9')
        record('Save', 'Control+Alt+m')
        record('Save', 'F6', 'Add')
        app.application('Code Editor')
        area = app.replace_code('saved with a macOS secondary binding')
        area.press('F6')
        eventually(lambda: (peer.root / 'shortcut-test.txt').read_text() == 'saved with a macOS secondary binding')
        panel = settings()
        layout = page.locator('.desktop-shortcut-state')
        eventually(lambda: json.loads(layout.text_content())['bindings']['fullscreen'] == ['Ctrl+Meta+F', 'F9'])
        page.keyboard.press('F9')
        expect(page.locator('.desktop-fullscreen-exit')).to_be_visible()
        page.keyboard.press('F9')
        expect(page.locator('.desktop-fullscreen-exit')).to_have_count(0)
        profiles = lambda: read_desktop_settings(preferences).get('shortcuts', {})
        eventually(lambda: profiles().get('macos', {}).get('overrides', {}).get('code_save') == 'Ctrl+Alt+M')
        assert profiles()['windows']['additions']['fullscreen'] == ['F7']
        assert profiles()['macos']['additions']['fullscreen'] == ['F9']
        app.stop()
        with Desktop(out / 'other-client', peer.config([peer.profile('Other client')], out),
                     runtime=app.runtime) as other:
            other.connect()
            other.application('Settings')
            other.page.locator('.desktop-settings-tabs').get_by_role('button', name='Keyboard shortcuts').click()
            other_keys = other.page.locator('.desktop-custom-keys')
            expect(other_keys.locator('.setting-shortcut-scheme:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Auto')
            expect(other_keys.get_by_role('button', name='Change shortcut: Toggle desktop fullscreen (F7)')).to_be_visible()
            other_keys.locator('.setting-shortcut-scheme:visible').get_by_role('radio', name='macOS', exact=True).check()
            expect(other_keys.get_by_role('button', name='Change shortcut: Toggle desktop fullscreen (F9)')).to_be_visible()
            expect(other_keys.get_by_role('button', name='Change shortcut: Save', exact=True)).to_contain_text('Ctrl+Option+M')
        app.launch()
        page = app.page
        app.connect()
        panel = settings()
        tabs = page.locator('.desktop-settings-tabs')
        expect(panel.locator('.setting-shortcut-scheme:visible').get_by_role('radio', checked=True)).to_have_accessible_name('macOS')
        expect(panel.get_by_role('button', name='Change shortcut: Save', exact=True)).to_contain_text('Ctrl+Option+M')
        panel.locator('.setting-shortcut-scheme:visible').get_by_role('radio', name='Auto', exact=True).check()
        expect(panel.get_by_role('button', name='Change shortcut: Save', exact=True)).to_contain_text('Ctrl+S')
        expect(panel.get_by_role('button', name='Change shortcut: Toggle desktop fullscreen (F7)', exact=True)).to_be_visible()
        panel.locator('.desktop-shortcut-extra').filter(has_text='F7').get_by_role('button', name='Remove shortcut').click()
        eventually(lambda: not profiles()['windows']['additions'])
        assert profiles()['macos']['additions']['fullscreen'] == ['F9']
        panel.locator('.setting-shortcut-scheme:visible').scroll_into_view_if_needed()
        app.screenshot('desktop-shortcuts.png')
        assert not errors, errors
        print('Desktop About, Ctrl+Tab in maximized and fullscreen modes, terminal focus, recording and persistence passed.')


if __name__ == '__main__':
    main()
