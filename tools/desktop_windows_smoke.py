"""Window geometry and Files preferences through the real WebView and SSH peer."""
import re
import ctypes
from ctypes import wintypes
import json
import time
from server_apps_fixture import read_desktop_settings
from desktop_fixture import Desktop, Peer, ROOT, expect, eventually


def native_window(pid):
    user32 = ctypes.windll.user32
    handles = []
    callback = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    @callback
    def collect(hwnd, _):
        owner = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and user32.IsWindowVisible(wintypes.HWND(hwnd)):
            title = ctypes.create_unicode_buffer(128)
            user32.GetWindowTextW(wintypes.HWND(hwnd), title, len(title))
            if title.value.startswith('Arcnix'):
                handles.append(hwnd)
        return True

    user32.EnumWindows(collect, 0)
    assert len(handles) == 1, handles
    return wintypes.HWND(handles[0])


def native_bounds(hwnd):
    rect = wintypes.RECT()
    assert ctypes.windll.user32.GetWindowRect(hwnd, ctypes.byref(rect))
    return tuple(getattr(rect, edge) for edge in ('left', 'top', 'right', 'bottom'))


def monitor_bounds(hwnd):
    class MonitorInfo(ctypes.Structure):
        _fields_ = [('size', wintypes.DWORD), ('monitor', wintypes.RECT),
                    ('work', wintypes.RECT), ('flags', wintypes.DWORD)]

    user32 = ctypes.windll.user32
    user32.MonitorFromWindow.restype = wintypes.HANDLE
    monitor = wintypes.HANDLE(user32.MonitorFromWindow(hwnd, 2))
    info = MonitorInfo(ctypes.sizeof(MonitorInfo))
    assert user32.GetMonitorInfoW(monitor, ctypes.byref(info))
    return tuple(getattr(info.monitor, edge) for edge in ('left', 'top', 'right', 'bottom'))


