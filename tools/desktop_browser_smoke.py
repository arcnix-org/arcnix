"""Run the Desktop browser against an owned HTTP site reached only over SSH forwarding."""
import base64
from contextlib import contextmanager
import ctypes
from ctypes import wintypes
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import re
import shutil
import socket
import threading
import time
from urllib.parse import parse_qs, urlsplit

from server_apps_fixture import read_desktop_settings
from desktop_fixture import Desktop, Peer, ROOT, eventually, executable, expect


class Site(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def do_GET(self):
        cookie = self.headers.get('Cookie', '')
        set_cookie = None
        content_type = 'text/html; charset=utf-8'
        if self.path == '/first':
            body = """<title>Loading fixture</title><h1>Server network</h1>
                      <script>
                      const socket = new WebSocket('ws://' + location.host + '/socket');
                      socket.onmessage = async (event) => {
                          if (event.data === 'ok') document.title = await (await fetch('/api')).text()
                      };
                      </script>"""
            set_cookie = 'arcnix_fixture=first; Path=/; SameSite=Lax; Max-Age=3600'
        elif self.path == '/socket':
            key = self.headers['Sec-WebSocket-Key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
            accept = base64.b64encode(hashlib.sha1(key.encode()).digest()).decode()
            self.send_response(101)
            self.send_header('Upgrade', 'websocket')
            self.send_header('Connection', 'Upgrade')
            self.send_header('Sec-WebSocket-Accept', accept)
            self.end_headers()
            self.wfile.write(b'\x81\x02ok\x88\x00')
            self.close_connection = True
            return
        elif self.path == '/api':
            body = 'Routed first page'
        elif self.path == '/second':
            body = '<title>Routed second page</title><h1>Second tab</h1>'
        elif self.path == '/theme':
            body = """<title>Checking page theme</title>
                      <style>
                      :root { color-scheme: light dark; --page-theme: Light; }
                      @media (prefers-color-scheme: dark) { :root { --page-theme: Dark; } }
                      </style>
                      <h1>Theme-aware page</h1>
                      <script>
                      const mode = matchMedia('(prefers-color-scheme: dark)');
                      let changes = 0;
                      function report() {
                        const css = getComputedStyle(document.documentElement).getPropertyValue('--page-theme').trim();
                        const js = mode.matches ? 'Dark' : 'Light';
                        document.title = css === js ? js + ' page (' + changes + ')' : 'Theme mismatch';
                      }
                      mode.addEventListener('change', () => { changes++; report(); });
                      report();
                      </script>"""
        elif self.path == '/styled':
            port = self.server.server_port
            body = f"""<title>Fixture missing resources</title><h1 id="sample">Page content</h1>
                       <script>
                       const styled = () => {{
                         if (window.fixtureScript && getComputedStyle(document.getElementById('sample')).color === 'rgb(26, 89, 172)')
                           document.title = 'Styles and scripts loaded';
                       }};
                       </script>
                       <link rel="stylesheet" href="http://assets.browser-fixture.test:{port}/style.css" onload="styled()">
                       <script defer src="http://scripts.browser-fixture.test:{port}/fixture.js" onload="styled()"></script>"""
        elif self.path == '/style.css':
            content_type = 'text/css'
            body = '#sample { color: rgb(26, 89, 172); }'
        elif self.path == '/fixture.js':
            content_type = 'text/javascript'
            body = 'window.fixtureScript = true;'
        elif self.path == '/cookie':
            body = '<title>' + ('Cookie kept' if 'arcnix_fixture=first' in cookie else 'Isolated profile') + '</title>'
        elif self.path in ('/sync/start', '/sync/update'):
            updated = self.path.endswith('/update')
            value = 'synthetic-cookie-secret-updated' if updated else 'synthetic-cookie-secret'
            set_cookie = f'arcnix_sync={value}; Path=/; SameSite=Lax; HttpOnly'
            body = '<title>Session cookie ' + ('updated' if updated else 'created') + '</title>'
        elif self.path == '/sync/check':
            title = 'Updated session cookie restored' if 'arcnix_sync=synthetic-cookie-secret-updated' in cookie else (
                'Session cookie restored' if 'arcnix_sync=synthetic-cookie-secret' in cookie else 'No session cookie')
            body = f'<title>{title}</title>'
        elif self.path == '/loopback':
            body = """<title>Checking browser identity</title>
                      <script>
                      (async () => {
                        const data = await navigator.userAgentData?.getHighEntropyValues(['fullVersionList']);
                        const identity = JSON.stringify({agent: navigator.userAgent, data});
                        await fetch('/identity?' + new URLSearchParams({identity}));
                        document.title = 'Remote loopback';
                      })();
                      </script>"""
        elif self.path.startswith('/identity?'):
            identity = parse_qs(urlsplit(self.path).query)['identity'][0]
            self.server.identity = dict(
                user_agent=self.headers.get('User-Agent'),
                brands=self.headers.get('Sec-CH-UA'),
                full_versions=self.headers.get('Sec-CH-UA-Full-Version-List'),
                client=json.loads(identity),
            )
            body = 'Recorded'
        else:
            body = '<title>Fixture</title>'
        data = body.encode()
        self.send_response(200)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        if self.path == '/loopback':
            self.send_header('Accept-CH', 'Sec-CH-UA-Full-Version-List')
        if set_cookie:
            self.send_header('Set-Cookie', set_cookie)
        self.end_headers()
        try:
            self.wfile.write(data)
        except OSError:
            pass

    def log_message(self, *_args):
        pass


class IPv6Site(ThreadingHTTPServer):
    address_family = socket.AF_INET6


@contextmanager
def site():
    with ThreadingHTTPServer(('127.0.0.1', 0), Site) as server, IPv6Site(
        ('::1', server.server_port), Site,
    ) as ipv6:
        servers = (server, ipv6)
        threads = [threading.Thread(target=peer.serve_forever, daemon=True) for peer in servers]
        for thread in threads:
            thread.start()
        try:
            yield server
        finally:
            for peer in servers:
                peer.shutdown()
            for thread in threads:
                thread.join()


def native_regions(pid, points=()):
    """Read only the regions of visible Wry HWNDs owned by this test process."""
    user = ctypes.windll.user32
    gdi = ctypes.windll.gdi32
    callback = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    user.IsWindowVisible.argtypes = [wintypes.HWND]
    user.GetWindowRgn.argtypes = [wintypes.HWND, wintypes.HANDLE]
    user.EnumWindows.argtypes = [callback, wintypes.LPARAM]
    user.EnumChildWindows.argtypes = [wintypes.HWND, callback, wintypes.LPARAM]
    gdi.CreateRectRgn.restype = wintypes.HANDLE
    gdi.DeleteObject.argtypes = [wintypes.HANDLE]
    gdi.PtInRegion.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_int]
    regions = []

    @callback
    def child(handle, _):
        name = ctypes.create_unicode_buffer(64)
        user.GetClassNameW(handle, name, len(name))
        if name.value == 'WRY_WEBVIEW' and user.IsWindowVisible(handle):
            clip = gdi.CreateRectRgn(0, 0, 0, 0)
            try:
                kind = user.GetWindowRgn(handle, clip)
                if kind:
                    regions.append([bool(gdi.PtInRegion(clip, x, y)) for x, y in points])
            finally:
                gdi.DeleteObject(clip)
        return True

    @callback
    def top(handle, _):
        owner = wintypes.DWORD()
        user.GetWindowThreadProcessId(handle, ctypes.byref(owner))
        if owner.value == pid:
            user.EnumChildWindows(handle, child, 0)
        return True

    user.EnumWindows(top, 0)
    return regions


def expect_clipping(bridge, pid, overlap):
    viewport = json.loads(bridge.input_value())
    area = viewport['area']
    frame = viewport['frame']
    width = area['right'] - area['left']
    height = area['bottom'] - area['top']
    points = [(x, y) for x in range(6, width, 12) for y in range(6, height, 40)]
    expected = [
        frame['left'] <= x + area['left'] < frame['right']
        and frame['top'] <= y + area['top'] < frame['bottom']
        and not any(clip['left'] <= x + area['left'] < clip['right']
                and clip['top'] <= y + area['top'] < clip['bottom']
                for clip in viewport['clip'])
        for x, y in points
    ]
    assert any(expected) and (not all(expected)) == overlap
    eventually(lambda: native_regions(pid, points) == [expected])


def main():
    out = ROOT / 'artifacts' / 'desktop-browser' / f'run-{time.time_ns()}'
    out.mkdir(parents=True)
    binary = out / 'arcnix.exe'
    shutil.copy2(executable(), binary)
    with site() as server, Peer(out / 'first', browser_port=server.server_port) as first, Peer(
        out / 'second', browser_port=server.server_port,
    ) as second, Peer(out / 'denied', browser_port=server.server_port) as denied:
        port = server.server_port
        config = first.config([first.profile('Browser server A'), second.profile('Browser server B'),
                               denied.profile('Forwarding disabled')], out)
        first_settings = first.root / 'home/arcnix/.arcnix/desktop.json'
        first_settings.parent.mkdir(parents=True, exist_ok=True)
        first_settings.write_text(json.dumps({
            'version': 1, 'theme': 'System', 'appearance': {'dock': {'floating_status': True}},
        }), encoding='utf-8')
        second_settings = second.root / 'home/arcnix/.arcnix/desktop.json'
        second_settings.parent.mkdir(parents=True, exist_ok=True)
        second_settings.write_text(json.dumps({'version': 1, 'theme': 'Dark'}), encoding='utf-8')
        for peer in (second, denied):
            trust = peer.config([], out)['config']['trusted_hosts']
            config['config']['trusted_hosts'].extend(trust)
        # Let the application select separate shell and SSH browser profiles.
        with Desktop(out / 'desktop', config, binary=binary,
                     extra_env={'WEBVIEW2_USER_DATA_FOLDER': ''}) as app:
            page = app.page
            page.emulate_media(reduced_motion='reduce')
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            app.connect()
            app.application('Browser')
            browser = page.locator('.desktop-window.app-browser')
            address = browser.get_by_role('textbox', name='Web address', exact=True)
            label = browser.locator('.browser-tab.active .browser-tab-label')
            bridge = browser.locator('.browser-viewport-input')
            endpoint = f'http://browser-fixture.test:{port}'

            def visit(url, title):
                address.fill(url)
                address.press('Enter')
                expect(label).to_have_text(title, timeout=20000)

            visit(endpoint + '/first', 'Routed first page')
            expect_clipping(bridge, app.process.pid, overlap=False)
            eventually(lambda: bridge.evaluate('''bridge => {
                const layer = bridge.closest('.desktop-window-layer').getBoundingClientRect();
                const canvas = bridge.closest('.desktop-canvas').getBoundingClientRect();
                const frame = JSON.parse(bridge.value).frame;
                return frame && layer.bottom < canvas.bottom &&
                    ['left', 'top', 'right', 'bottom'].every(edge =>
                        Math.abs(frame[edge] - Math.round(canvas[edge] * devicePixelRatio)) <= 1);
            }'''))
            dock = page.locator('.desktop-dock-zone').bounding_box()
            grip = browser.locator('.desktop-window-mark').bounding_box()
            x = grip['x'] + grip['width'] / 2
            y = grip['y'] + grip['height'] / 2
            delta = dock['y'] - y - 110
            page.mouse.move(x, y)
            page.mouse.down()
            page.mouse.move(x, y + delta, steps=8)
            page.mouse.up()
            expect(bridge).to_have_value(re.compile(r'"clip":\[\{'))
            expect_clipping(bridge, app.process.pid, overlap=True)
            clock = page.locator('.desktop-dock .desktop-panel-clock')
            clock.hover()
            tooltip = clock.locator('.desktop-clock-tooltip')
            expect(tooltip).to_be_visible()
            eventually(lambda: bridge.evaluate('''bridge => {
                const tooltip = document.querySelector('.desktop-clock-tooltip');
                const box = tooltip.getBoundingClientRect();
                return JSON.parse(bridge.value).clip.some(clip => ['left', 'top', 'right', 'bottom'].every(edge =>
                    clip[edge] === Math.round(box[edge] * devicePixelRatio)));
            }'''))
            expect_clipping(bridge, app.process.pid, overlap=True)
            page.mouse.move(x, y + delta)
            page.mouse.down()
            page.mouse.move(x, y, steps=8)
            page.mouse.up()
            expect(bridge).to_have_value(re.compile(r'"clip":\[\]'))
            path = first.root / 'browser-forwarding.jsonl'
            assert any(entry['host'] == 'browser-fixture.test'
                       for entry in [json.loads(line) for line in path.read_text().splitlines()])

            visit(endpoint + '/second', 'Routed second page')
            browser.get_by_role('button', name='Back', exact=True).click()
            expect(label).to_have_text('Routed first page')
            browser.get_by_role('button', name='Forward', exact=True).click()
            expect(label).to_have_text('Routed second page')

            browser.get_by_role('button', name='New web tab', exact=True).last.click()
            expect(browser.locator('.browser-tab')).to_have_count(2)
            assert browser.locator('.browser-tabs').evaluate("""tabs => {
                const center = selector => {
                    const rect = tabs.querySelector(selector).getBoundingClientRect();
                    return rect.top + rect.height / 2;
                };
                return Math.abs(center('.glyph-plus') - center('.browser-tab.active .glyph-close')) < 1;
            }""")
            visit(endpoint + '/cookie', 'Cookie kept')
            browser.get_by_role('button', name='Routed second page', exact=True).click()
            expect(label).to_have_text('Routed second page')

            browser.get_by_role('button', name='Toggle bookmark', exact=True).click()
            browser.get_by_role('button', name='Bookmarks', exact=True).click()
            bookmark = browser.locator('.browser-bookmark-panel').get_by_role('button', name='Routed second page', exact=True)
            expect(bookmark).to_be_visible()
            saved = first.root / 'home/arcnix/.arcnix/desktop.json'
            eventually(lambda: bool(read_desktop_settings(saved).get('browser_bookmarks')))
            bookmark.click()

            visit(f'http://127.0.0.1:{port}/loopback', 'Remote loopback')
            assert any(json.loads(line)['host'] == '127.0.0.1' for line in path.read_text().splitlines())
            identity = server.identity
            agent = identity['user_agent']
            assert agent == identity['client']['agent'], identity
            version = re.search(r' Chrome/(\d+)\.0\.0\.0 Safari/537\.36$', agent)
            assert version, identity
            major = version[1]
            assert f'"Google Chrome";v="{major}"' in identity['brands'], identity
            assert f'"Chromium";v="{major}"' in identity['brands'], identity
            assert 'Edge' not in identity['brands'], identity
            client = identity['client']['data']
            assert client['platform'] == 'Windows' and client['mobile'] is False, identity
            assert {'brand': 'Google Chrome', 'version': major} in client['brands'], identity
            full_version = next(item['version'] for item in client['fullVersionList']
                                if item['brand'] == 'Google Chrome')
            assert full_version.split('.', 1)[0] == major, identity
            assert f'"Google Chrome";v="{full_version}"' in identity['full_versions'], identity
            print(agent, flush=True)
            print(identity['brands'], flush=True)

            visit(f'localhost:{port}/second', 'Routed second page')
            expect(address).to_have_value(f'http://localhost:{port}/second')
            for host in ('http-only.browser-fixture.test', '192.0.2.87'):
                visit(host + '/second', 'Routed second page')
                expect(address).to_have_value(f'http://{host}/second')
            explicit = 'https://http-only.browser-fixture.test/second'
            address.fill(explicit)
            address.press('Enter')
            expect(browser.locator('.browser-error')).to_be_visible(timeout=20000)
            expect(address).to_have_value(explicit)
            visit(f'[::1]:{port}/loopback', 'Remote loopback')
            expect(address).to_have_value(f'http://[::1]:{port}/loopback')
            targets = {json.loads(line)['host'] for line in path.read_text().splitlines()}
            assert {'127.0.0.1', 'localhost', '::1', 'browser-fixture.test'} <= targets, targets

            app.application('Settings')
            expect(browser).to_be_visible()
            expect(bridge).to_have_value(re.compile(r'"clip":\[\{'))
            expect_clipping(bridge, app.process.pid, overlap=True)
            app.show_desktop()
            expect(browser).to_be_hidden()
            eventually(lambda: not native_regions(app.process.pid))
            app.show_desktop()
            expect(browser).to_be_visible()
            expect(label).to_have_text('Remote loopback')
            expect_clipping(bridge, app.process.pid, overlap=True)

            app.application('Browser')
            browser.locator('.window-maximize').click()
            expect(browser).to_have_class(re.compile(r'\bmaximized\b'))
            expect_clipping(bridge, app.process.pid, overlap=False)
            browser.locator('.window-maximize').click()
            expect(browser).not_to_have_class(re.compile(r'\bmaximized\b'))
            before = path.read_bytes()
            address.fill('file:///C:/Windows/win.ini')
            address.press('Enter')
            expect(browser.locator('.browser-error')).to_have_text('Enter an HTTP or HTTPS address')
            assert path.read_bytes() == before

            tabs = browser.locator('.browser-tab .browser-tab-label')
            tabs.first.click()
            visit(endpoint + '/theme', 'Light page (0)')
            tabs.nth(1).click()
            visit(endpoint + '/theme', 'Light page (0)')
            app.application('Settings')
            settings = page.locator('.desktop-settings-page')
            settings.get_by_role('button', name='Appearance', exact=True).click()
            for count, (choice, effective) in enumerate([
                ('Aurora', 'Dark'), ('Seaside', 'Light'), ('Aurora', 'Dark'), ('Fjord', 'Light'),
            ], start=1):
                settings.get_by_role('button', name=choice, exact=True).click()
                expect(settings.locator('.setting-color-scheme:visible').get_by_role('radio', checked=True)).to_have_accessible_name('Auto')
                if effective == 'Dark':
                    settings.locator('.setting-color-scheme:visible').get_by_role('radio', name='Dark', exact=True).check()
                title = f'{effective} page ({count})'
                expect(tabs.nth(1)).to_have_text(title)
                expect(page.locator('.app')).to_have_class(re.compile(r'\btheme-light\b'))
                # A hidden WebView can defer its media query event until it is shown.
                app.application('Browser')
                tabs.first.click()
                expect(label).to_have_text(title)
                tabs.nth(1).click()
                app.application('Settings')
            app.application('Browser')

            browser.locator('.window-close').click()
            app.application('Browser')
            visit(endpoint + '/cookie', 'Cookie kept')
            page.get_by_role('button', name='Disconnect', exact=True).click()
            expect(browser).to_have_count(0)

            app.connect(index=1)
            app.application('Browser')
            expect(page.locator('.server-desktop')).to_have_class(re.compile(r'\btheme-dark\b'))
            visit(endpoint + '/theme', 'Dark page (0)')
            visit(endpoint + '/cookie', 'Isolated profile')
            browser.get_by_role('button', name='Bookmarks', exact=True).click()
            expect(browser.locator('.browser-bookmark-panel')).to_have_text('No bookmarks')
            browser.get_by_role('button', name='Bookmarks', exact=True).click()
            unavailable = second.root / 'browser-destination-unreachable'
            unavailable.touch()
            address.fill(f'http://127.0.0.1:{port}/second')
            browser.get_by_role('button', name='Open web address', exact=True).click()
            expect(browser.locator('.browser-error')).to_have_text(
                'The server cannot connect to this address', timeout=20000)
            unavailable.unlink()
            browser.get_by_role('button', name='Retry page', exact=True).click()
            expect(label).to_have_text('Routed second page', timeout=20000)

            visit(endpoint + '/styled', 'Fixture missing resources')
            resources = browser.locator('.browser-resource-warning')
            expect(resources).to_contain_text('SSH blocked some page resources', timeout=15000)
            resources.get_by_role('button', name='Enable SSH forwarding…', exact=True).click()
            dialog = page.get_by_role('dialog', name='Configure SSH forwarding', exact=True)
            expect(dialog).to_contain_text('arcnix to forward TCP connections to any host and port')
            dialog.get_by_role('button', name='Apply and reconnect', exact=True).click()
            expect(label).to_have_text('Styles and scripts loaded', timeout=25000)
            expect(resources).to_have_count(0)
            assert json.loads((second.root / 'browser-forwarding-repaired.json').read_text()) == {'arcnix': True}
            page.get_by_role('button', name='Disconnect', exact=True).click()

            denied_policy = denied.root / 'browser-forwarding-denied'
            denied_policy.touch()
            app.connect(index=2)
            app.application('Browser')
            address.fill('https://www.example.com/')
            browser.get_by_role('button', name='Open web address', exact=True).click()
            expect(browser.locator('.browser-error')).to_have_text(
                'SSH forwarding was denied for this address', timeout=20000)
            expect(browser.get_by_role('button', name='Open in local browser', exact=True)).to_have_count(0)
            for host in ('127.0.0.1', 'localhost', '[::1]'):
                browser.get_by_role('button', name='New web tab', exact=True).last.click()
                expect(label).to_have_text('New web tab')
                address.fill(f'http://{host}:{port}/loopback')
                browser.get_by_role('button', name='Open web address', exact=True).click()
                expect(browser.locator('.browser-error')).to_have_text(
                    'SSH forwarding was denied for this address', timeout=20000)
                expect(browser.get_by_role('button', name='Open in local browser', exact=True)).to_have_count(0)
            repair = browser.get_by_role('button', name='Enable SSH forwarding…', exact=True)
            allowed = denied.root / 'browser-forwarding-repaired.json'
            repair.click()
            dialog = page.get_by_role('dialog', name='Configure SSH forwarding', exact=True)
            expect(dialog).to_contain_text('arcnix to forward TCP connections to any host and port')
            dialog.get_by_role('button', name='Cancel', exact=True).click()
            assert not allowed.exists()

            admin_required = denied.root / 'browser-forwarding-admin-required'
            admin_required.touch()
            repair.click()
            dialog.get_by_role('button', name='Apply and reconnect', exact=True).click()
            expect(browser.locator('.browser-error-panel .error')).to_have_text(
                'Administrator access is required to change SSH forwarding.', timeout=10000)
            assert not allowed.exists()
            repair.click()
            dialog.get_by_role('textbox', name='Administrator password (optional)', exact=True).fill('fixture-admin-secret')
            dialog.get_by_role('button', name='Apply and reconnect', exact=True).click()
            expect(label).to_have_text('Remote loopback', timeout=25000)
            assert json.loads(allowed.read_text()) == {'arcnix': True}
            assert denied_policy.exists()
            expect(browser.locator('.browser-tab')).to_have_count(4)
            expect(page.locator('.connection-state')).to_have_text('SSH connected')
            visit(f'http://localhost:{port}/loopback', 'Remote loopback')
            visit(f'http://[::1]:{port}/styled', 'Styles and scripts loaded')
            expect(resources).to_have_count(0)
            assert json.loads(allowed.read_text()) == {'arcnix': True}
            assert not errors, errors
            print('Browser rendering over SSH, live theme sync, diagnostics, and account-wide forwarding passed.')


if __name__ == '__main__':
    main()
