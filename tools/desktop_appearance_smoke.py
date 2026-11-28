"""Verify theme inheritance, previews, and file placement in the desktop WebView."""
import time

from desktop_fixture import Desktop, Peer, ROOT, close_dialog, eventually, expect, expect_radio_options
from server_apps_fixture import desktop_settings_dir, read_desktop_settings


def check_symbol_centers(page, selector):
    cdp = page.context.new_cdp_session(page)
    try:
        root = cdp.send('DOM.getDocument')['root']['nodeId']
        nodes = cdp.send('DOM.querySelectorAll', dict(nodeId=root, selector=selector))['nodeIds']
        assert nodes, selector
        for node in nodes:
            tile = cdp.send('DOM.describeNode', dict(nodeId=node, depth=1))['node']
            symbol = next(p for p in tile['pseudoElements'] if p['pseudoType'] == 'before')
            def center(element):
                quad = cdp.send('DOM.getBoxModel', dict(backendNodeId=element['backendNodeId']))['model']['border']
                return ((quad[0] + quad[4]) / 2, (quad[1] + quad[5]) / 2)
            assert all(abs(a - b) < 1 for a, b in zip(center(tile), center(symbol))), selector
    finally:
        cdp.detach()


def check_app_artwork(page):
    # A centered CSS mask can still contain an off-center drawing.
    icons = ROOT / 'crates/arcnix/assets/icons'
    line_names = ('home', 'folder', 'file', 'edit', 'code', 'play', 'trash',
                  'browser', 'docker', 'monitor', 'app-center', 'terminal', 'settings')
    files = [icons / f'{name}.svg' for name in line_names] + sorted((icons / 'color').glob('*.svg'))
    for path in files:
        samples = page.evaluate('''async svg => {
            const image = new Image();
            image.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
            await image.decode();
            return [32, 48, 64].map(size => {
                const canvas = document.createElement('canvas');
                canvas.width = canvas.height = size;
                const context = canvas.getContext('2d');
                context.drawImage(image, 0, 0, size, size);
                const {data} = context.getImageData(0, 0, size, size);
                let left = size, top = size, right = -1, bottom = -1;
                for (let y = 0; y < size; y++) {
                    for (let x = 0; x < size; x++) {
                        if (data[4 * (y * size + x) + 3] < 128) continue;
                        left = Math.min(left, x);
                        right = Math.max(right, x);
                        top = Math.min(top, y);
                        bottom = Math.max(bottom, y);
                    }
                }
                return [size, left, size - 1 - right, top, size - 1 - bottom];
            });
        }''', path.read_text(encoding='utf-8'))
        for size, left, right, top, bottom in samples:
            assert 0 < min(left, right, top, bottom) and max(left, right, top, bottom) < size / 2, (path, size)
            # The play triangle's envelope sits one 24-unit grid step to the right for optical balance.
            optical_shift = size / 24 if path == icons / 'play.svg' else 0
            assert abs(left - right - 2 * optical_shift) <= 1 and abs(top - bottom) <= 1, (path, samples)


