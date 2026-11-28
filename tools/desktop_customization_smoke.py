"""Desktop layout, wallpaper and Dock backed by the connected user's SSH home."""
import base64
import io
import json
import re
import time

from PIL import Image

from server_apps_fixture import desktop_settings_dir, read_desktop_settings
from desktop_fixture import Desktop, Peer, ROOT, close_dialog, expect, eventually


def check_wallpaper(surface, name):
    expect(surface).to_have_css('background-image', re.compile(re.escape(f'/wallpapers/{name}.webp')))
    path, width, height = surface.evaluate("""async el => {
        const url = getComputedStyle(el).backgroundImage.slice(5, -2);
        const image = new Image();
        image.src = url;
        await image.decode();
        return [new URL(url).pathname, image.naturalWidth, image.naturalHeight];
    }""")
    assert (path, width, height) == (f'/wallpapers/{name}.webp', 3840, 2160)


def check_terminal_ink(page, button):
    # The layout boxes can be centered even when the rasterized mint frame is not.
    cdp = page.context.new_cdp_session(page)
    colors = page.add_style_tag(content="""
        .desktop-dock { background: black !important; }
        .desktop-dock-app.app-terminal .desktop-app-icon {
            background: red !important; border-color: red !important;
            box-shadow: none !important; color: white !important;
        }
    """)
    try:
        cdp.send('Emulation.setDeviceMetricsOverride',
                 dict(**page.viewport_size, deviceScaleFactor=1.02, mobile=False))
        for offset in ('0px', '.33px', '.66px'):
            button.evaluate('(el, offset) => el.style.left = offset', offset)
            box = button.bounding_box()
            # Use the full compositor frame. A cropped WebView screenshot can
            # resample fractional edge pixels before the test sees them.
            frame = Image.open(io.BytesIO(base64.b64decode(cdp.send('Page.captureScreenshot')['data']))).convert('RGB')
            scale = page.evaluate('devicePixelRatio')
            y = round((box['y'] + box['height'] / 2) * scale)
            row = [frame.getpixel((x, y)) for x in range(round(box['x'] * scale), round((box['x'] + box['width']) * scale))]
            tile = [x for x, (red, _, _) in enumerate(row) if red > 128]
            ink = [x for x, rgb in enumerate(row) if min(rgb) > 128]
            assert min(ink) - min(tile) == max(tile) - max(ink), (offset, tile, ink)
    finally:
        button.evaluate("el => el.style.removeProperty('left')")
        colors.evaluate('el => el.remove()')
        cdp.detach()


