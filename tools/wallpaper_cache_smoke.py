"""Verify remote wallpaper import, local thumbnail assets, hash refresh, and persistence."""
import time
import subprocess
from hashlib import sha256

from PIL import Image

from desktop_fixture import Desktop, Peer, ROOT, eventually, expect
from server_apps_fixture import read_desktop_settings



class WallpaperDesktop(Desktop):
    def launch(self):
        super().launch()
        self.page.on('pageerror', lambda error: print(f'WebView error: {error}', flush=True))
        self.page.on('console', lambda message: print(f'WebView console: {message.text}', flush=True) if message.type == 'error' else None)

    def __exit__(self, exc, *args):
        if exc:
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
            print(f'Desktop process exit status: {self.process.poll()}', flush=True)
            print((self.out / 'desktop.log').read_text(encoding='utf-8', errors='replace'), flush=True)
        super().__exit__(exc, *args)


def appearance(app):
    app.application('Settings')
    panel = app.page.locator('.desktop-settings-page')
    panel.get_by_role('button', name='Appearance', exact=True).click()
    return panel.get_by_role('group', name='Wallpaper', exact=True)


def choose(app, gallery, name):
    gallery.get_by_role('button', name='Add Wallpaper', exact=True).click()
    picker = app.page.get_by_role('dialog', name='Add Wallpaper', exact=True)
    picker.get_by_role('button', name=name, exact=True).click()
    picker.get_by_role('button', name='Add Wallpaper', exact=True).click()
    expect(picker).not_to_be_visible()