def check_appearance_choices(panel):
    def button(name):
        return panel.get_by_role('button', name=name, exact=True)

    scheme = panel.get_by_role('radiogroup', name='Color scheme')
    icons = panel.get_by_role('radiogroup', name='Icon family')
    indicator = scheme.locator('.sapid-segment-indicator.is-positioned')
    panel.page.emulate_media(reduced_motion='no-preference')
    assert indicator.evaluate("""el => {
        const style = getComputedStyle(el);
        return style.transitionProperty.includes('left') &&
            style.transitionDuration.split(',').some(duration => parseFloat(duration) > 0);
    }""")
    panel.page.emulate_media(reduced_motion='reduce')
    expect(indicator).to_have_css('transition-duration', '0s')
    panel.page.emulate_media(reduced_motion=None)
    gallery = panel.get_by_role('group', name='Wallpaper', exact=True)
    presets = {
        'Prism': ('prism-light', 'prism-dark'),
        'Valley': ('valley-light', 'valley-dark'),
        'Tide': ('tide-light', 'tide-dark'),
        'Clay': ('clay-light', 'clay-dark'),
        'Meadow': ('meadow-light', 'meadow-dark'),
        'Fjord': ('fjord-daylight', 'fjord'),
        'Aurora': ('aurora-twilight', 'aurora-night'),
        'Seaside': ('seaside-sunrise', 'seaside-moonrise'),
    }
    expect(gallery.locator('.wallpaper-choice')).to_have_text([''] * len(presets))
    selected_wallpaper = gallery.locator('.wallpaper-choice[aria-pressed=true]')
    background = panel.page.locator('.desktop-wallpaper-art')
    preset_accents = set()
    for mode in ('Light', 'Dark'):
        for preset, assets in presets.items():
            card = button(preset).locator('..')
            card.click(position={'x': 20, 'y': 20})
            expect(button(preset)).to_have_attribute('aria-pressed', 'true')
            scheme.get_by_role('radio', name=mode, exact=True).check()
            expect(scheme.get_by_role('radio', checked=True)).to_have_accessible_name(mode)
            expect(panel.page.locator('.server-desktop')).to_have_class(
                f'server-desktop theme-{mode.lower()} desktop-{mode.lower()}',
            )
            accent = card.evaluate("el => getComputedStyle(el).getPropertyValue('--accent-fill').trim()")
            red, green, blue = (int(accent[i:i + 2], 16) for i in (1, 3, 5))
            expected_color = f'rgb({red}, {green}, {blue})'
            expect(indicator).to_have_css('background-color', expected_color)
            expect(panel.get_by_role('textbox', name='Accent color', exact=True)).to_have_value(accent)
            readable = card.evaluate("el => getComputedStyle(el).getPropertyValue('--accent').trim()")
            expect(card).to_have_css('border-top-color', f'rgb{tuple(bytes.fromhex(readable[1:]))}')
            preset_accents.add(accent)
            icons.get_by_role('radio', name='Color icons', exact=True).check()
            expect(icons.get_by_role('radio', checked=True)).to_have_accessible_name('Color icons')
            selected_icon = icons.get_by_role('radio', checked=True)
            selected_icon.press('Tab')
            panel.page.keyboard.press('Shift+Tab')
            expect(selected_icon).to_be_focused()
            assert selected_icon.evaluate("""el => el.matches(':focus-visible') &&
                getComputedStyle(el.closest('label')).outlineStyle !== 'none'""")
            selected_icon.press('ArrowLeft')
            line_icons = icons.get_by_role('radio', name='Line icons', exact=True)
            expect(line_icons).to_be_checked()
            expect(line_icons).to_be_focused()
            override = 'Aurora' if preset == 'Seaside' else 'Seaside'
            gallery.get_by_role('button', name=f'{override} wallpaper', exact=True).click()
            expect(selected_wallpaper).to_have_attribute('aria-label', f'{override} wallpaper')
            gallery.get_by_role('button', name='Restore theme wallpaper', exact=True).click()
            expect(selected_wallpaper).to_have_attribute('aria-label', f'{preset} wallpaper')
            expect(background).to_have_class(f'desktop-wallpaper-art wallpaper-{preset.lower()}')
            swatches = card.locator('.theme-wallpaper-swatch')
            images = swatches.evaluate_all('els => els.map(el => getComputedStyle(el).backgroundImage)')
            for image, asset in zip(images, assets, strict=True):
                assert image.endswith(f'/wallpapers/thumbnails/{asset}.webp")'), image
            active_image = images[0 if mode == 'Light' else 1]
            expect(background).to_have_css('background-image', active_image.replace('/thumbnails/', '/'))
            expect(selected_wallpaper.locator('.wallpaper-swatch')).to_have_css('background-image', active_image)
            if preset == 'Aurora':
                assert background.evaluate('el => el.getAnimations({subtree: true}).length') == 0
            if preset == 'Aurora' and mode == 'Dark':
                panel.locator('.setting-color-scheme:visible').get_by_role('radio', name='Auto', exact=True).check()
                expect(selected_wallpaper).to_have_attribute('aria-label', 'Aurora wallpaper')
                expect(background).to_have_class('desktop-wallpaper-art wallpaper-aurora')
    assert len(preset_accents) == 2 * len(presets)
    button('Fjord').click()