def main():
    out = ROOT / 'artifacts/desktop-customization' / f'run-{time.time_ns()}'
    local = out / 'local'
    local.mkdir(parents=True)
    with Peer(out / 'peer') as peer:
        home = peer.root / 'home/arcnix'
        desktop = home / 'Desktop'
        (desktop / 'Projects').mkdir(parents=True, exist_ok=True)
        (desktop / 'Projects' / 'plan.txt').write_text('sample plan', encoding='utf-8')
        (desktop / 'notes.txt').write_text('desktop notes', encoding='utf-8')
        long_name = '服务器运维配置记录及备份校验结果详细归档-2026-11-18.txt'
        (desktop / long_name).write_text('long shortcut label', encoding='utf-8')
        (desktop / 'pattern.svg').write_text(
            '<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="720">'
            '<rect width="1280" height="720" fill="#215978"/></svg>', encoding='utf-8')
        (peer.root / 'Desktop').mkdir(exist_ok=True)
        (peer.root / 'Desktop' / 'outside-home.txt').write_text('not in the login home')
        settings_file = home / '.arcnix/desktop.json'
        settings_file.parent.mkdir(exist_ok=True)
        settings_file.write_text(json.dumps({
            'version': 1, 'theme': 'System',
            'appearance': {'wallpaper': 'dunes', 'dock': {'magnify': True, 'floating_status': False}},
            'files': {'folder_icons': {'/home/arcnix/Desktop/Projects': 'projects'}},
        }), encoding='utf-8')

        def preferences():
            return read_desktop_settings(settings_file)['appearance']

        with Desktop(out / 'desktop', peer.config([peer.profile('Customization fixture')], local)) as app:
            p = app.page
            errors = []
            p.on('pageerror', lambda error: errors.append(str(error)))

            def desktop_button(label):
                return p.locator('.desktop-shortcuts').get_by_role('button', name=label, exact=True)

            def dock_button(label):
                return p.locator('.desktop-dock').get_by_role('button', name=label, exact=True)

            def check_hover(button, clipped=False):
                button.hover()
                layout = button.evaluate('''el => {
                    const frame = el.getBoundingClientRect();
                    const cell = el.closest('.desktop-icon-slot').getBoundingClientRect();
                    const icon = el.querySelector('.desktop-app-icon').getBoundingClientRect();
                    const label = el.querySelector('.desktop-app-label');
                    const text = label.getBoundingClientRect();
                    return {
                        margins: [icon.top - frame.top, frame.bottom - text.bottom],
                        inCell: frame.bottom <= cell.bottom && frame.width < cell.width,
                        labelInFrame: text.left >= frame.left && text.right <= frame.right,
                        painted: getComputedStyle(el).backgroundColor !== 'rgba(0, 0, 0, 0)',
                        clipped: label.scrollHeight > label.clientHeight,
                    };
                }''')
                top, bottom = layout['margins']
                assert min(top, bottom) >= 3 and abs(top - bottom) <= 2, layout
                assert layout['inCell'] and layout['labelInFrame'] and layout['painted'], layout
                assert layout['clipped'] == clipped, layout

            def settings():
                return p.locator('.desktop-settings-page')

            def shortcut_options(title):
                link = settings().get_by_role('button', name=title, exact=True)
                expect(link).to_be_enabled()
                link.hover()
                assert link.evaluate('''el => {
                    const row = el.parentElement.getBoundingClientRect();
                    const button = el.getBoundingClientRect();
                    const label = el.querySelector('.desktop-setting-label').getBoundingClientRect();
                    const arrow = el.querySelector('.action-glyph').getBoundingClientRect();
                    const left = label.left - button.left;
                    const right = button.right - arrow.right;
                    return left >= 6 && Math.abs(left - right) < 1
                        && Math.abs(label.left - row.left) < 1 && Math.abs(arrow.right - row.right) < 1
                        && getComputedStyle(el).backgroundColor !== 'rgba(0, 0, 0, 0)';
                }''')
                link.click()
                dialog = p.get_by_role('dialog', name=title, exact=True)
                expect(dialog).to_be_visible()
                assert dialog.evaluate('''el => {
                    const rect = selector => el.querySelector(selector).getBoundingClientRect();
                    const check = rect('.desktop-shortcut-option input[type=checkbox]');
                    const close = rect('.modal-header .icon-button');
                    const title = rect('.modal-header .dialog-title');
                    const icon = rect('.desktop-shortcut-option > .action-glyph');
                    return Math.abs((check.left + check.right - close.left - close.right) / 2) < 1
                        && Math.abs(title.left - icon.left) < 1;
                }''')
                return dialog

            def check_dock_corners():
                active = p.locator('.desktop-dock-app.active')
                assert active.evaluate('''button => {
                    const icon = button.querySelector('.desktop-app-icon');
                    const radius = getComputedStyle(icon).borderTopLeftRadius;
                    const ratio = parseFloat(radius) / (radius.endsWith('%') ? 100 : icon.offsetWidth);
                    return ratio >= .15 && ratio <= .3;
                }''')

            def check_dock_selection():
                check_dock_corners()
                active = p.locator('.desktop-dock-app.active')
                assert active.evaluate('''button => {
                    const icon = button.querySelector('.desktop-app-icon').getBoundingClientRect();
                    const tile = button.getBoundingClientRect();
                    const dock = button.closest('.desktop-dock').getBoundingClientRect();
                    const zone = button.closest('.desktop-dock-zone');
                    const indicator = getComputedStyle(button, '::after');
                    const margins = [icon.top - tile.top, icon.left - tile.left,
                        tile.bottom - icon.bottom, tile.right - icon.right];
                    const left = zone.classList.contains('dock-left');
                    const right = zone.classList.contains('dock-right');
                    const center = left ? tile.left + parseFloat(indicator.left)
                        : right ? tile.right - parseFloat(indicator.right)
                        : tile.bottom - parseFloat(indicator.bottom);
                    const target = left ? (dock.left + icon.left) / 2
                        : right ? (dock.right + icon.right) / 2
                        : (dock.bottom + icon.bottom) / 2;
                    const [width, height] = [parseFloat(indicator.width), parseFloat(indicator.height)];
                    const dots = zone.querySelectorAll('.desktop-dock-app.running:not(.active)');
                    const alongDock = left || right ? [margins[0], margins[2]] : margins;
                    return alongDock.every(margin => Math.abs(margin / icon.width - .12) < .03)
                        && Math.abs(margins[1] - margins[3]) <= 1
                        && indicator.backgroundColor === 'rgb(255, 255, 255)' && indicator.boxShadow === 'none'
                        && [...dots].every(other => {
                            const dot = getComputedStyle(other, '::after');
                            return dot.backgroundColor === indicator.backgroundColor
                                && dot.boxShadow === indicator.boxShadow && dot.width === dot.height;
                        })
                        && Math.abs(center - target) <= 1
                        && ((left || right) ? height > width : width > height)
                        && indicator.content !== 'none';
                }''')
                assert p.locator('.desktop-dock').evaluate(
                    'el => el.scrollWidth <= el.clientWidth && el.scrollHeight <= el.clientHeight')

            def check_centered_dock(tray):
                active = tray.locator('.desktop-dock-app.active')
                assert active.evaluate('''button => {
                    const icon = button.querySelector('.desktop-app-icon');
                    const bar = button.closest('.desktop-dock').getBoundingClientRect();
                    const glyph = icon.getBoundingClientRect();
                    const top = icon.offsetTop;
                    const bottom = button.clientHeight - top - icon.offsetHeight;
                    return Math.abs(top - bottom) <= 1
                        && Math.abs((bar.top + bar.bottom - glyph.top - glyph.bottom) / 2) <= 1;
                }''')

            def saved():
                expect(settings().locator('.desktop-settings-status')).to_be_empty()

            def close_settings():
                p.locator('.desktop-window.active .window-close').click()
                expect(settings()).to_have_count(0)

            def menu(label):
                return p.locator('.desktop-context-menu').get_by_role('button', name=label, exact=True)

            def background_menu():
                p.locator('.desktop-canvas').click(button='right', position={'x': 565, 'y': 188})

            app.connect()
            app.workspace('Desktop')
            default_apps = ['Files', 'Terminal', 'Browser', 'Office', 'App Center', 'Monitor', 'Settings']
            expect(p.locator('.desktop-shortcuts .desktop-app-label').filter(
                has_text=re.compile('^(' + '|'.join(default_apps) + ')$'),
            )).to_have_text(default_apps)
            expect(p.locator('.desktop-dock-app .desktop-app-label')).to_have_text(
                default_apps,
            )
            bubble = p.locator('.desktop-context-menu')
            for label in ('Terminal', 'Settings'):
                launcher = dock_button(label)
                launcher.click(button='right')
                expect(bubble).to_have_attribute('data-dock-menu-side', 'above')
                icon_box = launcher.bounding_box()
                bubble_box = bubble.bounding_box()
                assert bubble_box['y'] + bubble_box['height'] < icon_box['y']
                arrow = float(bubble.evaluate("el => el.style.getPropertyValue('--dock-menu-arrow').slice(0, -2)"))
                assert abs(bubble_box['x'] + arrow - icon_box['x'] - icon_box['width'] / 2) < 2
                if label == 'Settings':
                    app.screenshot('dock-launcher-menu.png')
                p.keyboard.press('Escape')
                expect(bubble).to_have_count(0)
            expect(p.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-fjord')
            expect(desktop_button('Projects')).to_be_visible()
            project_icon = desktop_button('Projects').locator('.folder-icon-projects')
            expect(project_icon).to_be_visible()
            assert project_icon.evaluate("icon => getComputedStyle(icon, '::before').maskImage.includes('data:image/svg+xml;base64,')")
            expect(desktop_button('outside-home.txt')).to_have_count(0)
            expect(p.locator('.desktop-wallpaper-address')).to_contain_text('127.0.0.1:')
            # Measure shortcut layout margins without sampling a transient hover scale.
            p.emulate_media(reduced_motion='reduce')
            check_hover(desktop_button('Monitor'))
            check_hover(desktop_button(long_name), clipped=True)
            app.screenshot('shortcut-hover.png')
            p.set_viewport_size(dict(width=900, height=650))
            check_hover(desktop_button(long_name), clipped=True)
            p.set_viewport_size(dict(width=1440, height=980))
            p.emulate_media(reduced_motion=None)
            app_center = desktop_button('App Center')
            app_center.click()
            expect(app_center).to_be_focused()
            expect(p.locator('.desktop-window:not(.hidden)')).to_have_count(0)
            app_center.dblclick()
            expect(p.locator('.desktop-window:not(.hidden)')).to_be_visible()
            title = p.locator('.desktop-window.active .desktop-window-titlebar')
            assert title.evaluate("""element => !element.dispatchEvent(new MouseEvent('contextmenu', {
                bubbles: true, cancelable: true, button: 2,
            }))""")
            title.click(button='right')
            expect(p.locator('.desktop-context-menu')).to_have_count(0)
            expect(desktop_button('Terminal')).to_be_visible()
            assert p.locator('.desktop-window.active').evaluate('el => el.offsetLeft') == 22
            app.show_desktop()
            projects = desktop_button('Projects')
            projects.click()
            expect(projects).to_be_focused()
            expect(p.locator('.desktop-window.app-files:not(.hidden)')).to_have_count(0)
            projects.dblclick()
            expect(app.file('plan.txt')).to_be_visible()
            p.locator('.files-sidebar').get_by_role('button', name='Desktop', exact=True).click()
            expect(p.locator('.desktop-window.app-files.active .pathbar .breadcrumb.current')).to_have_attribute(
                'aria-label', '/home/arcnix/Desktop',
            )
            app.show_desktop()

            notes = desktop_button('notes.txt')
            notes.drag_to(desktop_button('Files'), target_position={'x': 8, 'y': 30})
            eventually(lambda: preferences().get('order', [])[:1] == [{'kind': 'file', 'id': 'notes.txt'}])
            expect(p.locator('.desktop-shortcut').first).to_have_attribute('aria-label', 'notes.txt')
            notes.click()
            expect(notes).to_be_focused()
            expect(p.locator('.app-editor.active')).to_have_count(0)
            notes.press('Enter')
            expect(p.locator('.app-editor.active .remote-editor-content')).to_have_value('desktop notes')
            p.locator('.desktop-window.active .window-close').click()

            desktop_button('Monitor').click(button='right')
            menu('Hide desktop shortcut').click()
            expect(desktop_button('Monitor')).to_have_count(0)
            background_menu()
            menu('Desktop settings').click()
            expect(settings().get_by_role('button', name='Desktop', exact=True)).to_have_attribute('aria-pressed', 'true')
            settings().get_by_role('button', name='Dock', exact=True).click()
            expect(settings().locator('.desktop-settings-card .section-title')).to_have_text(
                ['Dock layout', 'App window overview', 'Dock icons', 'Status bar', 'Launcher'])
            settings().get_by_text('Dock layout', exact=True).dblclick(position={'x': 8, 'y': 8})
            assert not p.evaluate('window.getSelection().toString()')
            position = settings().get_by_role('radiogroup', name='Dock position')
            expect(position.get_by_text('Bottom', exact=True)).to_have_css('user-select', 'none')
            expect(settings().locator('.dock-icons-settings').get_by_role('slider', name='Dock icon size', exact=True)).to_be_visible()
            pinned = shortcut_options('System app icons')
            pinned_order = ['Home', 'Files', 'Terminal', 'Text Editor', 'Code Editor', 'Browser', 'Player',
                            'Image Viewer', 'PDF Reader', 'Office', 'App Center', 'Monitor', 'Settings', 'Trash']
            expect(pinned.locator('.preference-check')).to_have_text(pinned_order)
            for label in ('Player', 'Text Editor', 'Code Editor'):
                expect(pinned.get_by_role('checkbox', name=label, exact=True)).not_to_be_checked()
            close_dialog(pinned)
            settings().get_by_role('button', name='Desktop', exact=True).click()
            settings().get_by_role('switch', name='Show server info', exact=True).click()
            saved()
            expect(p.locator('.desktop-wallpaper-copy')).to_have_count(0)
            shortcuts = shortcut_options('System app icons')
            monitor = shortcuts.get_by_role('checkbox', name='Monitor', exact=True)
            terminal = shortcuts.get_by_role('checkbox', name='Terminal', exact=True)
            expect(monitor).not_to_be_checked()
            expect(terminal).to_be_checked()
            terminal.uncheck()
            saved()
            close_dialog(shortcuts)
            expect(desktop_button('Terminal')).to_have_count(0)
            expect(desktop_button('Files')).to_be_visible()
            expect(dock_button('Terminal')).to_be_visible()
            expect(notes).to_be_visible()
            eventually(lambda: set(preferences()['hidden']) == {'monitor', 'terminal'})
            shortcut_options('System app icons')
            monitor.check()
            saved()
            close_dialog(shortcuts)
            expect(desktop_button('Monitor')).to_be_visible()
            expect(desktop_button('Terminal')).to_have_count(0)
            eventually(lambda: preferences()['hidden'] == ['terminal'])
            shortcut_options('System app icons')
            shortcuts.locator('.modal-header').get_by_role('button', name='Restore system app icons', exact=True).click()
            saved()
            expect(terminal).to_be_checked()
            close_dialog(shortcuts)
            expect(desktop_button('Monitor')).to_be_visible()
            expect(desktop_button('Terminal')).to_be_visible()
            settings().get_by_role('button', name='Appearance', exact=True).click()
            new_presets = ['Prism', 'Valley', 'Tide', 'Clay', 'Meadow']
            expect(settings().locator('.theme-preset-select')).to_have_text(new_presets + ['Fjord', 'Aurora', 'Seaside'])
            art = p.locator('.desktop-wallpaper-art')
            scheme = settings().get_by_role('radiogroup', name='Color scheme', exact=True)
            for preset in new_presets:
                choice = settings().get_by_role('button', name=preset, exact=True)
                choice.click()
                expect(choice).to_have_attribute('aria-pressed', 'true')
                expect(scheme.get_by_role('radio', checked=True)).to_have_accessible_name('Auto')
                swatches = choice.locator('..').locator('.theme-wallpaper-swatch')
                slug = preset.lower()
                check_wallpaper(swatches.first, f'{slug}-light')
                check_wallpaper(swatches.last, f'{slug}-dark')
                gallery_swatch = settings().locator(f'.wallpaper-swatch.wallpaper-{slug}')
                for mode in ('Light', 'Dark'):
                    scheme.get_by_role('radio', name=mode, exact=True).check()
                    check_wallpaper(art, f'{slug}-{mode.lower()}')
                    check_wallpaper(gallery_swatch, f'{slug}-{mode.lower()}')
            themes = settings().locator('.desktop-settings-card').filter(
                has=p.get_by_role('group', name='Themes', exact=True),
            )
            themes.get_by_role('button', name='Restore default theme', exact=True).click()
            expect(settings().get_by_role('button', name='Prism', exact=True)).to_have_attribute('aria-pressed', 'true')
            expect(scheme.get_by_role('radio', checked=True)).to_have_accessible_name('Auto')
            check_wallpaper(art, 'prism-light')
            seaside = settings().get_by_role('button', name='Seaside', exact=True)
            p.emulate_media(reduced_motion='no-preference')
            p.evaluate("""() => {
                window.wallpaperFades = 0;
                document.addEventListener('animationstart', event => {
                    if (event.animationName === 'wallpaper-fade-in') window.wallpaperFades++;
                });
            }""")
            seaside.click()
            eventually(lambda: p.evaluate('window.wallpaperFades') > 0)
            saved()
            art = p.locator('.desktop-wallpaper-art.wallpaper-seaside')
            expect(art).to_be_visible()
            swatches = seaside.locator('..').locator('.theme-wallpaper-swatch')
            expect(swatches).to_have_count(2)
            gallery_swatch = settings().locator('.wallpaper-swatch.wallpaper-seaside')
            for mode, picture in [('Light', 'seaside-sunrise'), ('Dark', 'seaside-moonrise')]:
                scheme = settings().get_by_role('radiogroup', name='Color scheme', exact=True)
                scheme.get_by_role('radio', name=mode, exact=True).check()
                expect(scheme.get_by_role('radio', checked=True)).to_have_accessible_name(mode)
                check_wallpaper(art, picture)
                check_wallpaper(gallery_swatch, picture)
                check_wallpaper(swatches.first, 'seaside-sunrise')
                check_wallpaper(swatches.last, 'seaside-moonrise')
            settings().get_by_role('button', name='Aurora', exact=True).click()
            saved()
            expect(scheme.get_by_role('radio', checked=True)).to_have_accessible_name('Auto')
            expect(p.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-aurora')
            fjord = settings().get_by_role('button', name='Fjord', exact=True)
            fjord.click()
            eventually(lambda: preferences().get('theme') == 'fjord')
            expect(p.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-fjord')
            fjord_swatches = fjord.locator('..').locator('.theme-wallpaper-swatch')
            expect(fjord_swatches).to_have_count(2)
            check_wallpaper(fjord_swatches.first, 'fjord-daylight')
            check_wallpaper(fjord_swatches.last, 'fjord')
            gallery = settings().get_by_role('group', name='Wallpaper', exact=True)
            for mode, picture in [('Dark', 'fjord'), ('Light', 'fjord-daylight')]:
                scheme = settings().get_by_role('radiogroup', name='Color scheme', exact=True)
                scheme.get_by_role('radio', name=mode, exact=True).check()
                expect(scheme.get_by_role('radio', checked=True)).to_have_accessible_name(mode)
                check_wallpaper(p.locator('.desktop-wallpaper-art'), picture)
                check_wallpaper(gallery.locator('.wallpaper-swatch.wallpaper-fjord'), picture)
            expect(gallery.get_by_role('button', name='Add Picture', exact=True)).to_have_count(0)
            p.emulate_media(reduced_motion='reduce')
            fades = p.evaluate('window.wallpaperFades')
            gallery.get_by_role('button', name='Seaside wallpaper', exact=True).click()
            eventually(lambda: preferences().get('wallpaper') == 'seaside')
            expect(p.locator('.wallpaper-transition-layer')).to_have_count(0)
            assert p.evaluate('window.wallpaperFades') == fades
            p.emulate_media(reduced_motion=None)
            assert preferences()['theme'] == 'fjord'
            close_settings()
            expect(p.locator('.desktop-shortcut').first).to_have_attribute('aria-label', 'notes.txt')
            desktop_button('Projects').drag_to(notes, target_position={'x': 8, 'y': 30})
            eventually(lambda: preferences()['order'][:1] == [{'kind': 'file', 'id': 'Projects'}])
            dock_button('Terminal').click(button='right')
            expect(menu('Dock settings')).to_have_count(0)
            p.locator('.desktop-menu-backdrop').click(position={'x': 2, 'y': 2})
            dock_button('Settings').click(button='right')
            menu('Dock settings').click()
            expect(settings().get_by_role('button', name='Dock', exact=True)).to_have_attribute('aria-pressed', 'true')
            settings().locator('.desktop-setting-row').filter(has_text='Dock icon size').locator('.setting-dock-icon-size:visible').get_by_role('radio', name='Large', exact=True).check()
            saved()
            expect(p.locator('.desktop-dock')).to_have_class('desktop-dock dock-size-64')
            expect(settings().get_by_role('button', name='Magnify Dock icons', exact=True)).to_have_count(0)
            settings().get_by_role('switch', name='Automatically hide the dock', exact=True).click()
            saved()
            close_settings()
            canvas = p.locator('.desktop-canvas').bounding_box()
            p.mouse.move(canvas['x'] + canvas['width'] / 2, canvas['y'] + canvas['height'] - 4)
            eventually(lambda: p.locator('.desktop-dock').bounding_box()['y'] < canvas['y'] + canvas['height'] - 30)
            dock_button('Terminal').drag_to(dock_button('Files'), target_position={'x': 5, 'y': 22})
            eventually(lambda: preferences()['dock']['order'][:1] == ['terminal'])
            dock_button('Monitor').click(button='right')
            menu('Remove from dock').click()
            eventually(lambda: 'monitor' in preferences()['dock']['unpinned'])
            expect(dock_button('Monitor')).to_have_count(0)

            app.stop()
            app.launch()
            p = app.page
            p.on('pageerror', lambda error: errors.append(str(error)))
            app.connect()
            app.workspace('Desktop')
            expect(p.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-seaside')
            expect(p.locator('.desktop-wallpaper-copy')).to_have_count(0)
            expect(desktop_button('Projects')).to_be_visible()
            expect(p.locator('.desktop-shortcut').first).to_have_attribute('aria-label', 'Projects')
            expect(p.locator('.desktop-dock-app').first).to_have_attribute('aria-label', 'Terminal')
            expect(p.locator('.desktop-dock')).to_have_class('desktop-dock dock-size-64 dock-auto-hide')
            expect(dock_button('Monitor')).to_have_count(0)
            canvas = p.locator('.desktop-canvas').bounding_box()
            p.mouse.move(canvas['x'] + canvas['width'] / 2, canvas['y'] + canvas['height'] - 4)
            eventually(lambda: p.locator('.desktop-dock').bounding_box()['y'] < canvas['y'] + canvas['height'] - 30)
            app.application('Settings')
            settings().get_by_role('button', name='Dock', exact=True).click()
            pinned = shortcut_options('System app icons')
            expect(pinned.locator('.preference-check')).to_have_text(pinned_order)
            close_dialog(pinned)
            settings().get_by_role('switch', name='Automatically hide the dock', exact=True).click()
            saved()
            settings().locator('.setting-dock-style:visible').get_by_role('radio', name='Panel mode', exact=True).check()
            saved()
            panel = p.locator('.dock-panel-zone > .desktop-dock')
            canvas = p.locator('.desktop-canvas')
            panel_box, canvas_box = panel.bounding_box(), canvas.bounding_box()
            assert abs(panel_box['x'] - canvas_box['x']) < 2
            assert abs(panel_box['width'] - canvas_box['width']) < 2
            assert abs(panel_box['y'] + panel_box['height'] - canvas_box['y'] - canvas_box['height']) < 2
            clock = panel.locator('.desktop-panel-clock')
            expect(clock.locator('.desktop-panel-time')).not_to_be_empty()
            expect(clock.locator('.desktop-panel-date')).not_to_be_empty()
            assert clock.bounding_box()['x'] > panel_box['x'] + panel_box['width'] / 2
            check_dock_selection()

            align_left = settings().locator('.setting-app-icon-alignment:visible')
            center_icons = settings().locator('.setting-app-icon-alignment:visible')
            leading = panel.locator('.desktop-dock-leading')
            apps = panel.locator('.desktop-dock-apps')
            expect(align_left.get_by_role('radio', checked=True)).to_have_accessible_name('Align left')
            leading_box = leading.bounding_box()
            assert 0 < apps.bounding_box()['x'] - leading_box['x'] - leading_box['width'] < 12
            center_icons.get_by_role('radio', name='Center icons', exact=True).check()
            eventually(lambda: preferences()['dock'].get('panel_centered') is True)
            expect(center_icons.get_by_role('radio', checked=True)).to_have_accessible_name('Center icons')
            apps_box = apps.bounding_box()
            assert abs(apps_box['x'] + apps_box['width'] / 2 - panel_box['x'] - panel_box['width'] / 2) < 2
            expect(apps).to_have_css('border-top-width', '0px')
            expect(apps).to_have_css('background-color', 'rgba(0, 0, 0, 0)')
            expect(apps).to_have_css('box-shadow', 'none')
            assert panel.evaluate('''el => Math.abs(parseFloat(getComputedStyle(el).paddingBlockStart)
                / el.querySelector('.desktop-app-icon').getBoundingClientRect().width - .045) < .01''')
            check_centered_dock(apps)
            assert abs(panel.get_by_role('button', name='Launcher', exact=True).bounding_box()['x'] - panel_box['x']) < 15
            p.set_viewport_size(dict(width=760, height=660))
            expect(settings()).to_have_css('flex-direction', 'row')
            assert apps.bounding_box()['x'] + apps.bounding_box()['width'] < clock.bounding_box()['x']
            check_dock_selection()
            check_centered_dock(apps)
            p.set_viewport_size(dict(width=1440, height=980))
            expect(settings()).to_have_css('flex-direction', 'row')

            slider = settings().get_by_role('slider', name='Dock icon size', exact=True)
            dock_size = settings().locator('.desktop-setting-row').filter(has_text='Dock icon size')
            for label, pixels in [('Small', 32), ('Medium', 48), ('Large', 64)]:
                dock_size.get_by_role('radio', name=label, exact=True).check()
                saved()
                expect(slider).to_have_value(str(pixels))
                expect(panel).to_have_css('--dock-ideal', f'{pixels}px')
                check_dock_selection()
            track = slider.bounding_box()
            for label, value, x, progress in [('Small', 32, 1, '0%'), ('Large', 64, track['width'] - 1, '100%')]:
                slider.click(position={'x': x, 'y': track['height'] / 2})
                expect(slider).to_have_value(str(value))
                assert slider.evaluate("el => el.style.getPropertyValue('--icon-size-progress')") == progress
                eventually(lambda: preferences()['dock']['size'] == label.lower() and 'icon_size' not in preferences()['dock'])
                expect(dock_size.get_by_role('radio', checked=True)).to_have_accessible_name(label)
                check_dock_corners()
            track = slider.bounding_box()
            y = track['y'] + track['height'] / 2
            p.mouse.move(track['x'] + track['width'] - 2, y)
            p.mouse.down()
            p.mouse.move(track['x'] + track['width'] * .9, y, steps=6)
            custom_pixels = eventually(lambda: slider.evaluate('''slider => {
                const dock = slider.closest('.desktop-canvas').querySelector('.desktop-dock');
                const size = slider.valueAsNumber;
                return size > 56 && size < 64 && dock.style.getPropertyValue('--dock-preview') === `${size}px`
                    ? size : null;
            }'''))
            assert 'icon_size' not in preferences()['dock']
            p.mouse.up()
            eventually(lambda: preferences()['dock'].get('icon_size') == custom_pixels)
            expect(panel).to_have_class(f'desktop-dock dock-size-{custom_pixels} dock-centered')
            app.screenshot('panel-dock.png')

            saved()
            app.stop()
            layout_path = desktop_settings_dir(settings_file) / 'preferences/desktop.json'
            layout = json.loads(layout_path.read_bytes())
            layout_path.write_text(json.dumps(dict(layout, show_apps=False)), encoding='utf-8')
            app.launch()
            p = app.page
            p.on('pageerror', lambda error: errors.append(str(error)))
            app.connect()
            app.workspace('Desktop')
            expect(p.locator('.dock-panel-zone > .desktop-dock')).to_have_class(f'desktop-dock dock-size-{custom_pixels} dock-centered')
            expect(p.locator('.desktop-panel-time')).not_to_be_empty()
            app.application('Settings')
            settings().get_by_role('button', name='Desktop', exact=True).click()
            expect(desktop_button('Files')).to_have_count(0)
            shortcuts = shortcut_options('System app icons')
            files_shortcut = shortcuts.get_by_role('checkbox', name='Files', exact=True)
            expect(files_shortcut).not_to_be_checked()
            files_shortcut.check()
            saved()
            close_dialog(shortcuts)
            expect(desktop_button('Files')).to_be_visible()
            expect(desktop_button('Terminal')).to_have_count(0)
            expect(desktop_button('Settings')).to_have_count(0)
            eventually(lambda: {'terminal', 'settings', 'docker'} <= set(preferences()['hidden']))
            settings().get_by_role('button', name='Dock', exact=True).click()
            expect(settings().locator('.setting-app-icon-alignment:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Center icons')
            expect(settings().get_by_role('switch', name='Show dock status bar', exact=True)).to_have_count(0)
            settings().locator('.setting-app-icon-alignment:visible').get_by_role('radio', name='Align left', exact=True).check()
            eventually(lambda: 'panel_centered' not in preferences()['dock'])
            expect(p.locator('.dock-panel-zone > .desktop-dock')).to_have_class(f'desktop-dock dock-size-{custom_pixels}')
            settings().locator('.setting-dock-style:visible').get_by_role('radio', name='Floating dock', exact=True).check()
            saved()
            expect(p.locator('.dock-panel-zone')).to_have_count(0)
            expect(p.locator('.desktop-panel-time')).to_have_count(0)
            expect(settings().locator('.dock-clock-settings')).to_have_count(0)
            expect(settings().locator('.setting-app-icon-alignment:visible')).to_have_count(0)
            slider = settings().get_by_role('slider', name='Dock icon size', exact=True)
            expect(slider).to_have_value(str(custom_pixels))
            check_dock_selection()
            check_centered_dock(p.locator('.desktop-dock'))
            status_toggle = settings().get_by_role('switch', name='Show dock status bar', exact=True)
            switch = settings().locator('.sapid-switch', has=p.get_by_role('switch', name='Show dock status bar', exact=True))
            track = switch.locator('.sapid-switch-track')
            assert track.evaluate('el => el.offsetHeight') == 20
            assert switch.locator('.sapid-switch-thumb').evaluate('el => el.offsetWidth') == 14
            target = switch.locator('.sapid-switch-control')
            assert target.evaluate('el => el.offsetWidth >= 56 && el.offsetHeight >= 44')
            expect(status_toggle).not_to_be_checked()
            status_toggle.click()
            expect(status_toggle).to_be_checked()
            expect(switch.locator('.sapid-switch-focus')).to_be_hidden()
            expect(track).to_have_css('outline-style', 'none')
            saved()
            status_toggle.press('Space')
            expect(status_toggle).not_to_be_checked()
            expect(track).to_have_css('outline-style', 'solid')
            status_toggle.press('Space')
            expect(status_toggle).to_be_checked()
            saved()
            expect(settings().locator('.dock-clock-settings').get_by_role('radiogroup', name='Time format', exact=True)).to_be_visible()
            status = p.locator('.desktop-dock-status')

            def check_clock_inset():
                assert status.evaluate('''el => {
                    const dock = el.closest('.desktop-dock');
                    const rect = node => node.getBoundingClientRect();
                    const frame = rect(dock);
                    const home = rect(dock.querySelector('.desktop-dock-home .action-glyph'));
                    const clock = rect(el.querySelector('.desktop-panel-clock'));
                    const [start, end] = [...dock.querySelectorAll('.desktop-dock-divider')].map(rect);
                    const icons = [...dock.querySelectorAll('.desktop-dock-app .desktop-app-icon')].map(rect);
                    const side = dock.closest('.dock-side-zone') !== null;
                    const [before, after, cross, span] = side
                        ? ['top', 'bottom', 'left', 'width'] : ['left', 'right', 'top', 'height'];
                    const same = (a, b) => Math.abs(a - b) <= 1;
                    const startInset = home[before] - frame[before];
                    const endInset = frame[after] - clock[after];
                    const corner = parseFloat(getComputedStyle(dock).borderTopLeftRadius);
                    const gap = parseFloat(getComputedStyle(dock).gap);
                    const homeButton = rect(dock.querySelector('.desktop-dock-home'));
                    return Math.min(startInset, endInset) >= 6
                        && same(start[before] - homeButton[after], gap)
                        && same(clock[before] - end[after], gap)
                        && icons.every(icon => same(icon[cross] + icon[span] / 2, frame[cross] + frame[span] / 2))
                        && corner < Math.min(frame.width, frame.height) / 3
                        && same(startInset, endInset)
                        && same(start[span], end[span]) && same(start[cross], end[cross])
                        && same(icons[0][before] - start[after], end[before] - icons.at(-1)[after]);
                }''')

            expect(status.locator('.desktop-dock-divider')).to_be_visible()
            check_clock_inset()
            radii = []
            for key, pixels, preset in [('Home', 32, 'small'), ('End', 64, 'large')]:
                slider.press(key)
                expect(slider).to_have_value(str(pixels))
                eventually(lambda: preferences()['dock']['size'] == preset and 'icon_size' not in preferences()['dock'])
                check_clock_inset()
                radii.append(status.evaluate(
                    'el => parseFloat(getComputedStyle(el.closest(".desktop-dock")).borderTopLeftRadius)'
                ))
            assert radii[0] < radii[1], radii
            expect(status.locator('.action-glyph')).to_have_count(0)
            expect(status).not_to_contain_text(re.compile(r'\b(?:SSH|SFTP)\b'))
            clock_time = status.locator('.desktop-panel-time')
            expect(clock_time).to_have_text(re.compile(r'\b\d{1,2}:\d{2}:\d{2}\b'))
            expect(clock_time).not_to_have_text(clock_time.text_content(), timeout=3000)
            expect(status.locator('.desktop-panel-date')).not_to_be_empty()
            assert status.evaluate('''el => {
                const time = el.querySelector('.desktop-panel-time').getBoundingClientRect();
                const date = el.querySelector('.desktop-panel-date').getBoundingClientRect();
                return date.top > time.top && Math.abs(date.right - time.right) < 1;
            }''')
            assert preferences()['dock']['floating_status'] is True
            active_dock_button = dock_button('Settings')
            dock_icon = active_dock_button.locator('.desktop-app-icon')
            icon_box = dock_icon.bounding_box()
            active_dock_button.hover()
            expect(dock_icon).to_have_css('transform', 'none')
            assert dock_icon.bounding_box() == icon_box
            p.mouse.move(25, 100)
            app.screenshot('floating-dock-status.png')
            settings().locator('.setting-dock-style:visible').get_by_role('radio', name='Panel mode', exact=True).check()
            saved()
            expect(status).to_have_count(0)
            expect(p.locator('.desktop-panel-time')).to_have_text(re.compile(r'\b\d{1,2}:\d{2}:\d{2}\b'))
            settings().locator('.setting-dock-style:visible').get_by_role('radio', name='Floating dock', exact=True).check()
            saved()
            expect(status_toggle).to_be_checked()
            expect(status).to_be_visible()
            expect(settings()).to_have_css('flex-direction', 'row')
            p.set_viewport_size(dict(width=680, height=660))
            expect(settings()).to_have_css('flex-direction', 'row')
            check_dock_selection()
            check_clock_inset()
            assert status.evaluate('el => el.scrollWidth <= el.clientWidth')
            p.set_viewport_size(dict(width=760, height=660))
            tabs = settings().locator('.desktop-settings-tabs')
            preferences_pane = settings().locator('.desktop-preferences')
            for label in ('General', 'Network', 'Desktop', 'Dock', 'Appearance', 'Keyboard shortcuts', 'About'):
                tab = tabs.get_by_role('button', name=label, exact=True)
                tab.click()
                expect(tab).to_have_attribute('aria-pressed', 'true')
                expect(preferences_pane.locator('.desktop-settings-pane:visible').first).to_be_visible()
                assert preferences_pane.evaluate('el => el.scrollWidth <= el.clientWidth'), label
                assert preferences_pane.evaluate('el => el.scrollTop === 0'), label
                if label == 'Appearance':
                    gallery = settings().get_by_role('group', name='Wallpaper', exact=True)
                    expect(gallery.locator('.wallpaper-choice')).to_have_count(8)
                    gallery.scroll_into_view_if_needed()
                    app.screenshot('compact-wallpaper-settings.png')
            p.set_viewport_size(dict(width=1440, height=980))
            settings().get_by_role('button', name='Dock', exact=True).click()
            expect(settings().locator('.setting-dock-position:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Bottom')
            p.locator('.app-settings.active .window-maximize').click()

            def maximized_fits_dock(edge):
                return p.locator('.app-settings.maximized').evaluate('''(window, edge) => {
                    const canvas = window.closest('.desktop-canvas');
                    const frame = canvas.getBoundingClientRect();
                    const dock = canvas.querySelector('.desktop-dock-zone').getBoundingClientRect();
                    const box = window.getBoundingClientRect();
                    const near = (a, b) => Math.abs(a - b) <= 1;
                    return near(box.top, frame.top)
                        && near(box.left, edge === 'left' ? dock.right : frame.left)
                        && near(box.right, edge === 'right' ? dock.left : frame.right)
                        && near(box.bottom, edge === 'bottom' ? dock.top : frame.bottom);
                }''', edge)

            eventually(lambda: maximized_fits_dock('bottom'))
            bubble = p.locator('.desktop-context-menu')
            side_gaps = {}
            for edge, menu_side in [('left', 'right'), ('right', 'left')]:
                settings().locator('.setting-dock-position:visible').get_by_role('radio', name=edge.title() + ' side', exact=True).check()
                p.mouse.move(25, 100)
                zone = p.locator(f'.desktop-dock-zone.dock-{edge}')
                expect(zone).to_be_visible()
                eventually(lambda: preferences()['dock'].get('position') == edge)
                dock = zone.locator('.desktop-dock')
                expect(dock).to_have_css('flex-direction', 'column')
                def launcher_gap():
                    return dock.evaluate('''dock => {
                        const leading = dock.querySelector('.desktop-dock-leading');
                        const button = leading.querySelector('button').getBoundingClientRect();
                        const divider = leading.querySelector('.desktop-dock-divider').getBoundingClientRect();
                        return divider.top - button.bottom;
                    }''')

                weekday = settings().get_by_role('switch', name='Weekday', exact=True)
                weekday_text = dock.locator('.desktop-panel-weekday')
                for key, pixels in [('Home', 32), ('End', 64)]:
                    slider.press(key)
                    expect(slider).to_have_value(str(pixels))
                    status_toggle.click()
                    expect(status).to_have_count(0)
                    without_clock = launcher_gap()
                    status_toggle.click()
                    expect(status).to_be_visible()
                    expect(weekday_text).to_be_empty()
                    with_date = launcher_gap()
                    weekday.check()
                    expect(weekday_text).not_to_be_empty()
                    side_gaps[edge, pixels] = (without_clock, with_date, launcher_gap())
                    weekday.uncheck()
                    expect(weekday_text).to_be_empty()
                slider.press('Home' if edge == 'left' else 'End')
                expect(slider).to_have_value('32' if edge == 'left' else '64')
                assert dock.evaluate('''dock => {
                    const canvas = dock.closest('.desktop-canvas').getBoundingClientRect();
                    const box = dock.getBoundingClientRect();
                    return box.height < canvas.height && Math.abs(box.top + box.bottom - canvas.top - canvas.bottom) < 2;
                }''')
                check_dock_selection()
                side_clock = dock.locator('.desktop-panel-clock')
                check_clock_inset()
                if edge == 'left':
                    check_terminal_ink(p, dock_button('Terminal'))
                expect(side_clock.locator('.desktop-panel-time')).to_have_text(re.compile(r'^\d{2}:\d{2}$'))
                expect(side_clock.locator('.desktop-panel-date')).to_have_text(re.compile(r'^\d{2}/\d{2}$'))
                tooltip = side_clock.locator('.desktop-clock-tooltip')
                expect(tooltip).to_be_hidden()
                side_clock.hover()
                expect(tooltip).to_be_visible()
                detail_time = r'\d{1,2}:\d{2}' if edge == 'left' else r'\d{1,2}:\d{2}:\d{2}'
                expect(tooltip).to_contain_text(re.compile(r'\d{4}\s+' + detail_time + r'\s+UTC'))
                app.screenshot(f'{edge}-dock-clock.png')
                dock_button('Settings').click(button='right')
                expect(bubble).to_have_attribute('data-dock-menu-side', menu_side)
                p.keyboard.press('Escape')
                expect(bubble).to_have_count(0)
                eventually(lambda: maximized_fits_dock(edge))
                if edge == 'left':
                    dock_button('Settings').drag_to(dock_button('Terminal'), target_position={'x': 12, 'y': 4})
                    eventually(lambda: preferences()['dock']['order'][0] == 'settings')
            assert all(max(gaps) - min(gaps) <= 1 for gaps in side_gaps.values()), side_gaps
            settings().locator('.setting-dock-style:visible').get_by_role('radio', name='Panel mode', exact=True).check()
            align_top = settings().locator('.setting-app-icon-alignment:visible')
            center_icons = settings().locator('.setting-app-icon-alignment:visible')

            def side_panel_fits(dock, centered):
                return dock.evaluate('''(dock, centered) => {
                    const zone = dock.parentElement;
                    const rect = el => el.getBoundingClientRect();
                    const desktop = rect(zone.closest('.desktop-canvas'));
                    const panel = rect(dock);
                    const leading = rect(dock.querySelector('.desktop-dock-leading'));
                    const apps = rect(dock.querySelector('.desktop-dock-apps'));
                    const clock = rect(dock.querySelector('.desktop-panel-clock'));
                    const style = getComputedStyle(dock);
                    const near = (a, b) => Math.abs(a - b) < 1;
                    const edge = zone.classList.contains('dock-left') ? 'left' : 'right';
                    return near(panel.top, desktop.top) && near(panel.bottom, desktop.bottom)
                        && near(panel[edge], desktop[edge])
                        && near(leading.top - panel.top, parseFloat(style.paddingTop))
                        && near(panel.bottom - clock.bottom, parseFloat(style.paddingBottom))
                        && (centered
                            ? near((apps.top + apps.bottom) / 2, (panel.top + panel.bottom) / 2)
                            : near(apps.top - leading.bottom, parseFloat(style.rowGap)));
                }''', centered)

            for edge in ('left', 'right'):
                settings().locator('.setting-dock-position:visible').get_by_role('radio', name=edge.title() + ' side', exact=True).check()
                panel = p.locator(f'.dock-{edge}.dock-panel-zone > .desktop-dock')
                expect(panel.locator('.desktop-panel-time')).to_have_text(re.compile(r'^\d{2}:\d{2}$'))
                expect(align_top.get_by_role('radio', checked=True)).to_have_accessible_name('Align top')
                eventually(lambda: side_panel_fits(panel, False))
                if edge == 'left':
                    app.screenshot('left-panel-align-top.png')
                center_icons.get_by_role('radio', name='Center icons', exact=True).check()
                expect(center_icons.get_by_role('radio', checked=True)).to_have_accessible_name('Center icons')
                eventually(lambda: side_panel_fits(panel, True))
                p.set_viewport_size(dict(width=900, height=590))
                eventually(lambda: side_panel_fits(panel, True))
                eventually(lambda: maximized_fits_dock(edge))
                check_dock_corners()
                align_top.get_by_role('radio', name='Align top', exact=True).check()
                eventually(lambda: side_panel_fits(panel, False))
                check_dock_corners()
                p.set_viewport_size(dict(width=1440, height=980))
            settings().locator('.setting-dock-position:visible').get_by_role('radio', name='Bottom', exact=True).check()
            eventually(lambda: maximized_fits_dock('bottom'))
            auto_hide = settings().get_by_role('switch', name='Automatically hide the dock', exact=True)
            auto_hide.click()
            expect(auto_hide).to_be_checked()
            eventually(lambda: maximized_fits_dock('bottom'))
            dock_layout = settings().locator('.dock-layout-settings')
            dock_icons = settings().locator('.dock-icons-settings')
            dock_status = settings().locator('.dock-status-settings')
            dock_icons.locator('.setting-dock-icon-size:visible').get_by_role('radio', name='Large', exact=True).check()
            status_format = dock_status.locator('.setting-time-format:visible')
            status_format.get_by_role('radio', name='24-hour', exact=True).check()
            dock_layout.locator('.settings-card-heading').get_by_role('button', name='Restore default dock layout', exact=True).click()
            expect(auto_hide).not_to_be_checked()
            expect(dock_icons.locator('.setting-dock-icon-size:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Large')
            floating_status = dock_status.get_by_role('switch', name='Show dock status bar', exact=True)
            floating_status.check()
            expect(status_format.get_by_role('radio', checked=True)).to_have_accessible_name('24-hour')
            dock_icons.locator('.settings-card-heading').get_by_role('button', name='Restore default dock icons', exact=True).click()
            expect(dock_icons.locator('.setting-dock-icon-size:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Medium')
            expect(floating_status).to_be_checked()
            expect(status_format.get_by_role('radio', checked=True)).to_have_accessible_name('24-hour')
            floating_status.uncheck()
            dock_status.locator('.settings-card-heading').get_by_role('button', name='Restore status bar defaults', exact=True).click()
            expect(floating_status).to_be_checked()
            expect(dock_icons.locator('.setting-dock-icon-size:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Medium')
            expect(p.locator('.dock-side-zone')).to_have_count(0)
            expect(settings().locator('.setting-dock-position:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Bottom')
            assert not errors, errors
            app.screenshot('restored.png')
            settings().get_by_role('button', name='Appearance', exact=True).click()
            settings().locator('.setting-icon-family:visible').get_by_role('radio', name='Color icons', exact=True).check()
            expect(p.locator('.server-desktop')).to_have_attribute('data-desktop-icon-family', 'color')
            project_icon = desktop_button('Projects').locator('.folder-icon-projects')
            assert project_icon.evaluate('''icon => {
                const style = getComputedStyle(icon, '::before');
                return style.maskImage === style.getPropertyValue('--folder-color-mask')
                    && style.backgroundImage.startsWith('linear-gradient(');
            }''')
    print(json.dumps(dict(output=str(out), checks=[
        'A default floating window uses the standard inset and can cover shortcuts; login HOME files open in Files or Text Editor',
        'Desktop shortcut visibility is saved per app, supports context-menu hiding and reset, and preserves other hidden apps when showing one from an older all-hidden layout',
        'Shortcut hover surrounds the icon and label at both desktop sizes and clips long names',
        'Wallpaper overrides render after restart and custom picture actions are absent',
        'Dock reorder, size, auto-hide and unpin survive restart, and hovering leaves icon size unchanged',
        'Full-width panel, server clock, persistent icon alignment, live slider preview and saved custom size',
        'Proportional icon margins, centered activity indicators and a two-line bottom clock that ticks every second',
        'Dock position, compact side clocks with full tooltips, inward menus and maximized window clearance',
        'Floating side Docks stay centered while left and right panels fill the desktop with top and centered icon layouts',
        'The active Dock icon has a visible underline and the separate Desktop and Dock settings fit the compact layout',
    ]), indent=2))


if __name__ == '__main__':
    main()