def main():
    out = ROOT / 'artifacts/desktop-windows' / f'run-{time.time_ns()}'
    out.mkdir(parents=True)
    local = out / 'local'
    local.mkdir()
    with Peer(out / 'peer') as peer:
        home = peer.root / 'home/arcnix'
        desktop_path = '/home/arcnix/Desktop'
        for directory in ('Desktop', 'Documents', 'Downloads'):
            (home / directory).mkdir(parents=True, exist_ok=True)
        (peer.root / 'Work space 中文').mkdir()
        (home / 'Desktop/desktop-file.txt').write_text('desktop file')
        remote = home / '.arcnix/desktop.json'
        remote.parent.mkdir(parents=True)
        disk_layout = dict(x=38, y=51, width=720, height=570)
        remote.write_text(json.dumps(dict(version=1, theme='Dark', windows={'disks': {'bounds': disk_layout}})))
        with Desktop(out / 'desktop', peer.config([peer.profile('Window preferences')], local)) as app:
            p = app.page
            p.emulate_media(reduced_motion='reduce')
            errors = []
            p.on('pageerror', lambda error: errors.append(str(error)))

            def prefs():
                return read_desktop_settings(remote)

            def settled():
                p.wait_for_function("""() => !document.documentElement.classList.contains('desktop-motion')
                    && !document.querySelector('.desktop-window.active')?.getAnimations().length""")

            def bounds():
                return p.locator('.desktop-window.active').bounding_box()

            def close_inset():
                return p.locator('.desktop-window.active').evaluate("""win =>
                    win.getBoundingClientRect().right - win.querySelector('.window-close').getBoundingClientRect().right
                """)

            def drag(locator, dx, dy, cancel=False, expected=None):
                box = locator.bounding_box()
                x, y = box['x'] + box['width'] / 2, box['y'] + box['height'] / 2
                p.mouse.move(x, y)
                hit = p.evaluate('([x,y])=>document.elementFromPoint(x,y)?.className',[x,y])
                if 'edge-' in locator.evaluate('node=>node.className'):
                    assert 'desktop-resize-handle' in hit, (box,hit,bounds())
                drag_from(x, y, dx, dy, cancel, expected)

            def drag_from(x, y, dx, dy, cancel=False, expected=None):
                p.mouse.move(x, y)
                p.mouse.down()
                p.mouse.move(x + dx, y + dy, steps=12)
                if expected is not None:
                    assert same(bounds(), expected), (bounds(), expected)
                if cancel:
                    p.keyboard.press('Escape')
                p.mouse.up()

            def same(a, b):
                return all(abs(a[key] - b[key]) <= 2 for key in ('x', 'y', 'width', 'height'))

            def work_area():
                canvas = p.locator('.desktop-canvas').bounding_box()
                dock = p.locator('.desktop-dock-zone').bounding_box()
                return dict(canvas, height=dock['y'] - canvas['y'])

            def projected(window, reference, area=None):
                area = area or work_area()
                result = {}
                for axis, size in [('x', 'width'), ('y', 'height')]:
                    center = window[axis] - reference[axis] + window[size] / 2
                    target = center / reference[size] * area[size] - window[size] / 2
                    result[size] = min(window[size], area[size])
                    result[axis] = area[axis] + min(max(0, target), area[size] - result[size])
                return same(bounds(), result)

            app.connect()
            app.application('Files')
            expect(p.locator('.desktop-resize-handle:visible')).to_have_count(8)
            settled()
            original = bounds()
            reference = work_area()
            p.set_viewport_size(dict(width=1800, height=1100))
            eventually(lambda: projected(original, reference))
            p.set_viewport_size(dict(width=760, height=650))
            eventually(lambda: projected(original, reference))
            p.set_viewport_size(dict(width=1440, height=980))
            eventually(lambda: same(bounds(), original))
            drag(p.locator('.desktop-window.active .edge-se'), -250, -170)
            assert abs(bounds()['width'] - (original['width'] - 250)) <= 2, (bounds(),original,errors)
            assert abs(bounds()['height'] - (original['height'] - 170)) <= 2
            shrunk = bounds()
            titlebar = p.locator('.desktop-window.active .desktop-window-titlebar')
            expect(titlebar).to_have_css('cursor', 'default')
            drag(titlebar.locator('.desktop-window-mark'), 90, 45)
            moved = bounds()
            assert abs(moved['x'] - shrunk['x'] - 90) <= 2
            assert abs(moved['y'] - shrunk['y'] - 45) <= 2
            drag(p.locator('.desktop-window.active .edge-nw'), -14, -12)
            positioned = bounds()
            assert abs(positioned['x'] - moved['x'] + 14) <= 2
            assert abs(positioned['y'] - moved['y'] + 12) <= 2
            assert abs(positioned['width'] - moved['width'] - 14) <= 2
            drag(p.locator('.desktop-window.active .edge-e'), 50, 0, cancel=True)
            assert same(positioned, bounds())
            eventually(lambda: prefs().get('windows', {}).get('files', {}).get('bounds'))
            eventually(lambda: abs(prefs()['windows']['files']['bounds']['width'] - positioned['width']) <= 2)
            geometry = prefs()['windows']['files']['bounds']

            p.locator('.desktop-window.active .window-maximize').click()
            settled()
            expect(p.locator('.desktop-window.active')).to_have_class(re.compile(r'\bmaximized\b'))
            expect(p.locator('.desktop-resize-handle:visible')).to_have_count(0)
            maximized = bounds()
            p.set_viewport_size(dict(width=900, height=700))
            eventually(lambda: bounds()['width'] < maximized['width'] and bounds()['height'] < maximized['height'])
            p.set_viewport_size(dict(width=1440, height=980))
            eventually(lambda: same(bounds(), maximized))
            app.application('Monitor')
            settled()
            expect(p.locator('.desktop-window.active')).not_to_have_class(re.compile(r'\bmaximized\b'))
            monitor_before = bounds()
            assert abs(monitor_before['width'] - disk_layout['width']) <= 2, monitor_before
            assert abs(monitor_before['height'] - disk_layout['height']) <= 2, monitor_before
            p.set_viewport_size(dict(width=1800, height=1100))
            eventually(lambda: projected(monitor_before, reference))
            enlarged_area = work_area()
            before_drag = bounds()
            drag(p.locator('.desktop-window.active .edge-s'), 0, -65)
            after_drag = dict(before_drag, height=before_drag['height'] - 65)
            eventually(lambda: same(bounds(), after_drag))
            eventually(lambda: prefs()['windows'].get('monitor', {}).get('bounds', {}).get('height') == 505)
            p.set_viewport_size(dict(width=1440, height=980))
            eventually(lambda: projected(after_drag, enlarged_area))

            p.set_viewport_size(dict(width=710, height=820))
            eventually(lambda: bounds()['width'] < 480 and projected(after_drag, enlarged_area))
            narrow = bounds()
            narrow_area = work_area()
            monitor_title = p.locator('.desktop-window.active .desktop-window-mark')
            overhang = dict(narrow, x=narrow['x'] + 21, y=narrow['y'] + 11)
            inset = close_inset()
            drag(monitor_title, 21, 11, expected=overhang)
            eventually(lambda: same(bounds(), overhang))
            assert abs(close_inset() - inset) < 1

            def narrow_snapshot():
                saved = prefs()['windows']['monitor']['bounds']
                reference = saved['reference']
                return (same(saved, dict(overhang, x=overhang['x'] - narrow_area['x'],
                                         y=overhang['y'] - narrow_area['y']))
                        and abs(reference['width'] - narrow_area['width']) <= 1
                        and abs(reference['height'] - narrow_area['height']) <= 1)

            eventually(narrow_snapshot)
            p.set_viewport_size(dict(width=720, height=820))
            eventually(lambda: abs(bounds()['x'] - overhang['x']) <= 1
                       and abs(bounds()['width'] - overhang['width']) <= 1
                       and 0 < bounds()['x'] + bounds()['width'] - work_area()['x'] - work_area()['width'] < 21)
            before_resize = bounds()
            slightly_wider = dict(before_resize, x=before_resize['x'] - 6, width=before_resize['width'] + 6)
            drag(p.locator('.desktop-window.active .edge-w'), -6, 0, expected=slightly_wider)
            eventually(lambda: same(bounds(), slightly_wider))
            slightly_lower = dict(slightly_wider, y=slightly_wider['y'] + 6)
            drag(monitor_title, 0, 6, expected=slightly_lower)
            eventually(lambda: same(bounds(), slightly_lower))

            p.set_viewport_size(dict(width=1440, height=980))
            app.application('Files')
            settled()
            expect(p.locator('.desktop-window.active')).to_have_class(re.compile(r'\bmaximized\b'))
            p.locator('.desktop-window.active .window-maximize').click()
            settled()
            eventually(lambda: same(positioned, bounds()))
            p.set_viewport_size(dict(width=1920, height=1200))
            eventually(lambda: projected(positioned, reference))
            p.set_viewport_size(dict(width=760, height=650))
            eventually(lambda: projected(positioned, reference))
            assert prefs()['windows']['files']['bounds'] == geometry
            p.set_viewport_size(dict(width=1440, height=980))
            eventually(lambda: same(positioned, bounds()))

            hwnd = native_window(app.process.pid)
            native_before = native_bounds(hwnd)
            screen = monitor_bounds(hwnd)
            assert native_before != screen
            p.locator('.desktop-fullscreen-toggle').click()
            eventually(lambda: native_bounds(hwnd) == screen)
            p.keyboard.press('Escape')
            eventually(lambda: native_bounds(hwnd) == native_before)
            assert same(positioned, bounds())
            assert prefs()['windows']['files']['bounds'] == geometry

            sidebar = p.locator('.files-sidebar')
            sidebar.get_by_role('button', name='Desktop', exact=True).click()
            expect(app.file('desktop-file.txt')).to_be_visible()
            pane = p.locator('.files-pane')
            address = p.locator('.files-window-tools .pathbar')
            expect(p.locator('.files-pane .pathbar, .files-titlebar .path-edit')).to_have_count(0)
            root_crumb = address.locator('.breadcrumb[aria-label="/"]')
            root_crumb.dblclick()
            expect(address.locator('.path-input')).to_be_focused()
            expect(address.locator('.path-input')).to_have_value(desktop_path)
            p.keyboard.press('Escape')
            expect(address.locator('.breadcrumb.current')).to_have_text('Desktop')
            root_crumb.focus()
            p.keyboard.press('Control+l')
            expect(address.locator('.path-input')).to_be_focused()
            p.keyboard.press('Escape')
            star = address.locator('.path-location + .files-favorite')
            star.click()
            expect(star).to_have_attribute('aria-pressed', 'true')
            expect(sidebar.get_by_role('button', name=f'Open favorite {desktop_path}', exact=True)).to_be_visible()
            favorites = sidebar.locator('.files-favorites')
            folder_favorite = favorites.locator(f'.files-favorite-row[aria-label="{desktop_path}"]')
            file_path = f'{desktop_path}/desktop-file.txt'
            file_row = pane.locator(f'.file-row[aria-label="{file_path}"]')
            file_row.drag_to(folder_favorite, target_position={'x': 40, 'y': 3})
            eventually(lambda: prefs()['files']['favorites'] == [file_path, desktop_path])
            file_favorite = favorites.locator(f'.files-favorite-row[aria-label="{file_path}"]')
            folder_favorite.drag_to(file_favorite, target_position={'x': 40, 'y': 3})
            eventually(lambda: prefs()['files']['favorites'] == [desktop_path, file_path])
            file_row.drag_to(folder_favorite, target_position={'x': 40, 'y': 3})
            eventually(lambda: prefs()['files']['favorites'] == [file_path, desktop_path])

            file_icon = file_row.locator('.file-entry-icon')
            file_color = file_icon.evaluate('icon => getComputedStyle(icon).color')
            app.file('desktop-file.txt').click(button='right')
            palette = p.locator('.file-context-menu .file-color-menu')
            palette.get_by_role('button', name='Green', exact=True).focus()
            p.keyboard.press('ArrowRight')
            expect(palette.get_by_role('button', name='Blue', exact=True)).to_be_focused()
            p.keyboard.press('Enter')
            expect(file_row.locator('.folder-color-dot')).to_have_css('background-color', 'rgb(91, 153, 236)')
            expect(file_icon).to_have_css('color', file_color)
            root_crumb.click()
            expect(address.locator('.breadcrumb.current')).to_have_attribute('aria-label', '/')
            sidebar.get_by_role('button', name='Home', exact=True).click()
            app.file('Desktop').click(button='right')
            palette.get_by_role('button', name='Purple', exact=True).click()
            expect(folder_favorite.locator('.tag-purple')).to_be_visible()
            folder_row = pane.locator(f'.file-row[aria-label="{desktop_path}"]')
            folder_icon = folder_row.locator('.file-entry-icon.glyph-folder')
            expect(folder_row.locator('.folder-color-dot')).to_have_css('background-color', 'rgb(165, 128, 235)')
            expect(folder_icon).to_have_css('color', 'rgb(165, 128, 235)')
            app.file('Desktop').click(button='right')
            expect(palette.get_by_role('button', name='Purple', exact=True)).to_have_attribute('aria-pressed', 'true')
            app.action('Customize folder…')
            customize = p.get_by_role('dialog', name='Customize folder', exact=True)
            title = customize.locator('.dialog-title')
            original_dialog = customize.bounding_box()
            drag(title, -72, 20)
            moved_dialog = dict(original_dialog, x=original_dialog['x'] - 72, y=original_dialog['y'] + 20)
            assert same(customize.bounding_box(), moved_dialog), (customize.bounding_box(), moved_dialog)
            drag(title, 45, -15, cancel=True)
            assert same(customize.bounding_box(), moved_dialog)
            drag(title, -1600, -1000)
            frame = p.locator('.desktop-modal-backdrop').bounding_box()
            assert same(customize.bounding_box(), dict(original_dialog, x=frame['x'], y=frame['y']))
            customize.get_by_role('button', name='Photos', exact=True).click()
            expect(customize.locator('.folder-icon-preview .folder-icon-photos')).to_be_visible()
            customize.get_by_role('button', name='Done', exact=True).click()
            expect(folder_row.locator('.folder-icon-photos')).to_be_visible()
            eventually(lambda: prefs().get('files', {}).get('folder_icons', {}).get(desktop_path) == 'photos')
            view_options = p.locator('.desktop-window.active').get_by_role('button', name='File view options', exact=True)
            view_options.click()
            p.locator('.file-actions-menu').get_by_role('button', name='Icon grid', exact=True).click()
            expect(pane).to_have_class(re.compile(r'\bicons-view\b'))
            expect(folder_row.locator('.folder-icon-photos')).to_be_visible()
            view_options.click()
            p.locator('.file-actions-menu').get_by_role('button', name='Detailed list', exact=True).click()
            sidebar.get_by_role('button', name='Blue', exact=True).click()
            tags = p.locator('.files-tags-view')
            expect(pane).to_be_hidden()
            expect(tags.locator('.files-tag-item')).to_have_count(1)
            tags.get_by_role('button', name=file_path, exact=True).click()
            expect(p.locator('.app-editor.active .remote-editor-content')).to_have_value('desktop file')
            p.locator('.desktop-window.active .window-close').click()
            app.application('Files')
            sidebar.get_by_role('button', name='All tags', exact=True).click()
            expect(tags.locator('.files-tag-item')).to_have_count(2)
            tags.get_by_role('button', name=desktop_path, exact=True).click()
            expect(app.file('desktop-file.txt')).to_be_visible()
            toggle = p.locator('.files-sidebar-toggle')
            toggle.click()
            expect(sidebar).to_be_hidden()
            eventually(lambda: prefs().get('files', {}).get('sidebar_hidden') is True)

            p.locator('.desktop-window.active .window-close').click()
            settled()
            app.application('Files')
            settled()
            eventually(lambda: same(positioned, bounds()))
            app.stop()
            app.launch()
            p = app.page
            p.emulate_media(reduced_motion='reduce')
            p.on('pageerror', lambda error: errors.append(str(error)))
            app.connect()
            app.application('Files')
            settled()
            eventually(lambda: same(positioned, bounds()))
            expect(p.locator('.files-sidebar')).to_be_hidden()
            p.locator('.files-sidebar-toggle').click()
            sidebar = p.locator('.files-sidebar')
            expect(sidebar.locator(f'.files-favorite-row[aria-label="{desktop_path}"] .tag-purple')).to_be_visible()
            expect(sidebar.locator('.files-favorite-row').first).to_have_attribute('aria-label', file_path)
            sidebar.get_by_role('button', name=f'Open favorite {desktop_path}', exact=True).click()
            expect(app.file('desktop-file.txt')).to_be_visible()
            app.screenshot('restored-files.png')
            sidebar.get_by_role('button', name=f'Remove favorite {desktop_path}', exact=True).click()
            sidebar.get_by_role('button', name=f'Remove favorite {file_path}', exact=True).click()
            eventually(lambda: prefs()['files']['favorites'] == [])
            assert (home / 'Desktop/desktop-file.txt').is_file()
            assert prefs()['files']['colors'][desktop_path] == 'purple'
            assert prefs()['files']['colors'][file_path] == 'blue'
            assert prefs()['files']['folder_icons'][desktop_path] == 'photos'
            sidebar.get_by_role('button', name='Home', exact=True).click()
            expect(p.locator(f'.file-row[aria-label="{desktop_path}"] .folder-icon-photos')).to_be_visible()
            app.file('Desktop').click(button='right')
            app.action('Customize folder…')
            restored = p.get_by_role('dialog', name='Customize folder', exact=True)
            assert same(restored.bounding_box(), original_dialog)
            restored.get_by_role('button', name='Restore default folder icon', exact=True).click()
            restored.get_by_role('button', name='Done', exact=True).click()
            expect(p.locator(f'.file-row[aria-label="{desktop_path}"] .glyph-folder')).to_be_visible()
            eventually(lambda: 'folder_icons' not in prefs()['files'])
            area = work_area()
            original = bounds()
            titlebar = p.locator('.desktop-window.active .desktop-window-titlebar')
            drag(titlebar.locator('.desktop-window-mark'),
                 area['x'] - original['x'] - original['width'] + 25,
                 area['y'] - original['y'] - 55)
            top_left = bounds()
            assert top_left['x'] < area['x'] and top_left['y'] < area['y'], top_left
            assert abs(top_left['x'] + top_left['width'] - area['x'] - 64) <= 2, top_left
            assert abs(top_left['y'] + titlebar.bounding_box()['height'] - area['y'] - 19) <= 2, top_left
            eventually(lambda: prefs()['windows']['files']['bounds']['x'] < 0
                       and prefs()['windows']['files']['bounds']['y'] < 0)
            assert not p.evaluate('([x,y]) => !!document.elementFromPoint(x,y)?.closest(".desktop-window")',
                                  [area['x'] - 4, area['y'] + 8])
            app.screenshot('outside-top-left.png')

            app.stop()
            app.launch()
            p = app.page
            p.emulate_media(reduced_motion='reduce')
            p.on('pageerror', lambda error: errors.append(str(error)))
            app.connect()
            app.application('Files')
            settled()
            eventually(lambda: same(bounds(), top_left))
            # The titlebar padding at the far end remains draggable even when the
            # app title and the start of the bar are clipped by the desktop.
            drag_from(top_left['x'] + top_left['width'] - 13, area['y'] + 9,
                      original['x'] - top_left['x'], original['y'] - top_left['y'])
            eventually(lambda: same(bounds(), original))
            partly_hidden = dict(original, x=area['x'] - 40, y=area['y'] - 12)
            drag_from(original['x'] + 24, original['y'] + 20,
                      partly_hidden['x'] - original['x'], partly_hidden['y'] - original['y'])
            eventually(lambda: same(bounds(), partly_hidden))
            p.set_viewport_size(dict(width=1460, height=986))
            eventually(lambda: same(bounds(), dict(partly_hidden, x=area['x'] - 20, y=area['y'] - 6)))
            p.set_viewport_size(dict(width=1800, height=1200))
            eventually(lambda: projected(partly_hidden, area))
            assert bounds()['x'] > work_area()['x'] + 2 and bounds()['y'] > work_area()['y'] + 2
            p.set_viewport_size(dict(width=1440, height=980))
            eventually(lambda: same(bounds(), partly_hidden))
            drag_from(area['x'] + 24, area['y'] + 12,
                      original['x'] - partly_hidden['x'], original['y'] - partly_hidden['y'])
            eventually(lambda: same(bounds(), original))
            max_x = area['x'] + area['width'] - 64
            max_y = area['y'] + area['height'] - 18
            drag_from(original['x'] + 13, original['y'] + 11,
                      max_x - original['x'] + 12, max_y - original['y'] + 12)
            bottom_right = bounds()
            assert abs(bottom_right['x'] - max_x) <= 2, (original, bottom_right, max_x, max_y)
            assert abs(bottom_right['y'] - max_y) <= 2, bottom_right
            assert bottom_right['x'] + bottom_right['width'] > area['x'] + area['width'], bottom_right
            assert bottom_right['y'] + bottom_right['height'] > area['y'] + area['height'], bottom_right
            dock = p.locator('.desktop-dock-zone').bounding_box()
            floating_dock = p.locator('.desktop-dock').bounding_box()
            desktop = p.locator('.desktop-canvas').bounding_box()
            assert abs(desktop['y'] + desktop['height'] - floating_dock['y'] - floating_dock['height'] - 4) <= 1
            exposed_x = (max(bottom_right['x'], dock['x'] + dock['width']) + area['x'] + area['width']) / 2
            assert p.evaluate('''([x,y]) => !!document.elementFromPoint(x,y)?.closest('.app-files')''',
                              [exposed_x, dock['y'] + 12])
            drag_from(bottom_right['x'] + 13, bottom_right['y'] + 11,
                      original['x'] - bottom_right['x'], original['y'] - bottom_right['y'])
            eventually(lambda: same(bounds(), original))

            app.application('Settings')
            settled()
            settings = p.locator('.desktop-window.app-settings.active')
            search_button = settings.get_by_role('button', name='Search settings', exact=True)
            search = settings.get_by_role('textbox', name='Search settings', exact=True)
            sections = settings.locator('.desktop-settings-tabs')
            expect(search).to_have_count(0)
            search_button.click()
            expect(search).to_be_focused()
            search.fill('status bar')
            expect(sections.get_by_role('button')).to_have_text(['Dock'])
            sections.get_by_role('button', name='Dock', exact=True).click()
            expect(settings.locator('.desktop-settings-section-title')).to_have_text('Dock')
            expect(settings.get_by_role('switch', name='Show dock status bar', exact=True)).to_be_visible()
            search.fill('app icons')
            expect(sections.get_by_role('button')).to_have_text(['Desktop', 'Dock'])
            search.fill('unavailable setting')
            expect(sections.get_by_text('No matching settings')).to_be_visible()
            settings.get_by_role('button', name='Clear search', exact=True).click()
            expect(search).to_have_value('')
            expect(search).to_be_focused()
            expect(sections.get_by_role('button')).to_have_count(10)
            search_button.click()
            expect(search).to_have_count(0)
            assert settings.evaluate('''el => {
                const rect = selector => el.querySelector(selector).getBoundingClientRect();
                const sidebar = rect('.desktop-settings-sidebar');
                const heading = rect('.desktop-settings-sidebar-heading');
                const content = rect('.desktop-preferences');
                const card = rect('.desktop-settings-card');
                return Math.abs(sidebar.right - heading.right) < 1
                    && Math.abs(sidebar.right - content.left) < 1
                    && Math.abs(card.left - content.left - (content.right - card.right)) < 1;
            }''')
            settings.get_by_role('button', name='Desktop', exact=True).click()
            initial = bounds()
            area = work_area()
            right_margin = area['x'] + area['width'] - initial['x'] - initial['width']
            drag(settings.locator('.desktop-window-mark'), right_margin - 8, 0)
            preferred = bounds()
            saved_bounds = eventually(lambda: prefs().get('windows', {}).get('settings', {}).get('bounds'))

            def settings_fit():
                window = bounds()
                area = work_area()
                return (
                    window['x'] >= area['x'] - 1 and window['y'] >= area['y'] - 1
                    and window['x'] + window['width'] <= area['x'] + area['width'] + 1
                    and window['y'] + window['height'] <= area['y'] + area['height'] + 1
                )

            for width, height in [(1240, 980), (860, 980), (790, 520)]:
                p.set_viewport_size(dict(width=width, height=height))
                eventually(settings_fit)
                if height == 980:
                    assert abs(bounds()['height'] - preferred['height']) <= 2
                actions = settings.locator('.desktop-window-actions button')
                expect(actions).to_have_count(3)
                assert actions.evaluate_all('''buttons => buttons.every(button => {
                    const box = button.getBoundingClientRect();
                    return button.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
                })''')
                pane = settings.locator('.desktop-preferences')
                assert pane.evaluate('el => el.clientHeight > 0 && el.scrollWidth <= el.clientWidth + 1')
            expect(settings.locator('.desktop-settings-page')).to_have_css('flex-direction', 'row')
            settings.get_by_role('button', name='About', exact=True).click()
            expect(settings.locator('.about-details')).to_be_visible()
            p.set_viewport_size(dict(width=1440, height=980))
            eventually(lambda: same(bounds(), preferred))
            assert prefs()['windows']['settings']['bounds'] == saved_bounds

            settings.get_by_role('button', name='Dock', exact=True).click()
            settings.locator('.setting-dock-style:visible').get_by_role('radio', name='Panel mode', exact=True).check()
            for label, side in [('Bottom', 'bottom'), ('Right side', 'right'), ('Left side', 'left')]:
                settings.locator('.setting-dock-position:visible').get_by_role('radio', name=label, exact=True).check()
                position = ':not(.dock-side-zone)' if side == 'bottom' else f'.dock-{side}'
                dock = p.locator(f'.desktop-dock-zone.dock-panel-zone{position}')
                expect(dock).to_be_visible()

                layer = p.locator('.desktop-window-layer').bounding_box()
                before = settings.bounding_box()
                inset = close_inset()
                clipped = dict(before)
                if side == 'bottom':
                    clipped['y'] = layer['y'] + layer['height'] - 18
                    grip = (clipped['x'] + 13, clipped['y'] + 12)
                elif side == 'right':
                    clipped['x'] = layer['x'] + layer['width'] - 64
                    grip = (clipped['x'] + 13, clipped['y'] + 13)
                else:
                    clipped['x'] = layer['x'] + 64 - before['width']
                    grip = (clipped['x'] + clipped['width'] - 13, clipped['y'] + 13)
                dx, dy = clipped['x'] - before['x'], clipped['y'] - before['y']
                start_x = before['x'] + before['width'] - 7 if side == 'left' else grip[0] - dx
                drag_from(start_x, grip[1] - dy, dx, dy, expected=clipped)
                eventually(lambda: same(bounds(), clipped))
                assert abs(close_inset() - inset) < 1
                drag_from(*grip, -dx, -dy, expected=before)
                eventually(lambda: same(bounds(), before))
            left_dock = p.locator('.desktop-dock-zone.dock-left.dock-panel-zone')
            settings.locator('.window-close').click()
            app.show_desktop()
            app.application('Player')
            settled()
            player = p.locator('.desktop-window.app-player.active')

            def player_area():
                canvas = p.locator('.desktop-canvas').bounding_box()
                dock = left_dock.bounding_box()
                return dict(canvas, x=dock['x'] + dock['width'], width=canvas['width'] - dock['width'])

            initial = player.bounding_box()
            reference = player_area()
            clipped = dict(initial, x=reference['x'] + reference['width'] - initial['width'] + 90,
                           y=reference['y'] + reference['height'] - initial['height'] + 65)
            drag(player.locator('.desktop-window-mark'), clipped['x'] - initial['x'], clipped['y'] - initial['y'],
                 expected=clipped)
            eventually(lambda: same(bounds(), clipped))
            player_bounds = eventually(lambda: prefs().get('windows', {}).get('player', {}).get('bounds'))
            assert abs(player_bounds['reference']['width'] - reference['width']) <= 1
            assert abs(player_bounds['reference']['height'] - reference['height']) <= 1
            # The Player retains its full bounds for restoration. Its clipped
            # portion must stop receiving pointer hits outside the desktop.
            edge_x = reference['x'] + reference['width']
            assert p.evaluate('''([x, y]) => {
                const app = point => document.elementFromPoint(point, y)?.closest('.app-player');
                return !!app(x - 4) && !app(x + 4);
            }''', [edge_x, clipped['y'] + 80])
            p.set_viewport_size(dict(width=1485, height=1015))
            eventually(lambda: same(bounds(), clipped) and player_area()['width'] > reference['width'] + 30)
            expanded = player_area()
            assert 0 < clipped['x'] + clipped['width'] - expanded['x'] - expanded['width'] < 60
            assert 0 < clipped['y'] + clipped['height'] - expanded['y'] - expanded['height'] < 45
            p.set_viewport_size(dict(width=1530, height=1045))
            eventually(lambda: same(bounds(), clipped)
                       and abs(bounds()['x'] + bounds()['width'] - player_area()['x'] - player_area()['width']) <= 2
                       and abs(bounds()['y'] + bounds()['height'] - player_area()['y'] - player_area()['height']) <= 2)
            p.set_viewport_size(dict(width=1590, height=1150))
            eventually(lambda: projected(clipped, reference, player_area()))
            restored = bounds()
            expanded = player_area()
            assert restored['x'] >= clipped['x'] + 2 and restored['y'] >= clipped['y'] + 2
            assert restored['x'] + restored['width'] <= expanded['x'] + expanded['width'] + 1
            assert restored['y'] + restored['height'] <= expanded['y'] + expanded['height'] + 1
            p.set_viewport_size(dict(width=1840, height=1280))
            eventually(lambda: projected(clipped, reference, player_area()))
            assert bounds()['x'] > restored['x'] + 2 and bounds()['y'] > restored['y'] + 2

            p.set_viewport_size(dict(width=1370, height=920))
            eventually(lambda: 0 < bounds()['x'] - reference['x'] < clipped['x'] - reference['x'] - 2
                       and 0 < bounds()['y'] - reference['y'] < clipped['y'] - reference['y'] - 2
                       and abs(bounds()['width'] - clipped['width']) <= 1
                       and abs(bounds()['height'] - clipped['height']) <= 1)
            flush_width = round(1440 - (reference['width'] - clipped['width']))
            p.set_viewport_size(dict(width=flush_width, height=980))
            eventually(lambda: abs(bounds()['x'] - player_area()['x']) <= 3
                       and abs(bounds()['width'] - clipped['width']) <= 2
                       and abs(bounds()['height'] - clipped['height']) <= 1)
            p.set_viewport_size(dict(width=990, height=600))
            eventually(lambda: same(bounds(), player_area()))
            p.set_viewport_size(dict(width=1440, height=980))
            eventually(lambda: same(bounds(), clipped))
            assert prefs()['windows']['player']['bounds'] == player_bounds
            app.application('Files')
            app.settings('General')
            p.locator('.setting-language:visible').get_by_role('radio', name='简体中文', exact=True).check()
            app.servers()
            expect(p.locator('.files-sidebar').get_by_role('button', name='下载文件夹', exact=True)).to_be_visible()
            assert not errors, errors
    print(json.dumps(dict(output=str(out), checks=[
        'Eight resize handles, captured titlebar movement and Escape cancellation',
        'All desktop edges can clip floating windows while keeping a titlebar grip reachable',
        'Floating windows stay visible beside the Dock down to the desktop edge',
        'Floating dock leaves a four-pixel gap at the desktop edge',
        'Growth reveals top and left overhangs first, then follows the saved relative center on both axes',
        'Independent window geometry across apps, reopen, resize, maximize and client restart',
        'A drag in a narrow Desktop saves the full rectangle and both reference dimensions, and a partially visible window moves and resizes by the pointer delta',
        'A right-aligned Settings window stays inside the desktop on shrink, reflows, and restores its saved placement',
        'Settings search filters sections live and the split panes stay aligned with balanced content margins',
        'Windows can move past the bottom, left, and right Dock boundaries and be dragged back',
        'Player reveals its right and bottom overhang, then follows its relative center inside the side Dock; shrink eventually fits flush',
        'Native monitor fullscreen with Escape restoring the original OS window',
        'Breadcrumb double-click editing and single-click navigation',
        'File and folder favorites, drag ordering, tag filters and sidebar persistence over SSH',
    ]), indent=2))


if __name__ == '__main__':
    main()