def main():
    out = ROOT / 'artifacts/wallpaper-cache' / str(time.time_ns())
    with Peer(out / 'peer') as peer:
        source = peer.root / 'custom 海岸.png'
        Image.new('RGB', (3840, 2160), '#326a91').save(source)
        duplicate = peer.root / 'copy.png'
        duplicate.write_bytes(source.read_bytes())
        # A real 4K source must remain distinct from the small gallery image.
        assert source.stat().st_size < 8 * 1024 * 1024
        original_size = source.stat().st_size
        (peer.root / 'broken.png').write_bytes(b'not an image')
        prefs = peer.root / 'home/arcnix/.arcnix/desktop.json'
        config = peer.config([peer.profile('Wallpaper test')], out)
        with WallpaperDesktop(out / 'desktop', config, extra_env={'RUST_BACKTRACE': '1'}) as app:
            gallery = appearance_after_connect(app)
            expect(gallery.locator('.wallpaper-choice')).to_have_text([''] * 8)
            add = gallery.get_by_role('button', name='Add Wallpaper', exact=True)
            restore = gallery.get_by_role('button', name='Restore theme wallpaper', exact=True)
            for label in ('Restore default theme', 'Restore theme colors', 'Restore theme icons', 'Restore theme wallpaper'):
                action = app.page.locator('.desktop-settings-page').get_by_role('button', name=label, exact=True)
                expect(action).to_have_text('')
                expect(action.locator('.glyph-restore-defaults')).to_be_visible()
            expect(gallery.locator('.wallpaper-custom-mark')).to_have_count(0)
            restore.hover()
            eventually(lambda: restore.evaluate("el => getComputedStyle(el, '::after').visibility === 'visible'"))
            assert restore.evaluate("el => getComputedStyle(el, '::after').content") == '"Restore theme wallpaper"'
            app.page.mouse.move(1, 1)
            app.page.keyboard.press('Tab')
            restore.focus()
            eventually(lambda: restore.evaluate("el => getComputedStyle(el, '::after').visibility === 'visible'"))
            app.screenshot('wallpaper-restore-tooltip.png')
            add.focus()

            assert add.bounding_box()['x'] < restore.bounding_box()['x']
            bounds, restore_bounds = gallery.bounding_box(), restore.bounding_box()
            assert restore_bounds['x'] + restore_bounds['width'] > bounds['x'] + bounds['width'] - 24
            previews = gallery.locator('.wallpaper-preview').evaluate_all(
                'els => els.map(el => getComputedStyle(el).backgroundImage)')
            assert all('/wallpapers/thumbnails/' in url for url in previews)
            themes = app.page.locator('.theme-wallpaper-swatch').evaluate_all(
                'els => els.map(el => getComputedStyle(el).backgroundImage)')
            assert themes and all('/wallpapers/thumbnails/' in url for url in themes)
            sizes = app.page.locator('.wallpaper-preview').evaluate_all("""async elements => Promise.all(elements.map(async el => {
                const image = new Image();
                image.src = getComputedStyle(el).backgroundImage.slice(5, -2);
                await image.decode();
                return [image.naturalWidth, image.naturalHeight];
            }))""")
            assert sizes and all(size == [320, 180] for size in sizes), sizes
            choose(app, gallery, source.name)
            full = app.page.locator('.desktop-wallpaper-image').last
            small = gallery.locator('.custom-wallpaper-thumbnail')
            expect(full).to_be_visible()
            expect(small).to_have_count(1)
            mark = gallery.locator('.custom-wallpaper-entry .wallpaper-custom-mark')
            expect(mark.locator('.glyph-tag')).to_be_visible()
            assert mark.evaluate("el => getComputedStyle(el).pointerEvents") == 'none'
            eventually(lambda: full.evaluate('el => el.complete && el.naturalWidth === 3840'))
            eventually(lambda: small.evaluate('el => el.complete && el.naturalWidth === 320 && el.naturalHeight === 180'))
            full_url = full.get_attribute('src')
            thumbnail_url = small.get_attribute('src')
            assert full_url != thumbnail_url and '-thumb.png' in thumbnail_url
            assert '/wallpaper-cache/' in full_url
            expect(gallery.locator('.custom-wallpaper-choice')).to_have_text('')
            eventually(lambda: read_desktop_settings(prefs).get('appearance', {}).get('wallpaper') == {'remote': '/custom 海岸.png'})
            cache = out / 'desktop/wallpaper-cache'
            digest = sha256(source.read_bytes()).hexdigest()
            cached_image = cache / f'{digest}.png'
            cached_thumbnail = cache / f'{digest}-thumb.png'
            assert cached_image.read_bytes() == source.read_bytes()
            with Image.open(cached_thumbnail) as preview:
                assert preview.size == (320, 180)
            stamp = cached_thumbnail.stat().st_mtime_ns
            gallery.get_by_role('button', name='Custom wallpaper: /custom 海岸.png', exact=True).click()
            expect(gallery.get_by_role('button', name='Add Wallpaper', exact=True)).to_be_enabled()
            assert cached_thumbnail.stat().st_mtime_ns == stamp
            eventually(lambda: len((peer.root / 'wallpaper-hashes.log').read_text().splitlines()) >= 2)
            choose(app, gallery, duplicate.name)
            expect(small).to_have_count(2)
            assert full.get_attribute('src') == full_url
            assert small.evaluate_all('els => els.map(el => el.getAttribute("src"))') == [thumbnail_url] * 2
            assert list(cache.glob('*-thumb.png')) == [cached_thumbnail]
            assert cached_thumbnail.stat().st_mtime_ns == stamp
            Image.new('RGB', (3840, 2160), '#915932').save(source)
            assert source.stat().st_size == original_size
            gallery.get_by_role('button', name='Custom wallpaper: /custom 海岸.png', exact=True).click()
            eventually(lambda: full.get_attribute('src') != full_url)
            copy_entry = gallery.locator('.custom-wallpaper-entry').filter(
                has=app.page.get_by_role('button', name='Custom wallpaper: /copy.png', exact=True))
            expect(copy_entry.locator('.custom-wallpaper-thumbnail')).to_have_attribute('src', thumbnail_url)
            assert cached_image.is_file() and cached_thumbnail.is_file()
            copy_entry.hover()
            copy_entry.get_by_role('button', name='Remove wallpaper').click()
            expect(small).to_have_count(1)
            assert small.get_attribute('src') != thumbnail_url
            eventually(lambda: not cached_image.exists() and not cached_thumbnail.exists())
            assert duplicate.is_file()
            digest = sha256(source.read_bytes()).hexdigest()
            cached_image = cache / f'{digest}.png'
            cached_thumbnail = cache / f'{digest}-thumb.png'
            choose(app, gallery, 'broken.png')
            expect(gallery.locator('.wallpaper-error')).not_to_have_text('')
            expect(gallery.locator('.custom-wallpaper-choice')).to_have_count(1)
            expect(full).to_be_visible()
            restore.click()
            expect(full).not_to_be_visible()
            expect(gallery.locator('.custom-wallpaper-choice')).to_have_count(1)
            assert source.is_file() and cached_thumbnail.is_file()
            gallery.get_by_role('button', name='Custom wallpaper: /custom 海岸.png', exact=True).click()
            expect(full).to_be_visible()
            eventually(lambda: read_desktop_settings(prefs).get('appearance', {}).get('wallpaper') == {'remote': '/custom 海岸.png'})
            app.screenshot('wallpapers.png')
            gallery.screenshot(path=str(out / 'wallpaper-gallery.png'))
            saved_url = full.get_attribute('src')
            (peer.root / 'wallpaper-no-hash-helper').touch()
            app.stop()
            app.launch()
            gallery = appearance_after_connect(app)
            expect(app.page.locator('.desktop-wallpaper-image').last).to_have_attribute('src', saved_url)
            expect(gallery.locator('.custom-wallpaper-thumbnail')).to_have_count(1)
            entry = gallery.locator('.custom-wallpaper-entry')
            entry.hover()
            remove = entry.get_by_role('button', name='Remove wallpaper', exact=True)
            expect(remove).to_have_text('')
            assert gallery.locator('button button').count() == 0
            assert gallery.locator('.wallpaper-entry:not(.custom-wallpaper-entry) .wallpaper-remove').count() == 0
            remove.hover()
            eventually(lambda: remove.evaluate("el => getComputedStyle(el, '::after').visibility === 'visible'"))
            mark_box = gallery.locator('.wallpaper-custom-mark').bounding_box()
            assert mark_box['x'] + mark_box['width'] < remove.bounding_box()['x']
            gallery.screenshot(path=str(out / 'wallpaper-gallery-with-remove.png'))
            before_remove = source.read_bytes()
            remove.click()
            expect(gallery.locator('.custom-wallpaper-choice')).to_have_count(0)
            expect(app.page.locator('.desktop-wallpaper-image')).not_to_be_visible()
            eventually(lambda: not cached_image.exists() and not cached_thumbnail.exists())
            eventually(lambda: read_desktop_settings(prefs).get('appearance', {}).get('wallpapers', []) == [])
            assert source.read_bytes() == before_remove
            choose(app, gallery, source.name)
            expect(gallery.locator('.custom-wallpaper-choice')).to_have_count(1)
            valley = gallery.get_by_role('button', name='Valley wallpaper', exact=True)
            valley.click()
            entry = gallery.locator('.custom-wallpaper-entry')
            entry.hover()
            app.page.keyboard.press('Tab')
            entry.get_by_role('button', name='Remove wallpaper', exact=True).focus()
            entry.get_by_role('button', name='Remove wallpaper', exact=True).click()
            expect(gallery.locator('.custom-wallpaper-choice')).to_have_count(0)
            expect(valley).to_have_attribute('aria-pressed', 'true')
            eventually(lambda: not cached_image.exists() and not cached_thumbnail.exists())
            eventually(lambda: read_desktop_settings(prefs).get('appearance', {}).get('wallpapers', []) == [])
            assert source.read_bytes() == before_remove
            app.stop()
            app.launch()
            gallery = appearance_after_connect(app)
            expect(gallery.locator('.custom-wallpaper-choice')).to_have_count(0)
            expect(gallery.get_by_role('button', name='Valley wallpaper', exact=True)).to_have_attribute('aria-pressed', 'true')

        print(f'Passed wallpaper cache, thumbnail, content refresh, restore tooltip, removal and restart checks: {out}')


def appearance_after_connect(app):
    app.connect()
    return appearance(app)


if __name__ == '__main__':
    main()
