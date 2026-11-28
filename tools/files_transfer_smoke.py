"""Exercise Files drag transfers and the shared transfer controls in WebView2."""
import re
import time

from desktop_fixture import Desktop, Peer, ROOT, eventually, expect


def drop_to_local(page, row):
    target = page.locator('.workspace-header .files-download-drop')
    expect(row).to_be_visible()
    src = row.bounding_box()
    x, y = src['x'] + 100, src['y'] + src['height'] / 2
    page.mouse.move(x, y)
    page.mouse.down()
    page.mouse.move(x + 15, y - 14, steps=5)
    expect(target).to_be_visible()
    dest = target.bounding_box()
    dx, dy = dest['x'] + dest['width'] / 2, dest['y'] + dest['height'] / 2
    page.mouse.move(dx, dy, steps=12)
    page.mouse.move(dx + 1, dy, steps=2)
    page.mouse.up()
    return page.get_by_role('dialog', name='Download directory', exact=True)


def main():
    out = ROOT / 'artifacts' / 'files-transfer' / f'run-{time.time_ns()}'
    local = out / 'local'
    incoming = out / 'incoming'
    destination = out / 'downloaded'
    local.mkdir(parents=True)
    incoming.mkdir()
    destination.mkdir()
    source = incoming / 'drop 中文.txt'
    payload = bytes(range(256)) * 24576
    source.write_bytes(payload)

    with Peer(out / 'peer', io_delay=.012) as peer, Desktop(
        out / 'desktop', peer.config([peer.profile('Files transfers')], local),
    ) as app:
        download_name = 'download-with-controls.bin'
        download_data = bytes(range(256)) * 32768
        (peer.root / download_name).write_bytes(download_data)
        cancelled_name = 'cancel-paused.bin'
        (peer.root / cancelled_name).write_bytes(download_data)
        remote_folder = peer.root / 'download-folder'
        remote_folder.mkdir()
        (remote_folder / 'nested.txt').write_bytes(b'nested download')
        page = app.page
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.emulate_media(reduced_motion='reduce')
        app.connect()
        app.workspace('Desktop')
        app.application('Files')
        pane = page.locator('.files-pane')
        expect(pane).to_have_class(re.compile('files-upload-ready'))
        tools = page.locator('.files-window-tools')
        for label in ('Transfer files', 'Upload', 'Download'):
            expect(tools.get_by_role('button', name=label, exact=True)).to_have_count(0)

        header = page.locator('.workspace-header .desktop-transfers')
        upload = header.get_by_role('button', name=re.compile('^Upload details'))
        download = header.get_by_role('button', name=re.compile('^Download details'))
        expect(upload).to_be_hidden()
        expect(download).to_be_hidden()

        def expect_running(button):
            ring = button.locator('..').get_by_role('progressbar')
            expect(ring).to_be_visible()
            expect(ring).to_have_attribute('aria-valuenow', re.compile(r'^[1-9]\d?$'))
            expect(button).to_have_text('')
            # The direction arrow is centered inside the actual painted ring.
            arrow = button.locator('.action-glyph').bounding_box()
            circle = ring.bounding_box()
            for axis, size in [('x', 'width'), ('y', 'height')]:
                assert circle[size] > arrow[size]
                assert abs(circle[axis] + circle[size] / 2 - arrow[axis] - arrow[size] / 2) < 1

        box = pane.bounding_box()
        client = page.context.new_cdp_session(page)
        drop = dict(x=box['x'] + box['width'] / 2, y=box['y'] + box['height'] / 2,
                    data=dict(items=[], files=[str(source)], dragOperationsMask=1))
        for event in ('dragEnter', 'dragOver'):
            client.send('Input.dispatchDragEvent', dict(type=event, **drop))
        expect(pane).to_have_attribute('data-file-drop-target', 'true')
        client.send('Input.dispatchDragEvent', dict(type='drop', **drop))
        expect_running(upload)
        expect(download).to_be_hidden()
        expect(upload).to_have_accessible_name('Upload details · Completed', timeout=25000)
        expect(upload.locator('..').get_by_role('progressbar')).to_be_hidden()
        assert (peer.root / source.name).read_bytes() == payload
        upload.click()
        upload_dialog = page.get_by_role('dialog', name='Upload details', exact=True)
        expect(upload_dialog.locator('.queue-row')).to_have_count(1)
        expect(upload_dialog.locator('.job-name')).to_contain_text(source.name)
        upload_dialog.get_by_role('button', name='Close', exact=True).click()

        row = pane.locator('.file-row', has=page.get_by_role('button', name=download_name, exact=True))
        row.click(button='right')
        expect(page.locator('.file-context-menu').get_by_role('button', name='Download', exact=True)).to_have_count(0)
        page.keyboard.press('Escape')
        choose = drop_to_local(page, row)
        folder = choose.get_by_role('textbox', name='Local folder', exact=True)
        expect(folder).to_have_value(str(local))
        folder.fill(str(destination / 'missing'))
        choose.get_by_role('button', name='Download', exact=True).click()
        expect(choose).to_contain_text('Choose an existing local directory.')
        folder.fill(str(destination))
        choose.get_by_role('button', name='Download', exact=True).click()
        expect_running(download)
        header.screenshot(path=str(out / 'transfer-rings.png'))
        expect(upload).to_be_visible()
        download.click()
        dialog = page.get_by_role('dialog', name='Download details', exact=True)
        job = dialog.locator('.queue-row')
        expect(job.locator('.job-name')).to_contain_text(download_name)
        expect(job.locator('.job-percent')).to_have_text(re.compile(r"^(?!0\.0%|100\.0%)\d"))
        job.get_by_role('button', name='Pause transfer', exact=True).click()
        expect(job.locator('.job-status')).to_have_text('Paused')
        expect(download.locator('..')).to_have_class(re.compile(r'\bpaused\b'))
        time.sleep(.45)
        before = job.locator('.job-percent').inner_text()
        time.sleep(.45)
        assert job.locator('.job-percent').inner_text() == before
        app.screenshot('paused-download.png')
        job.get_by_role('button', name='Resume transfer', exact=True).click()
        expect(job.locator('.job-status')).to_have_text('Completed', timeout=25000)
        assert (destination / download_name).read_bytes() == download_data
        assert not (local / download_name).exists()
        dialog.get_by_role('button', name='Close', exact=True).click()

        app.workspace('Transfer files')
        remote = page.locator('.remote-slot:visible .file-pane')
        app.file(cancelled_name, remote=True).click()
        remote.get_by_role('button', name='Download', exact=True).click()
        job = page.locator('.queue-row').last
        expect(job.locator('.job-percent')).to_have_text(re.compile(r"^(?!0\.0%|100\.0%)\d"))
        job.get_by_role('button', name='Pause transfer', exact=True).click()
        expect(job.locator('.job-status')).to_have_text('Paused')
        job.get_by_role('button', name='Cancel transfer', exact=True).click()
        expect(job.locator('.job-status')).to_have_text(re.compile(r'^Cancel(l)?ed$'))
        assert not (local / cancelled_name).exists()

        page.get_by_role('button', name='Pause queue', exact=True).click()
        app.file(source.name, remote=True).click()
        remote.get_by_role('button', name='Download', exact=True).click()
        job = page.locator('.queue-row').last
        expect(job.locator('.job-status')).to_have_text('Queued')
        job.get_by_role('button', name='Pause transfer', exact=True).click()
        expect(job.locator('.job-status')).to_have_text('Paused')
        page.get_by_role('button', name='Resume queue', exact=True).click()
        expect(job.locator('.job-status')).to_have_text('Paused')
        job.get_by_role('button', name='Cancel transfer', exact=True).click()
        expect(job.locator('.job-status')).to_have_text(re.compile(r'^Cancel(l)?ed$'))
        assert not (local / source.name).exists()
        app.workspace('Desktop')
        app.application('Monitor')
        app.application('Settings')
        settings = page.locator('.desktop-window.app-settings')
        settings.locator('.window-minimize').click()
        windows = page.locator('.desktop-window:visible')
        monitor = page.locator('.desktop-window.app-monitor')
        expect(windows).to_have_count(2)
        expect(monitor).to_have_class(re.compile(r'\bactive\b'))
        bounds = {app: page.locator(f'.desktop-window.app-{app}').bounding_box()
                  for app in ('files', 'monitor')}
        app.show_desktop()
        expect(windows).to_have_count(0)
        app.show_desktop()
        expect(windows).to_have_count(2)
        expect(monitor).to_have_class(re.compile(r'\bactive\b'))
        expect(settings).to_be_hidden()
        for application, box in bounds.items():
            assert page.locator(f'.desktop-window.app-{application}').bounding_box() == box

        app.application('Files')
        choose = drop_to_local(page, app.file(remote_folder.name).locator('..'))
        choose.get_by_role('textbox', name='Local folder', exact=True).fill(str(destination))
        choose.get_by_role('button', name='Download', exact=True).click()
        expect(choose).to_have_count(0)
        eventually(lambda: (destination / remote_folder.name / 'nested.txt').read_bytes() == b'nested download')
        assert not errors, errors
        print('Files WebView file drop, direction-specific pause/resume/cancel and Show Desktop restoration passed.')


if __name__ == '__main__':
    main()
