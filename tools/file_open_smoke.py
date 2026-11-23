"""Exercise remote file associations, the app chooser and real WebM playback."""
import json
import re
import subprocess
import time

from desktop_fixture import Desktop, Peer, ROOT, eventually, expect


def main():
    out = ROOT / 'artifacts/file-open' / f'run-{time.time_ns()}'
    with Peer(out / 'peer') as peer:
        webm = peer.root / 'oceans.WEBM'
        subprocess.run([
            'ffmpeg', '-nostdin', '-loglevel', 'error', '-f', 'lavfi',
            '-i', 'color=c=blue:s=320x180:r=15', '-t', '3', '-c:v', 'libvpx',
            '-y', str(webm),
        ], check=True)
        subprocess.run([
            'ffmpeg', '-nostdin', '-loglevel', 'error', '-i', str(webm), '-c:v', 'mpeg4',
            '-y', str(peer.root / 'native.avi'),
        ], check=True)
        (peer.root / 'notes.customnote').write_text('Unrecognized extension, valid text')
        (peer.root / 'unknown-name').write_text('No extension')
        (peer.root / 'page.html').write_text(
            '<!doctype html><h1 id="preview">Rendered HTML</h1>'
            '<script>document.querySelector("h1").textContent = "Script ran"</script>'
        )
        (peer.root / '.bashrc').write_text('# Shell configuration')
        (peer.root / 'source.ts').write_text('const greeting: string = "hello";')
        (peer.root / 'misc.webvideo').write_bytes(webm.read_bytes())
        (peer.root / 'report.reportdoc').write_text('<h1>Custom report</h1>')
        config = peer.config([peer.profile('File associations')], out,
                             file_associations={'legacyopen': {'default': 'code'}})
        with Desktop(out / 'desktop', config,
                     extra_env={'WEBVIEW2_USER_DATA_FOLDER': ''}) as app:
            page = app.page
            app.connect()
            chooser = page.get_by_role('dialog', name='Open with', exact=True)
            menu = page.locator('.file-context-menu:visible')
            submenu = menu.locator('.file-open-submenu')
            player = page.locator('.desktop-window.app-player')

            def associations():
                return json.loads(app.config.read_text())['config']['settings'].get('file_associations', {})

            def select_app(app_name, *, always=False):
                expect(chooser).to_be_visible()
                chooser.get_by_role('button', name=app_name, exact=True).click()
                chooser.get_by_role('button', name='Set as default and open' if always else 'Open once', exact=True).click()
                expect(chooser).to_be_hidden()

            def extension_row(ext):
                return page.locator('.file-association-row:visible').filter(
                    has=page.locator('.file-association-extension', has_text=re.compile(rf'^\.{re.escape(ext)}$')))

            def search_extensions(value):
                page.get_by_role('textbox', name='Search extensions', exact=True).fill(value)

            def associated_app(dialog, name):
                return dialog.locator('.file-associated-app').filter(
                    has=page.get_by_text(name, exact=True))

            def add_app(app_name):
                association.get_by_role('button', name='Add another app', exact=True).click()
                apps = association.get_by_role('group', name='Apps to add', exact=True)
                confirm = association.get_by_role('button', name='Add app', exact=True)
                expect(confirm).to_be_disabled()
                apps.get_by_role('button', name=app_name, exact=True).click()
                expect(confirm).to_be_enabled()
                confirm.click()
                expect(apps).to_be_hidden()
                expect(associated_app(association, app_name)).to_contain_text('Added manually')

            def right_click(name):
                app.workspace('Transfer files')
                app.file(name).click(button='right')
                expect(menu).to_be_visible()

            def show_submenu():
                menu.get_by_role('button', name='Open with', exact=True).hover()
                expect(submenu).to_be_visible()
                return submenu

            right_click(webm.name)
            expect(submenu).to_be_hidden()
            show_submenu()
            expect(submenu.get_by_role('button', name='Player', exact=True)).to_be_visible()
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_have_count(0)
            page.keyboard.press('Escape')
            app.file(webm.name).dblclick()
            expect(player).to_be_visible()
            video = player.locator('video')
            eventually(lambda: video.evaluate('video => video.readyState >= 2 && video.videoWidth === 320'))
            eventually(lambda: associations().get('webm', {}).get('opened_with') == ['player'])
            expect(page.locator('.desktop-window.app-code')).to_have_count(0)

            app.workspace('Transfer files')
            app.file('native.avi').dblclick()
            eventually(lambda: json.loads(player.locator('.mpv-state').text_content())['media']['loaded'])

            right_click('notes.customnote')
            show_submenu()
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_have_count(0)
            app.action('Choose another app')
            select_app('Code Editor')
            expect(app.code().locator('.view-lines')).to_contain_text('Unrecognized extension')
            eventually(lambda: associations().get('customnote', {}).get('opened_with') == ['code'])
            right_click('notes.customnote')
            show_submenu()
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_be_visible()
            expect(submenu.get_by_role('button', name='Player', exact=True)).to_have_count(0)
            page.keyboard.press('Escape')

            app.file('unknown-name').dblclick()
            expect(chooser.get_by_role('button', name='Set as default and open', exact=True)).to_have_count(0)
            select_app('Code Editor')
            expect(app.code().locator('.view-lines')).to_contain_text('No extension')
            right_click('unknown-name')
            show_submenu()
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_have_count(0)
            page.keyboard.press('Escape')
            right_click('.bashrc')
            show_submenu()
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_be_visible()
            page.keyboard.press('Escape')

            right_click('source.ts')
            show_submenu()
            expect(submenu.get_by_role('button', name='Text Editor', exact=True)).to_be_visible()
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_be_visible()
            expect(submenu.get_by_role('button', name='Player', exact=True)).to_be_visible()
            app.action('Text Editor')
            contents = page.get_by_role('dialog', name='Text Editor', exact=True).get_by_role(
                'textbox', name='File contents', exact=True)
            expect(contents).to_have_value(re.compile('const greeting'))
            contents.press('Control+End')
            contents.press('Enter')
            contents.press_sequentially('// second line')
            contents.press('Control+s')
            eventually(lambda: (peer.root / 'source.ts').read_bytes() ==
                       b'const greeting: string = "hello";\n// second line')
            page.get_by_role('dialog', name='Text Editor', exact=True).get_by_role(
                'button', name='Close dialog', exact=True,
            ).click()

            right_click('misc.webvideo')
            show_submenu()
            expect(submenu.get_by_role('button', name='Player', exact=True)).to_have_count(0)
            show_submenu().get_by_role('button', name='Choose another app', exact=True).click()
            select_app('Player', always=True)
            eventually(lambda: associations().get('webvideo', {}).get('default') == 'player')
            eventually(lambda: associations().get('webvideo', {}).get('opened_with') == ['player'])
            right_click('misc.webvideo')
            show_submenu()
            expect(submenu.get_by_role('button', name='Player', exact=True)).to_be_visible()
            page.keyboard.press('Escape')

            right_click('page.html')
            expect(menu.locator('.file-menu-group').first.get_by_role('button')).to_have_text(
                ['Open with'])
            show_submenu()
            expect(submenu.get_by_role('button', name='Browser', exact=True)).to_be_visible()
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_be_visible()
            app.action('Browser')
            expect(page.locator('.desktop-window.app-browser.active')).to_be_visible()
            def preview_tab():
                page.wait_for_timeout(100)
                return next((
                    tab for context in app.browser.contexts for tab in context.pages
                    if tab.url.startswith('http://arcnixpreview.localhost/')
                ), None)
            preview = eventually(preview_tab)
            expect(preview.locator('#preview')).to_have_text('Rendered HTML')

            app.application('Settings')
            desktop_settings = page.locator('.desktop-settings-page')
            desktop_settings.get_by_role('button', name='Default apps', exact=True).click()
            expect(extension_row('webm').locator('.file-association-current')).to_have_text('Player')
            expect(extension_row('legacyopen').locator('.file-association-current')).to_have_text('Code Editor')
            search_extensions('.m2ts')
            extension_row('m2ts').click()
            association = page.get_by_role('dialog', name='File association', exact=True)
            expect(associated_app(association, 'Player')).to_contain_text('Built-in support')
            association.get_by_role('button', name='Add another app', exact=True).click()
            apps = association.get_by_role('group', name='Apps to add', exact=True)
            expect(apps.get_by_role('button')).to_have_text(['Text Editor', 'Code Editor', 'Image Viewer', 'PDF Reader', 'Browser'])
            apps.get_by_role('button', name='Code Editor', exact=True).click()
            association.get_by_role('button', name='Cancel', exact=True).click()
            expect(apps).to_be_hidden()
            assert 'm2ts' not in associations()
            add_app('Code Editor')
            eventually(lambda: associations().get('m2ts') == {'added_apps': ['code']})
            expect(associated_app(association, 'Player')).to_contain_text('Default app')
            association.get_by_role('button', name='Add another app', exact=True).click()
            expect(apps.get_by_role('button')).to_have_text(['Text Editor', 'Image Viewer', 'PDF Reader', 'Browser'])
            association.get_by_role('button', name='Done', exact=True).click()
            search_extensions('.HTML')
            expect(extension_row('html').locator('.file-association-current')).to_have_text('Browser')
            expect(extension_row('html').locator('.file-association-other-apps')).to_contain_text('Code Editor')
            expect(extension_row('html').locator('.file-association-other-apps')).to_contain_text('Text Editor')
            extension_row('html').click()
            associated_app(association, 'Code Editor').get_by_role('button', name='Use as default', exact=True).click()
            eventually(lambda: associations().get('html', {}).get('default') == 'code')
            association.get_by_role('button', name='Done', exact=True).click()
            app.settings('Files')
            expect(extension_row('html').locator('.file-association-current')).to_have_text('Code Editor')
            expect(extension_row('html').locator('.file-association-other-apps')).to_contain_text('Browser')
            search_extensions('legacyopen')
            extension_row('legacyopen').click()
            association.get_by_role('button', name='Reset', exact=True).click()
            eventually(lambda: associations().get('legacyopen') == {'added_apps': ['code']})
            association.get_by_role('button', name='Done', exact=True).click()

            search_extensions('.reportdoc')
            expect(extension_row('reportdoc')).to_have_count(0)
            page.get_by_role('button', name='Add extension', exact=True).click()
            expect(association.get_by_role('textbox', name='File extension', exact=True)).to_have_value('.reportdoc')
            add_app('Browser')
            add_app('Code Editor')
            associated_app(association, 'Code Editor').get_by_role('button', name='Use as default', exact=True).click()
            eventually(lambda: associations().get('reportdoc') == {
                'default': 'code', 'added_apps': ['code', 'browser']})
            association.get_by_role('button', name='Done', exact=True).click()
            expect(extension_row('reportdoc').locator('.file-association-other-apps')).to_have_text('Browser')

            app.servers()
            right_click('report.reportdoc')
            show_submenu()
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_be_visible()
            expect(submenu.get_by_role('button', name='Browser', exact=True)).to_be_visible()
            page.keyboard.press('Escape')
            app.workspace('Transfer files')
            app.file('page.html').dblclick()
            expect(page.locator('.desktop-window.app-code.active')).to_be_visible()
            expect(app.code().locator('.view-lines')).to_contain_text('Rendered HTML')
            right_click('page.html')
            show_submenu().get_by_role('button', name='Choose another app', exact=True).click()
            select_app('Browser')
            expect(page.locator('.desktop-window.app-browser.active')).to_be_visible()
            assert associations().get('html', {}).get('default') == 'code'

            app.application('Files')
            html_file = app.file('page.html')
            html_file.press('Shift+F10')
            groups = menu.locator(':scope > .file-menu-group')
            expect(groups).to_have_count(5)
            expect(groups.nth(0).get_by_role('button')).to_have_text(
                ['Open with'])
            expect(groups.nth(1).get_by_role('button')).to_have_text(
                ['Copy', 'Duplicate', 'Make alias', 'Compress', 'Paste', 'Rename'])
            expect(groups.nth(2).get_by_role('button')).to_have_text(
                ['Copy selected paths', 'Properties', 'Terminal here'])
            expect(groups.nth(3).locator('.file-color-swatch')).to_have_count(8)
            expect(groups.nth(3).get_by_role('button', name='Customize folder…', exact=True)).to_have_count(0)
            expect(groups.nth(4).get_by_role('button')).to_have_text(
                ['Move to Trash', 'Delete permanently'])
            assert groups.nth(1).evaluate(
                'group => parseFloat(getComputedStyle(group).borderBlockStartWidth) > 0')
            expect(menu.get_by_role('button', name='Open', exact=True)).to_have_count(0)
            trigger = menu.get_by_role('button', name='Open with', exact=True)
            expect(trigger).to_be_focused()
            page.keyboard.press('ArrowRight')
            expect(submenu.get_by_role('button', name='Code Editor', exact=True)).to_be_focused()
            page.keyboard.press('End')
            expect(submenu.get_by_role('button', name='Choose another app', exact=True)).to_be_focused()
            page.keyboard.press('ArrowLeft')
            expect(trigger).to_be_focused()
            expect(submenu).to_be_hidden()
            page.keyboard.press('Escape')
            expect(html_file).to_be_focused()
            page.locator('.app-files.active .window-maximize').click()
            file_row = html_file.locator('..')
            row_box = file_row.bounding_box()
            file_row.click(button='right', position={'x': row_box['width'] - 12, 'y': row_box['height'] / 2})
            show_submenu()
            sub_box = submenu.bounding_box()
            parent_box = menu.bounding_box()
            assert sub_box['x'] + sub_box['width'] <= parent_box['x'] + 2
            page.keyboard.press('Escape')

            app.stop()
            app.launch()
            page = app.page
            association = page.get_by_role('dialog', name='File association', exact=True)
            app.connect()
            app.file('page.html').dblclick()
            expect(page.locator('.desktop-window.app-code.active')).to_be_visible()
            app.settings('Files')
            search_extensions('reportdoc')
            expect(extension_row('reportdoc').locator('.file-association-current')).to_have_text('Code Editor')
            expect(extension_row('reportdoc').locator('.file-association-other-apps')).to_have_text('Browser')
            extension_row('reportdoc').click()
            associated_app(association, 'Code Editor').get_by_role('button', name='Remove association', exact=True).click()
            eventually(lambda: associations().get('reportdoc') == {'added_apps': ['browser']})
            association.get_by_role('button', name='Done', exact=True).click()
            expect(extension_row('reportdoc').locator('.file-association-current')).to_have_text('Ask which app to use')
            search_extensions('html')
            extension_row('html').click()
            association.get_by_role('button', name='Reset', exact=True).click()
            eventually(lambda: 'default' not in associations().get('html', {}))
            association.get_by_role('button', name='Done', exact=True).click()
            expect(extension_row('html').locator('.file-association-current')).to_have_text('Browser')
    print('WebM playback, app chooser, learned shortcuts, HTML preview and shared persistent defaults passed.')


if __name__ == '__main__':
    main()
