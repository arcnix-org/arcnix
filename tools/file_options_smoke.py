"""Isolated WebView regressions for cancellable trees, permissions and preferences."""
from pathlib import Path
import json, os, re, socket, subprocess, sys, time, urllib.request

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'artifacts/python'))
from playwright.sync_api import sync_playwright, expect
from desktop_fixture import executable


def main():
    out = ROOT / 'artifacts/file-options-ui'
    out.mkdir(parents=True, exist_ok=True)
    ready = out / f'fixture-{time.time_ns()}.json'
    log = (out / 'desktop.log').open('w', encoding='utf-8')
    fixture = subprocess.Popen([sys.executable, str(ROOT / 'tools/sftp_fixture.py'), '--ready', str(ready)], cwd=ROOT, stdout=log, stderr=log)
    app = browser = page = None
    checks = []
    try:
        deadline = time.monotonic() + 20
        while not ready.exists():
            assert fixture.poll() is None and time.monotonic() < deadline
            time.sleep(.1)
        peer = json.loads(ready.read_text())
        host, port = peer['address'].rsplit(':', 1)
        assert host == '127.0.0.1'
        remote = Path(peer['root'])
        local = out / f'local-{time.time_ns()}'
        local.mkdir()
        (local / 'local.txt').write_bytes(b'local information')
        (remote / 'permissions-file.txt').write_bytes(b'permission bytes stay unchanged')
        (remote / 'permissions-folder').mkdir()
        (remote / 'permissions-folder' / 'child.txt').write_bytes(b'child must keep permissions')
        slow = remote / 'fixture-slow-delete'
        for index in range(24):
            child = slow / f'nested-{index}'
            child.mkdir(parents=True)
            (child / 'keep.txt').write_bytes(b'cancel test')
        config = out / 'config.json'
        profile = dict(id='options-first', name='Options first', host=host, port=int(port), username='arcnix', auth='PrivateKey', private_key=peer['client_key_ed25519'], initial_remote_path='/')
        config.write_text(json.dumps(dict(version=1, config=dict(servers=[profile, dict(profile, id='options-second', name='Options second')], groups=[], trusted_hosts=[dict(host=host, port=int(port), fingerprint=peer['host_key_rsa_fingerprint'])], local_directory=str(local), settings=dict(language='English', theme='Light', show_hidden_files=True, resource_interval_seconds=3)))))
        binary = executable()
        with sync_playwright() as p:
            def launch():
                nonlocal app, browser, page
                with socket.socket() as sock:
                    sock.bind(('127.0.0.1', 0))
                    debug = sock.getsockname()[1]
                endpoint = f'http://127.0.0.1:{debug}'
                env = os.environ.copy()
                env['WEBVIEW2_USER_DATA_FOLDER'] = str(out / f'webview-{time.time_ns()}')
                env['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'] = f'--remote-debugging-port={debug} --remote-debugging-address=127.0.0.1'
                app = subprocess.Popen([str(binary), '--config', str(config)], cwd=ROOT, env=env, stdout=log, stderr=log)
                deadline = time.monotonic() + 20
                while True:
                    try:
                        urllib.request.urlopen(endpoint + '/json/version', timeout=1).close()
                        break
                    except Exception:
                        assert app.poll() is None and time.monotonic() < deadline
                        time.sleep(.1)
                browser = p.chromium.connect_over_cdp(endpoint)
                page = browser.contexts[0].pages[0]
                page.set_default_timeout(10000)
                page.set_viewport_size(dict(width=1440, height=980))
                expect(page.locator('.host-select')).to_have_count(2)
                page.locator('.host-select').first.click()

            def connect():
                page.locator('.overview-transfer').click()
                expect(page.locator('.connection-state')).to_have_text('SSH connected')

            def file(pane, name):
                return page.locator(pane).get_by_role('button', name=name, exact=True)

            def action(name):
                page.locator('.file-context-menu').get_by_role('button', name=name, exact=True).click()

            def properties(name, pane='.remote-slot'):
                file(pane, name).click(button='right')
                action('Properties')
                dialog = page.get_by_role('dialog', name='Properties', exact=True)
                expect(dialog).to_be_visible()
                return dialog

            def octal(dialog):
                mode = dialog.get_by_role('textbox', name='Octal permissions', exact=True)
                expect(mode).to_be_enabled()
                return mode

            def save(dialog):
                dialog.get_by_role('button', name='Save permissions', exact=True).click()
                expect(dialog.locator('.property-feedback')).to_have_text('Permissions saved')

            def close(dialog):
                dialog.get_by_role('button', name='Close dialog', exact=True).click()
                expect(dialog).not_to_be_visible()

            def columns(pane):
                page.locator(pane).get_by_role('button', name='Remote file actions' if pane == '.remote-slot' else 'Local file actions', exact=True).click()
                page.locator('.file-actions-menu').get_by_role('button', name='Visible columns', exact=True).click()
                return page.get_by_role('dialog', name='Visible columns', exact=True)

            launch()
            connect()
            file('.remote-slot', 'fixture-slow-delete').click(button='right')
            action('Delete permanently')
            deletion = page.get_by_role('dialog', name='Delete permanently', exact=True)
            deletion.get_by_role('button', name='Delete permanently', exact=True).click()
            expect(deletion.locator('.file-task-progress')).to_contain_text('Scanning:')
            expect(deletion.get_by_role('button', name='Close dialog', exact=True)).to_be_enabled()
            start = time.monotonic()
            deletion.get_by_role('button', name='Stop deletion', exact=True).click()
            expect(deletion).not_to_be_visible()
            assert time.monotonic() - start < 2
            expect(page.locator('.file-task-status')).not_to_be_visible(timeout=5000)
            assert len(list(slow.rglob('keep.txt'))) == 24
            checks.append('Nested tree scanning shows live progress; Cancel and close remain enabled and scan cancellation preserves all files')

            file('.remote-slot', 'fixture-slow-delete').click(button='right')
            action('Delete permanently')
            deletion.get_by_role('button', name='Delete permanently', exact=True).click()
            expect(deletion.locator('.file-task-progress')).to_contain_text(re.compile(r'Deleting: [1-9]'), timeout=15000)
            page.screenshot(path=str(out / 'deleting-progress.png'))
            close(deletion)
            expect(page.locator('.file-task-status')).not_to_be_visible(timeout=5000)
            assert 0 < len(list(slow.rglob('keep.txt'))) < 24
            checks.append('Deletion reports actual completed counts, permits closing during execution, and stops before removing the remaining tree')

            dialog = properties('permissions-file.txt')
            mode = octal(dialog)
            expect(dialog).to_contain_text('Owner / Group')
            mode.fill('888')
            expect(dialog.get_by_role('button', name='Save permissions', exact=True)).to_be_disabled()
            mode.fill('4755')
            dialog.get_by_role('checkbox', name='Others Write', exact=True).check()
            expect(mode).to_have_value('4757')
            save(dialog)
            page.screenshot(path=str(out / 'properties-permissions.png'))
            close(dialog)
            dialog = properties('permissions-file.txt')
            expect(octal(dialog)).to_have_value('4757')
            close(dialog)
            assert (remote / 'permissions-file.txt').read_bytes() == b'permission bytes stay unchanged'
            checks.append('Structured remote properties load real metadata, validate octal modes, synchronize rwx controls, preserve special bits and save modes through SFTP')

            page.locator('.remote-slot').get_by_role('button', name='Expand permissions-folder', exact=True).click()
            dialog = properties('child.txt')
            child_mode = octal(dialog).input_value()
            close(dialog)
            dialog = properties('permissions-folder')
            octal(dialog).fill('0750')
            save(dialog)
            close(dialog)
            page.locator('.remote-slot').get_by_role('button', name='Expand permissions-folder', exact=True).click()
            dialog = properties('child.txt')
            expect(octal(dialog)).to_have_value(child_mode)
            close(dialog)
            file('.remote-slot', 'permissions-file.txt').click()
            file('.remote-slot', 'permissions-folder').click(modifiers=['Control'])
            file('.remote-slot', 'permissions-file.txt').click(button='right')
            action('Properties')
            dialog = page.get_by_role('dialog', name='Properties', exact=True)
            expect(octal(dialog)).to_have_value('')
            octal(dialog).fill('0700')
            save(dialog)
            close(dialog)
            dialog = properties('local.txt', '.local-slot')
            expect(dialog).to_contain_text('ACLs')
            expect(dialog.get_by_role('button', name='Save permissions', exact=True)).to_have_count(0)
            close(dialog)
            checks.append('Directory permission changes leave children unchanged; mixed multiple selection requires an explicit mode and local Windows ACL information is read-only')

            dialog = columns('.local-slot')
            dialog.get_by_role('checkbox', name='Size', exact=True).uncheck()
            dialog.get_by_role('checkbox', name='Type', exact=True).check()
            dialog.get_by_role('checkbox', name='Permissions', exact=True).check()
            dialog.get_by_role('button', name='Done', exact=True).click()
            expect(page.locator('.local-slot .column-size')).to_have_count(0)
            expect(page.locator('.local-slot .column-type')).to_be_visible()
            dialog = columns('.remote-slot')
            dialog.get_by_role('checkbox', name='Modified', exact=True).uncheck()
            dialog.get_by_role('checkbox', name='Permissions', exact=True).check()
            page.screenshot(path=str(out / 'visible-columns.png'))
            dialog.get_by_role('button', name='Done', exact=True).click()
            expect(page.locator('.remote-slot .column-permissions')).to_be_visible()
            expect(page.locator('.remote-slot .column-modified')).to_have_count(0)
            persisted = json.loads(config.read_text())['config']['settings']['folder_views']
            local_columns = next(iter(persisted['local-windows'].values()))['columns']
            assert local_columns['kind'] and not local_columns['size']
            remote_columns = persisted['server:options-first']['/']['columns']
            assert remote_columns['permissions'] and not remote_columns['modified']
            page.locator('.host-select').nth(1).click()
            connect()
            expect(page.locator('.local-slot .column-size')).to_have_count(0)
            expect(page.locator('.remote-slot .column-modified')).to_be_visible()
            expect(page.locator('.remote-slot .column-permissions')).to_have_count(0)
            checks.append('Column visibility updates headers and rows, persists by folder, and switching servers retains local choices while isolating remote ones')

            browser.close()
            app.terminate()
            app.wait(timeout=10)
            launch()
            connect()
            expect(page.locator('.local-slot .column-size')).to_have_count(0)
            expect(page.locator('.local-slot .column-type')).to_be_visible()
            expect(page.locator('.remote-slot .column-permissions')).to_be_visible()
            expect(page.locator('.remote-slot .column-modified')).to_have_count(0)
            page.locator('.host-select').nth(1).click()
            connect()
            expect(page.locator('.local-slot .column-size')).to_have_count(0)
            dialog = columns('.remote-slot')
            dialog.get_by_role('checkbox', name='Type', exact=True).check()
            dialog.get_by_role('button', name='Restore default columns', exact=True).click()
            dialog.get_by_role('button', name='Done', exact=True).click()
            expect(page.locator('.remote-slot .column-type')).to_have_count(0)
            page.set_viewport_size(dict(width=1000, height=700))
            dialog = properties('permissions-file.txt')
            expect(octal(dialog)).to_have_value('0700')
            page.screenshot(path=str(out / 'properties-narrow.png'))
            close(dialog)
            checks.append('A new desktop process reloads server-specific column preferences, restore-defaults stays isolated, and permission modes persist on the SSH peer')
            result = dict(passed=len(checks), checks=checks)
            (out / 'results.json').write_text(json.dumps(result, indent=2))
            print(json.dumps(result), flush=True)
    except Exception:
        if page:
            try:
                page.screenshot(path=str(out / 'failure.png'))
                (out / 'failure.txt').write_text(page.locator('body').inner_text(), encoding='utf-8')
            except Exception:
                pass
        raise
    finally:
        if app and app.poll() is None:
            app.terminate()
            app.wait(timeout=10)
        fixture.terminate()
        fixture.wait(timeout=10)
        log.close()


if __name__ == '__main__':
    main()