def main():
    out = ROOT / 'artifacts/desktop-appearance' / str(time.time_ns())
    with Peer(out / 'peer') as peer:
        home = peer.root / 'home/arcnix'
        theme_file = home / '.arcnix/desktop.json'
        theme_file.parent.mkdir(parents=True, exist_ok=True)
        legacy = '{"appearance":{"show_server":false}}'
        theme_file.write_text(legacy, encoding='utf-8')
        for name in ('copy.txt', 'move.txt'):
            (peer.root / name).write_text(name, encoding='utf-8')
        (peer.root / 'Linked folder').mkdir()
        (peer.root / 'Linked folder' / 'inside.txt').touch()
        (home / 'night.svg').write_text(
            '<svg xmlns="http://www.w3.org/2000/svg" width="90" height="60">'
            '<rect width="90" height="60" fill="#32264a"/></svg>', encoding='utf-8',
        )
        (home / 'dawn.svg').write_text(
            '<svg xmlns="http://www.w3.org/2000/svg" width="110" height="60">'
            '<rect width="110" height="60" fill="#a57262"/></svg>', encoding='utf-8',
        )
        with Desktop(out / 'desktop', peer.config([peer.profile('Desktop appearance')], out)) as app:
            page = app.page
            app.connect()
            app.application('Settings')
            panel = page.locator('.desktop-settings-page')
            desktop = page.locator('.server-desktop')
            grid = desktop.locator('.desktop-shortcuts')
            expect(desktop).to_have_attribute('data-desktop-icon-style', 'outline')
            expect(desktop).to_have_class('server-desktop theme-light desktop-light')
            expect(desktop.locator('.desktop-wallpaper-art.wallpaper-prism')).to_have_count(1)
            expect(desktop.locator('.desktop-wallpaper-copy')).to_have_count(0)
            assert theme_file.read_text(encoding='utf-8') == legacy
            assert desktop_settings_dir(theme_file) is None

            def button(name):
                return panel.get_by_role('button', name=name, exact=True)

            def shortcut_options(title):
                button(title).click()
                dialog = page.get_by_role('dialog', name=title, exact=True)
                expect(dialog).to_be_visible()
                return dialog

            def persist():
                return read_desktop_settings(theme_file).get('appearance', {})

            def check_shortcut_gutter():
                eventually(lambda: grid.evaluate('''grid => {
                    const rules = getComputedStyle(grid);
                    const side = rules.direction === 'rtl' ? 'right' : 'left';
                    const start = grid.closest('.desktop-canvas').getBoundingClientRect()[side];
                    const dock = parseFloat(rules.getPropertyValue(`--desktop-dock-${side}`)) || 0;
                    const icons = [...grid.querySelectorAll('.desktop-app-icon')].map(node => node.getBoundingClientRect());
                    const testAxis = (dimension, limit, inset, pitch) => {
                        const positions = icons.map(rect => ({size: rect[dimension], inset: inset(rect)}))
                            .sort((a, b) => a.inset - b.inset);
                        const first = positions[0];
                        if (!first) return false;
                        const next = positions.find(icon => icon.inset > first.inset + 1);
                        const gap = (next ? next.inset - first.inset : pitch) - first.size;
                        return gap < limit && Math.abs(first.inset * 2 - gap) <= 2;
                    };
                    const top = grid.getBoundingClientRect().top;
                    return testAxis('width', 50, rect => Math.abs(rect[side] - start) - dock,
                            parseFloat(rules.gridAutoColumns) + parseFloat(rules.columnGap))
                        && testAxis('height', 66, rect => rect.top - top,
                            parseFloat(rules.gridAutoRows) + parseFloat(rules.rowGap));
                }'''))

            def check_filled_icons():
                icons = page.locator('.desktop-shortcuts .desktop-app-icon, .desktop-dock-apps .desktop-app-icon')
                samples = icons.evaluate_all(r'''icons => {
                    const luminance = color => color.match(/[\d.]+/g).slice(0, 3).map(Number)
                        .map(value => value / 255)
                        .map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4)
                        .reduce((total, value, index) => total + value * [.2126, .7152, .0722][index], 0);
                    return icons.map(icon => {
                        const style = getComputedStyle(icon);
                        const tile = style.backgroundColor;
                        const symbol = getComputedStyle(icon, '::before').backgroundColor;
                        const light = Math.max(luminance(tile), luminance(symbol));
                        const dark = Math.min(luminance(tile), luminance(symbol));
                        return {tile, symbol, ratio: (light + .05) / (dark + .05), solid: style.backgroundImage === 'none'};
                    });
                }''')
                assert samples and all(sample['solid'] and sample['ratio'] >= 4 for sample in samples), samples
                assert len({sample['tile'] for sample in samples}) >= 5, samples

            def check_interval_style(theme):
                button('General').click()
                control = panel.get_by_role('combobox', name='Resource refresh interval')
                expect(control.locator('option:checked')).to_have_text('Follow application')
                expect(control.locator('option:not([hidden])')).to_have_text(['Follow application', 'Every 3 s', 'Every 5 s', 'Every 10 s', 'Every 30 s'])
                expect(control).to_have_css('color', desktop.evaluate('el => getComputedStyle(el).color'))
                expect(control).to_have_css('border-top-width', '0px')
                expect(control).to_have_css('text-overflow', 'ellipsis')
                assert control.evaluate("""el => el.getBoundingClientRect().right <=
                    el.closest('.settings-select-field').getBoundingClientRect().right + 1""")
                control.locator('..').screenshot(path=str(out / f'{theme}-refresh-interval.png'))

            button('About').click()
            links = panel.locator('.about-links')
            expect(links).to_have_css('column-gap', '8px')

            button('Desktop').click()
            headings_locator = panel.locator('.desktop-settings-card .section-title')
            expect(headings_locator.first).to_have_text('Desktop layout')
            headings = headings_locator.all_text_contents()
            assert headings[:2] == ['Desktop layout', 'Desktop icons'], headings
            expect(panel.get_by_role('slider', name='Dock icon size', exact=True)).to_have_count(0)
            check_shortcut_gutter()
            button('Dock').click()
            panel.locator('.setting-dock-position:visible').get_by_role('radio', name='Left side', exact=True).check()
            expect(page.locator('.dock-left')).to_be_visible()
            check_shortcut_gutter()
            panel.locator('.setting-dock-position:visible').get_by_role('radio', name='Bottom', exact=True).check()
            button('Desktop').click()
            panel.locator('.setting-icon-arrangement:visible').get_by_role('radio', name='Horizontal', exact=True).check()
            panel.locator('.setting-starting-edge:visible').get_by_role('radio', name='Right to left', exact=True).check()
            presets = panel.get_by_role('radiogroup', name='Desktop icon size', exact=True)
            desktop_size = panel.get_by_role('slider', name='Desktop icon size', exact=True)
            desktop_size.scroll_into_view_if_needed()
            icon = grid.locator('.desktop-app-icon').first

            def check_desktop_icon(pixels):
                eventually(lambda: icon.evaluate('''(node, size) => {
                    const { width, height } = getComputedStyle(node);
                    return [width, height].every(value => Math.abs(parseFloat(value) - size) < .5);
                }''', pixels))
                expect(icon).to_have_css('border-top-left-radius', '29%')

            icon_presets = [('Small', '32'), ('Medium', '48'), ('Large', '64')]
            for label, pixels in icon_presets:
                preset = presets.get_by_role('radio', name=label, exact=True)
                preset.check()
                expect(preset).to_be_checked()
                expect(desktop_size).to_have_value(pixels)
                check_desktop_icon(int(pixels))
                eventually(lambda: persist().get('layout', {}).get('size') == label.lower())
            expect(grid).to_have_css('direction', 'rtl')
            check_shortcut_gutter()
            assert persist()['show_server'] is False
            track = desktop_size.bounding_box()
            y = track['y'] + track['height'] / 2
            page.mouse.move(track['x'] + track['width'] * 0.65, y)
            page.mouse.down()
            page.mouse.move(track['x'] + 1, y, steps=6)
            expect(desktop_size).to_have_value('32')
            check_desktop_icon(32)
            check_shortcut_gutter()
            page.mouse.move(track['x'] + track['width'] - 1, y, steps=6)
            expect(desktop_size).to_have_value('64')
            check_desktop_icon(64)
            page.mouse.move(track['x'] + track['width'] * 0.87, y, steps=6)
            pixels = int(desktop_size.input_value())
            assert 55 < pixels < 64
            assert 'icon_size' not in persist()['layout']
            check_desktop_icon(pixels)
            page.mouse.up()
            eventually(lambda: persist()['layout'].get('icon_size') == pixels)
            expect(grid).to_have_class(f'desktop-shortcuts icons-by-row icons-from-right icons-size-{pixels}')
            button('Dock').click()
            dock_slider = panel.get_by_role('slider', name='Dock icon size', exact=True)
            dock_presets = panel.get_by_role('radiogroup', name='Dock icon size', exact=True)
            expect(dock_slider).to_have_value('48')
            for label, expected in icon_presets:
                dock_presets.get_by_role('radio', name=label, exact=True).check()
                expect(dock_slider).to_have_value(expected)
            dock_presets.get_by_role('radio', name='Medium', exact=True).check()
            button('Desktop').click()
            expect(desktop_size).to_be_visible()
            visible = shortcut_options('System app icons')
            expect(visible.get_by_role('checkbox', name='Files', exact=True)).to_be_checked()
            visible.get_by_role('checkbox', name='Home', exact=True).check()
            visible.get_by_role('checkbox', name='Trash', exact=True).check()
            close_dialog(visible)
            home_shortcut = grid.get_by_role('button', name='Home', exact=True)
            expect(home_shortcut).to_be_visible()
            home_mask = home_shortcut.locator('.desktop-app-icon').evaluate(
                "icon => getComputedStyle(icon, '::before').maskImage",
            )
            files_mask = grid.get_by_role('button', name='Files', exact=True).locator('.desktop-app-icon').evaluate(
                "icon => getComputedStyle(icon, '::before').maskImage",
            )
            assert home_mask.startswith('url("data:image/svg+xml;base64,') and home_mask != files_mask
            expect(grid.get_by_role('button', name='Trash', exact=True)).to_be_visible()

            button('Appearance').click()
            swatches = panel.locator('.theme-accent-options button')
            expect(swatches).to_have_count(36)
            assert swatches.evaluate_all("""buttons => {
                const bounds = buttons[0].parentElement.getBoundingClientRect();
                const row = buttons.map(b => b.getBoundingClientRect()).filter(r => r.y === buttons[0].getBoundingClientRect().y);
                const cell = bounds.width / row.length;
                return row.length === 18 && new Set(buttons.map(b => b.offsetTop)).size === 2
                    && bounds.right - row.at(-1).right < cell / 2;
            }""")
            check_appearance_choices(panel)
            button('Aurora').click()
            expect(desktop).to_have_class('server-desktop theme-light desktop-light')
            scheme = panel.get_by_role('radiogroup', name='Color scheme')
            scheme_options = scheme.get_by_role('radio')
            icon_options = panel.get_by_role('radiogroup', name='Tile background').get_by_role('radio')
            default_scheme = scheme.get_by_role('radio', checked=True)
            expect_radio_options(scheme_options, ['Auto', 'Light', 'Dark'])
            expect_radio_options(icon_options, ['Gradient', 'Solid color'])
            expect(default_scheme).to_have_accessible_name('Auto')
            panel.locator('.setting-color-scheme:visible').get_by_role('radio', name='Dark', exact=True).check()
            expect(desktop).to_have_class('server-desktop theme-dark desktop-dark')
            expect(panel.locator('.setting-tile-background:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Gradient')
            check_interval_style('aurora')
            button('Appearance').click()
            button('Seaside').click()
            expect(desktop).to_have_attribute('data-desktop-icon-style', 'filled')
            seaside_accent = panel.get_by_role('textbox', name='Accent color', exact=True).input_value()
            expect(default_scheme).to_have_accessible_name('Auto')
            expect(desktop).to_have_class('server-desktop theme-light desktop-light')
            expect(panel.locator('.setting-tile-background:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Solid color')
            check_filled_icons()
            check_interval_style('seaside')
            button('Region & language').click()
            region = panel.locator('.desktop-region')
            expect(region.locator('.setting-desktop-language:visible').get_by_role('radio', checked=True)).to_have_accessible_name('English')
            assert region.evaluate('''region => {
                const desktop = region.closest('.server-desktop');
                const color = el => getComputedStyle(el).color;
                return [...region.querySelectorAll('.section-title, .field-label, select')]
                    .every(el => color(el) === color(desktop));
            }''')
            region.screenshot(path=str(out / 'seaside-region.png'))
            button('Appearance').click()
            button('Fjord').click()
            expect(desktop).to_have_attribute('data-desktop-icon-style', 'outline')
            aurora = panel.locator('.theme-preset-grid').get_by_role('button', name='Aurora', exact=True).locator('..')
            expect(aurora.locator('.theme-wallpaper-swatch')).to_have_count(2)
            expect(aurora.locator('.theme-wallpaper-swatch').first).to_have_class(
                'theme-wallpaper-swatch wallpaper-aurora-light',
            )
            themes = panel.get_by_role('group', name='Themes', exact=True)
            active_theme = themes.locator('button[aria-pressed=true]')
            expect(active_theme).to_have_count(1)
            expect(button('Fjord')).to_have_attribute('aria-pressed', 'true')
            expect(default_scheme).to_have_accessible_name('Auto')
            accent = panel.get_by_role('textbox', name='Accent color', exact=True)
            fjord_accent = accent.input_value()
            panel.locator('.setting-color-scheme:visible').get_by_role('radio', name='Light', exact=True).check()
            expect(panel.locator('.setting-color-scheme:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Light')
            button('#249688').click()
            panel.locator('.setting-tile-background:visible').get_by_role('radio', name='Solid color', exact=True).check()
            panel.locator('.setting-icon-family:visible').get_by_role('radio', name='Color icons', exact=True).check()
            button('Seaside wallpaper').click()
            expect(desktop.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-seaside')
            button('Fjord').click()
            expect(default_scheme).to_have_accessible_name('Auto')
            expect(active_theme).to_have_text('Fjord')
            expect(accent).to_have_value(fjord_accent)
            expect(panel.locator('.setting-icon-family:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Line icons')
            expect(panel.locator('.setting-tile-background:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Gradient')
            expect(button('Fjord wallpaper')).to_have_attribute('aria-pressed', 'true')
            expect(desktop.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-fjord')

            expect(button('Customize')).to_have_count(0)
            expect(accent).to_be_visible()
            picker = panel.locator('input[type="color"]')
            expect(picker).to_have_accessible_name('Choose accent color')
            picker.fill('#3388bb')
            expect(accent).to_have_value('#3388bb')
            initial_accent = desktop.evaluate("el => getComputedStyle(el).getPropertyValue('--accent')")
            accent.fill('invalid')
            assert desktop.evaluate("el => getComputedStyle(el).getPropertyValue('--accent')") == initial_accent
            accent.press('Escape')
            expect(accent).to_have_value('#3388bb')
            accent.fill('#112233')
            accent.press('Tab')
            expect(picker).to_have_value('#112233')

            button('#249688').click()
            expect(accent).to_have_value('#249688')
            accent.fill('#123456')
            accent.press('Enter')
            panel.locator('.setting-tile-background:visible').get_by_role('radio', name='Solid color', exact=True).check()
            panel.locator('.setting-color-scheme:visible').get_by_role('radio', name='Dark', exact=True).check()
            expect(panel.locator('.setting-color-scheme:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Dark')
            expect(desktop).to_have_class('server-desktop theme-dark desktop-dark')
            dark_text = desktop.evaluate('e => getComputedStyle(e).color')
            expect(accent).to_have_value('#123456')
            check_filled_icons()
            expect(active_theme).to_have_text('Fjord')

            button('Aurora wallpaper').click()
            expect(desktop.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-aurora')
            panel.locator('.setting-tile-background:visible').get_by_role('radio', name='Gradient', exact=True).check()
            panel.locator('.setting-icon-family:visible').get_by_role('radio', name='Color icons', exact=True).check()
            button('Seaside').click()
            expect(active_theme).to_have_text('Seaside')
            expect(button('Seaside wallpaper')).to_have_attribute('aria-pressed', 'true')
            expect(desktop.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-seaside')
            expect(accent).to_have_value(seaside_accent)
            expect(panel.locator('.setting-icon-family:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Line icons')
            expect(default_scheme).to_have_accessible_name('Auto')
            expect(desktop).to_have_attribute('data-desktop-icon-style', 'filled')
            expect(desktop).to_have_class('server-desktop theme-light desktop-light')
            expect(desktop).not_to_have_css('color', dark_text)
            eventually(lambda: (saved := persist()).get('theme') == 'seaside' and
                       'wallpaper' not in saved and 'overrides' not in saved)
            active_indicator = page.locator('.desktop-dock-app.active')
            assert active_indicator.evaluate('''icon => {
                const indicator = getComputedStyle(icon, '::after');
                return parseFloat(indicator.height) >= 3 && indicator.boxShadow === 'none'
                    && indicator.backgroundColor === 'rgb(255, 255, 255)';
            }''')
            panel.locator('.setting-color-scheme:visible').get_by_role('radio', name='Dark', exact=True).check()
            expect(desktop).to_have_class('server-desktop theme-dark desktop-dark')
            dark_text = desktop.evaluate('e => getComputedStyle(e).color')
            gallery = panel.get_by_role('group', name='Wallpaper', exact=True)
            gallery.get_by_role('button', name='Restore theme wallpaper', exact=True).click()
            expect(gallery.locator('.wallpaper-choice[aria-pressed=true]')).to_have_attribute('aria-label', 'Seaside wallpaper')
            expect(desktop.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-seaside')

            expect(panel.get_by_text('Theme default', exact=True)).to_have_count(0)
            expect(gallery.get_by_role('button', name='Add Picture', exact=True)).to_have_count(0)
            gallery.get_by_role('button', name='Aurora wallpaper', exact=True).click()
            expect(desktop.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-aurora')
            expect_radio_options(icon_options, ['Gradient', 'Solid color'])
            files_symbol = grid.locator('.app-files .desktop-app-icon').first
            symbol_mask = lambda: files_symbol.evaluate("icon => getComputedStyle(icon, '::before').maskImage")
            line_mask = symbol_mask()
            check_app_artwork(page)
            check_symbol_centers(page, '.desktop-dock-app .desktop-app-icon')
            panel.locator('.setting-icon-family:visible').get_by_role('radio', name='Color icons', exact=True).check()
            expect(desktop).to_have_attribute('data-desktop-icon-family', 'color')
            expect(icon_options).to_have_count(0)
            assert symbol_mask() == 'none'
            catalog = app.apps()
            catalog_icons = catalog.locator('.desktop-app-catalog-item .desktop-app-icon')
            expect(catalog_icons.first).to_be_visible()
            for icons in (grid.locator('.desktop-app-icon'),
                          page.locator('.desktop-dock-app .desktop-app-icon'), catalog_icons):
                assert icons.count() > 0
                assert icons.evaluate_all('''icons => icons.every(icon => {
                    const tile = getComputedStyle(icon);
                    const symbol = getComputedStyle(icon, '::before');
                    return tile.backgroundColor === 'rgba(0, 0, 0, 0)' && tile.backgroundImage === 'none'
                        && tile.boxShadow === 'none' && symbol.maskImage === 'none'
                        && symbol.backgroundImage.startsWith('url("data:image/svg+xml;base64,');
                })''')
            check_symbol_centers(page, '.desktop-shortcuts .desktop-app-icon, .desktop-dock-app .desktop-app-icon, .desktop-app-catalog-item .desktop-app-icon')
            catalog.screenshot(path=str(out / 'color-apps.png'))
            close_dialog(catalog)
            panel.locator('.setting-icon-family:visible').get_by_role('radio', name='Line icons', exact=True).check()
            expect(desktop).to_have_attribute('data-desktop-icon-family', 'line')
            expect(panel.locator('.setting-tile-background:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Solid color')
            assert symbol_mask() == line_mask
            saved = eventually(lambda: persist() if persist().get('wallpaper') == 'aurora' and
                                persist().get('overrides', {}).get('icon_family') == 'line' else None)
            assert saved['theme'] == 'seaside'
            assert saved['overrides'] == {'mode': 'Dark', 'icon_family': 'line'}
            expect(active_theme).to_have_count(1)

            page.locator('.workbench-tabs').get_by_role('button', name='Fullscreen desktop').click()
            page.set_viewport_size(dict(width=440, height=750))
            expect(panel).to_have_css('flex-direction', 'row')
            for section in ('Appearance', 'Desktop', 'Dock', 'General', 'About'):
                button(section).click()
                expect(button(section)).to_have_attribute('aria-pressed', 'true')
                assert panel.locator('.desktop-preferences').evaluate('el => el.scrollWidth <= el.clientWidth + 1'), section
                for group in panel.get_by_role('radiogroup').all():
                    assert group.locator('label').evaluate_all("""labels => labels.every(label => {
                        const range = document.createRange();
                        range.selectNodeContents(label.querySelector('span') || label);
                        const text = range.getBoundingClientRect(), area = label.getBoundingClientRect();
                        return text.width > 0 && text.left >= area.left - 1 && text.right <= area.right + 1;
                    })"""), section
                if section == 'Appearance':
                    expect(panel.locator('.setting-color-scheme:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Dark')
                    choices = gallery.locator('.wallpaper-choices')
                    choices.scroll_into_view_if_needed()
                    assert choices.evaluate('''grid => {
                        const tiles = [...grid.children].filter(el => el.classList.contains('wallpaper-entry'));
                        const bounds = grid.getBoundingClientRect();
                        const boxes = tiles.map(tile => tile.getBoundingClientRect());
                        return boxes.length === 8 && boxes.at(-1).top > boxes[0].top &&
                            boxes.every(box => box.left >= bounds.left - 1 && box.right <= bounds.right + 1);
                    }''')
                    panel.screenshot(path=str(out / 'compact-appearance.png'))
                if section == 'Desktop':
                    expect(desktop_size).to_have_value(str(pixels))
                    check_desktop_icon(pixels)
                    check_shortcut_gutter()
                    panel.screenshot(path=str(out / 'compact-desktop-settings.png'))
                    visible = shortcut_options('System app icons')
                    expect(visible.get_by_role('checkbox', name='Home', exact=True)).to_be_checked()
                    assert visible.evaluate('''dialog => {
                        const box = dialog.getBoundingClientRect();
                        return box.left >= 0 && box.right <= innerWidth && box.bottom <= innerHeight;
                    }''')
                    page.keyboard.press('Escape')
                    expect(visible).to_have_count(0)
            page.set_viewport_size(dict(width=1440, height=980))
            page.get_by_role('button', name='Exit desktop fullscreen', exact=True).click()
            page.locator('.desktop-window.active .window-close').click()

            app.application('Files')
            canvas = page.locator('.desktop-canvas')
            point = canvas.evaluate("""canvas => {
                const frame = canvas.getBoundingClientRect();
                for (let y = 10; y < frame.height - 60; y += 15) {
                    for (let x = 10; x < frame.width - 15; x += 15) {
                        const hit = document.elementFromPoint(frame.left + x, frame.top + y);
                        if (hit?.closest('.desktop-canvas') === canvas
                            && !hit.closest('.desktop-window,.desktop-dock-zone')) return {x, y};
                    }
                }
                throw new Error('No desktop drop area');
            }""")

            def drop(name, modifier):
                source = page.locator('.file-row:visible').filter(
                    has=page.get_by_role('button', name=name, exact=True),
                )
                expect(source).to_have_attribute('draggable', 'true')
                source.scroll_into_view_if_needed()
                box = source.bounding_box()
                frame = canvas.bounding_box()
                x, y = box['x'] + box['width'] * 0.65, box['y'] + box['height'] / 2
                page.mouse.move(x, y)
                page.mouse.down()
                page.mouse.move(x + 16, y, steps=4)
                page.keyboard.down(modifier)
                try:
                    x, y = frame['x'] + point['x'], frame['y'] + point['y']
                    page.mouse.move(x, y, steps=6)
                    page.mouse.move(x + 1, y)
                    page.mouse.up()
                finally:
                    page.keyboard.up(modifier)

            drop('copy.txt', 'Control')
            eventually(lambda: (home / 'Desktop/copy.txt').is_file())
            expect(grid.get_by_role('button', name='copy.txt', exact=True)).to_be_visible()
            assert (peer.root / 'copy.txt').is_file()
            drop('move.txt', 'Shift')
            eventually(lambda: (home / 'Desktop/move.txt').is_file())
            expect(grid.get_by_role('button', name='move.txt', exact=True)).to_be_visible()
            assert not (peer.root / 'move.txt').exists()
            drop('Linked folder', 'Alt')
            link = grid.get_by_role('button', name='Linked folder alias', exact=True)
            expect(link).to_be_visible()
            app.show_desktop()
            link.click()
            expect(app.file('inside.txt')).to_be_visible()
            assert (peer.root / 'Linked folder/inside.txt').is_file()

            app.application('Settings')
            button('Dock').click()
            pinned = shortcut_options('System app icons')
            home_pin = pinned.get_by_role('checkbox', name='Home', exact=True)
            trash_pin = pinned.get_by_role('checkbox', name='Trash', exact=True)
            expect(home_pin).not_to_be_checked()
            expect(trash_pin).not_to_be_checked()
            home_pin.check()
            trash_pin.check()
            pinned.screenshot(path=str(out / 'pinned-shortcuts.png'))
            close_dialog(pinned)
            dock = page.locator('.desktop-dock-apps')
            dock_home = dock.get_by_role('button', name='Home', exact=True)
            dock_trash = dock.get_by_role('button', name='Trash', exact=True)
            expect(dock_trash).to_be_visible()
            dock_home.drag_to(dock.locator('.desktop-dock-app').first, target_position={'x': 5, 'y': 14})
            eventually(lambda: persist()['dock']['order'][0] == {'system': 'home'})

            page.get_by_role('button', name='Disconnect', exact=True).click()
            app.connect()
            app.workspace('Desktop')
            expect(desktop).to_have_attribute('data-desktop-icon-style', 'filled')
            expect(desktop).to_have_css('color', dark_text)
            expect(desktop.locator('.desktop-wallpaper-art')).to_have_class('desktop-wallpaper-art wallpaper-aurora')
            expect(desktop.locator('.desktop-wallpaper-copy')).to_have_count(0)
            home_shortcut.screenshot(path=str(out / 'home-shortcut.png'))
            expect(dock.locator('.desktop-dock-app').first).to_have_attribute('aria-label', 'Home')
            dock_home.click()
            expect(app.file('night.svg')).to_be_visible()
            sidebar = page.locator('.desktop-window.app-files.active .files-sidebar')
            for label, launcher in [('Home', dock_home), ('Trash', dock_trash)]:
                sidebar_mask = sidebar.get_by_role('button', name=label, exact=True).locator('.action-glyph').evaluate(
                    'glyph => getComputedStyle(glyph).maskImage',
                )
                assert sidebar_mask == launcher.locator('.desktop-app-icon').evaluate(
                    "icon => getComputedStyle(icon, '::before').maskImage",
                )
            dock_trash.click()
            expect(sidebar.get_by_role('button', name='Trash', exact=True)).to_have_attribute('aria-pressed', 'true')
            dock_trash.click(button='right')
            page.locator('.desktop-context-menu').get_by_role('button', name='Remove from dock', exact=True).click()
            expect(dock_trash).to_have_count(0)
            expect(grid.get_by_role('button', name='Trash', exact=True)).to_be_visible()

    print('Legacy desktop settings, layout, responsive settings, live appearance overrides and drag/drop passed.')


if __name__ == '__main__':
    main()
