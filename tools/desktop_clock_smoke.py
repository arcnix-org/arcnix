"""Desktop clocks follow the connected server across time zones and Dock modes."""
from datetime import datetime, timezone
import json
import re
import time

from desktop_fixture import Desktop, Peer, ROOT, close_dialog, eventually, expect, set_desktop_mode
from server_apps_fixture import read_desktop_settings
from update_smoke import ReleaseServer


def set_clock(peer, epoch, offset, dock, *, zone='', **preferences):
    (peer.root / 'desktop-clock.json').write_text(json.dumps({
        'unix_seconds': epoch, 'offset': offset, 'zone': zone,
    }), encoding='utf-8')
    settings = peer.root / 'home/arcnix/.arcnix/desktop.json'
    settings.parent.mkdir(parents=True, exist_ok=True)
    settings.write_text(json.dumps({
        'version': 1, 'theme': 'System', 'appearance': {'dock': dock}, **preferences,
    }), encoding='utf-8')


def main():
    language_scope = (
        'Changes the display language of this Arcnix desktop only. '
        'The server manages its system language separately.'
    )
    out = ROOT / 'artifacts/desktop-clock' / f'run-{time.time_ns()}'
    out.mkdir(parents=True)
    with Peer(out / 'east') as east, Peer(out / 'west') as west, ReleaseServer(b'', '0.0.0') as updates:
        updates.mode = 'none'
        epoch = int(datetime(2026, 11, 17, 22, 17, tzinfo=timezone.utc).timestamp())
        set_clock(east, epoch, '+0545', {'panel_mode': True}, time_zone='Asia/Shanghai')
        set_clock(west, epoch, '-0700', {}, zone='MST', clock={'timezone': 'local', 'show_seconds': True})
        locale = west.root / 'system-locale'
        system_locale = 'LANG="zh_CN.utf8"\nLC_TIME=de_DE.UTF-8\n'
        locale.write_text(system_locale, encoding='utf-8')
        system_zone = west.root / 'system-time-zone'
        system_zone.write_text('America/Phoenix\n', encoding='utf-8')
        (west.root / 'time-zone-choices.txt').write_text(
            'Africa/Abidjan\tCI\nAfrica/Accra\tGH\nAmerica/Adak\tUS\n'
            'Africa/Addis_Ababa\tET\nAustralia/Adelaide\tAU\nAsia/Aden\tYE\n'
            'Africa/Algiers\tDZ\nAsia/Amman\tJO\nEurope/Amsterdam\tNL\n'
            'Europe/Andorra\tAD\nAsia/Ankara\tTR\nEurope/Athens\tGR\n'
            'America/Bahia\tBR\nAsia/Baku\tAZ\nEurope/London\tGB\n',
            encoding='utf-8',
        )
        config = east.config([east.profile('Eastern test server'), west.profile('Western test server')], out)
        config['config']['settings']['theme'] = 'Dark'
        config['config']['trusted_hosts'].extend(west.config([], out)['config']['trusted_hosts'])
        with Desktop(out / 'desktop', config, extra_env={'ARCNIX_TEST_UPDATE_URL': updates.base + '/latest'}) as app:
            page = app.page
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            cdp = page.context.new_cdp_session(page)
            cdp.send('Emulation.setTimezoneOverride', {'timezoneId': 'America/Los_Angeles'})
            assert page.evaluate('Intl.DateTimeFormat().resolvedOptions().timeZone') == 'America/Los_Angeles'

            app.connect(0)
            app.workspace('Desktop')
            clock = page.locator('.dock-panel-zone .desktop-panel-clock')
            clock.hover()
            detail = clock.locator('.desktop-clock-tooltip')
            expect(detail).to_be_visible()
            expect(detail).to_contain_text('Wednesday, November 18, 2026')
            expect(detail).to_contain_text('UTC+05:45')
            expect(clock.locator('.desktop-panel-date')).to_have_text('11/18/2026')
            display = clock.locator('.desktop-panel-time')
            expect(display).to_have_text(re.compile(r'^4:02:[0-5]\d\s+AM$'))
            expect(display).not_to_have_text(display.inner_text(), timeout=3000)
            log = (east.root / 'server-app-queries.jsonl').read_text(encoding='utf-8')
            assert log.count('# arcnix:desktop:clock') == 1

            page.get_by_role('button', name='Disconnect', exact=True).click()
            app.connect(1)
            app.workspace('Desktop')
            clock = page.locator('.desktop-dock-status .desktop-panel-clock')
            dock_date = clock.locator('.desktop-panel-date')
            dock_time = clock.locator('.desktop-panel-time')
            expect(dock_date).to_have_text('11/17/2026')
            expect(dock_date).to_have_css('opacity', '1')
            expect(dock_date).to_have_css('font-weight', dock_time.evaluate('el => getComputedStyle(el).fontWeight'))
            expect(dock_time).to_have_text(re.compile(r'^3:17:[0-5]\d\s+PM$'))

            app.application('Settings')
            panel = page.locator('.desktop-settings-page')

            def button(name):
                return panel.get_by_role('button', name=name, exact=True)

            zone = panel.locator('.desktop-time-zone-current')
            picker = page.get_by_role('dialog', name='Select time zone', exact=True)
            search = picker.get_by_role('textbox', name='Search cities and time zones', exact=True)
            results = picker.locator('.time-zone-options')
            status = page.locator('.app-settings > .desktop-window-titlebar > .desktop-settings-saving')

            def open_zones():
                zone.click()
                expect(picker).to_be_visible()
                expect(search).to_be_focused()

            def set_zone(name):
                open_zones()
                search.fill(name.split('/')[-1].replace('_', ' '))
                picker.get_by_role('button', name=name, exact=True).click()
                expect(picker).to_have_count(0)

            button('Date & time').click()
            format_panel = panel.locator('.desktop-date-time')
            format_choices = format_panel.get_by_role('radiogroup', name='Time format')
            restore_format = format_panel.get_by_role('button', name='Restore default time format', exact=True)
            def right_edge(control):
                box = control.bounding_box()
                return box['x'] + box['width']
            assert abs(right_edge(format_choices) - right_edge(restore_format)) < 2
            preview = panel.locator('.desktop-time-preview')
            expect(preview).to_have_attribute('title', 'Server time (UTC-07:00)')
            expect(zone).to_have_text('America/Phoenix')
            expect(panel.locator('.time-zone-summary')).to_have_text('Server time zone · UTC-07:00')
            open_zones()
            expect(results.get_by_role('button').first).to_have_attribute('aria-label', 'Africa/Abidjan')
            assert results.get_by_role('button').count() > 12
            assert results.evaluate('list => list.scrollHeight > list.clientHeight')
            search.fill('united states phoenix')
            expect(results.get_by_role('button')).to_have_count(1)
            phoenix = results.get_by_role('button', name='America/Phoenix', exact=True)
            expect(phoenix).to_have_attribute('aria-pressed', 'true')
            expect(phoenix.locator('.time-zone-country')).to_have_text('United States')
            expect(phoenix.locator('.time-zone-offset')).to_have_text('UTC-07:00')
            expect(phoenix.locator('.time-zone-local-time')).to_have_text(re.compile(r'Tue,? 15:17'))
            search.press('ArrowDown')
            expect(phoenix).to_be_focused()
            phoenix.press('Escape')
            expect(picker).to_have_count(0)
            expect(zone).to_be_focused()
            open_zones()
            expect(search).to_have_value('')
            search.fill('unavailable zone')
            expect(results.get_by_text('No matching time zones')).to_be_visible()
            picker.get_by_role('button', name='Close dialog', exact=True).click()
            expect(picker).to_have_count(0)
            expect(panel.locator('.setting-time-format:visible').get_by_role('radio', checked=True)).to_have_accessible_name('AM / PM')
            panel.locator('.setting-time-format:visible').get_by_role('radio', name='24-hour', exact=True).check()
            expect(status).to_be_hidden()
            expect(preview.locator('.desktop-panel-time')).to_have_text(re.compile(r'^15:17:[0-5]\d$'))
            expect(clock.locator('.desktop-panel-time')).to_have_text(re.compile(r'^15:17:[0-5]\d$'))
            expect(panel.get_by_role('switch')).to_have_count(0)
            panel.screenshot(path=str(out / 'date-and-time.png'))
            button('Dock').click()
            clock_settings = panel.locator('.dock-clock-settings')
            expect(clock_settings.locator('.setting-time-format:visible').get_by_role('radio', checked=True)).to_have_accessible_name('24-hour')
            clock_settings.locator('.setting-time-format:visible').get_by_role('radio', name='AM / PM', exact=True).check()
            expect(clock.locator('.desktop-panel-time')).to_have_text(re.compile(r'^3:17:[0-5]\d\s+PM$'))
            clock_settings.locator('.setting-time-format:visible').get_by_role('radio', name='24-hour', exact=True).check()
            expect(clock.locator('.desktop-panel-time')).to_have_text(re.compile(r'^15:17:[0-5]\d$'))
            date = clock_settings.get_by_role('switch', name='Date', exact=True)
            seconds = clock_settings.get_by_role('switch', name='Seconds', exact=True)
            weekday = clock_settings.get_by_role('switch', name='Weekday', exact=True)
            slider = panel.get_by_role('slider', name='Dock icon size', exact=True)
            expect(weekday).not_to_be_checked()
            expect(slider).to_have_value('48')
            weekday.check()
            day = clock.locator('.desktop-panel-weekday')
            expect(day).to_have_text('Tue')
            slider.press('Home')
            for _ in range(7):
                slider.press('ArrowRight')
            expect(slider).to_have_value('39')
            expect(weekday).to_have_count(0)
            expect(day).to_be_hidden()
            slider.press('ArrowRight')
            expect(slider).to_have_value('40')
            expect(weekday).to_be_checked()
            expect(day).to_have_text('Tue')
            dock = page.locator('.desktop-dock')
            three_lines = clock.bounding_box()['height']
            height_with_weekday = dock.bounding_box()['height']
            dock.screenshot(path=str(out / 'bottom-weekday-40.png'))
            weekday.uncheck()
            expect(day).to_be_hidden()
            assert abs(dock.bounding_box()['height'] - height_with_weekday) < 1
            print(f'Bottom Dock at 40 px: three clock lines {three_lines:.1f} px, Dock {height_with_weekday:.1f} px')
            date.uncheck()
            seconds.uncheck()
            expect(clock.locator('.desktop-panel-date')).to_be_hidden()
            expect(clock.locator('.desktop-panel-time')).to_have_text('15:17')
            clock.hover()
            detail = clock.locator('.desktop-clock-tooltip')
            expect(detail).to_be_visible()
            expect(detail).to_contain_text('Tuesday, November 17, 2026')
            expect(detail).to_contain_text(re.compile(r'15:17:[0-5]\d'))
            expect(detail).to_contain_text('MST (UTC-07:00)')
            assert detail.evaluate('''el => {
                const box = el.getBoundingClientRect();
                return box.top >= 0 && box.bottom < el.parentElement.getBoundingClientRect().top;
            }''')
            date.check()
            seconds.check()
            status_switch = panel.get_by_role('switch', name='Show dock status bar', exact=True)
            status_switch.click()
            expect(clock_settings).to_have_count(0)
            panel.locator('.setting-dock-position:visible').get_by_role('radio', name='Left side', exact=True).check()
            expect(clock_settings).to_have_count(0)
            status_switch.click()
            side_clock = page.locator('.dock-left .desktop-panel-clock')
            expect(panel.locator('.dock-status-settings').get_by_text('Status bar', exact=True)).to_be_visible()
            expect(weekday).not_to_be_checked()
            expect(date).to_be_checked()
            expect(seconds).to_be_checked()
            west_prefs = west.root / 'home/arcnix/.arcnix/desktop.json'
            position = panel.locator('.setting-dock-position:visible')
            for edge, enabled in [('Right side', False), ('Left side', True)]:
                position.get_by_role('radio', name=edge, exact=True).check()
                seconds.set_checked(enabled)
                slider.press('Home')
                for _ in range(7):
                    slider.press('ArrowRight')
                expect(slider).to_have_value('39')
                expect(seconds).to_have_count(0)
                dock_time = page.locator('.desktop-dock .desktop-panel-time')
                expect(dock_time).to_have_text('15:17')
                eventually(lambda: read_desktop_settings(west_prefs)['clock']['show_seconds'] is enabled)
                position.get_by_role('radio', name='Bottom', exact=True).check()
                expect(seconds).to_be_checked(checked=enabled)
                position.get_by_role('radio', name=edge, exact=True).check()
                expect(seconds).to_have_count(0)
                slider.press('ArrowRight')
                expect(slider).to_have_value('40')
                expect(seconds).to_be_checked(checked=enabled)
                expect(dock_time).to_have_text(re.compile(r'^15:17:[0-5]\d$') if enabled else '15:17')
                if enabled:
                    track = slider.bounding_box()
                    y = track['y'] + track['height'] / 2
                    page.mouse.move(track['x'] + track['width'] / 4, y)
                    page.mouse.down()
                    page.mouse.move(track['x'] + 1, y)
                    expect(slider).to_have_value('32')
                    expect(seconds).to_have_count(0)
                    expect(dock_time).to_have_text('15:17')
                    page.mouse.up()
                    slider.press('End')
                    expect(seconds).to_be_checked()
            eventually(lambda: read_desktop_settings(west_prefs).get('clock', {}).get('format') == 'twenty_four_hour')
            weekday.focus()
            hold = west.root / 'desktop-write-hold'
            hold.touch()
            try:
                started = time.monotonic()
                weekday.press('Space')
                expect(status).to_be_hidden()
                expect(status).to_have_text('Saving…', timeout=7500)
                assert time.monotonic() - started >= 5
                expect(panel.get_by_text('Saving…', exact=True)).to_have_count(0)
                assert status.evaluate('''element => {
                    const note = element.getBoundingClientRect();
                    const bar = element.parentElement;
                    return note.left >= bar.querySelector('.desktop-settings-sidebar-heading').getBoundingClientRect().right - 1
                        && note.right <= bar.querySelector('.desktop-window-actions').getBoundingClientRect().left + 1;
                }''')
            finally:
                hold.unlink()
            expect(status).to_be_hidden()
            date.press('Space')
            seconds.press('Space')
            expect(side_clock.locator('.desktop-panel-weekday')).to_have_text('Tue')
            expect(side_clock.locator('.desktop-panel-date')).to_be_hidden()
            expect(side_clock.locator('.desktop-panel-time')).to_have_text('15:17')
            eventually(lambda: read_desktop_settings(west_prefs).get('clock') == {
                'format': 'twenty_four_hour', 'show_weekday': True, 'show_date': False, 'show_seconds': False,
            })
            panel.locator('.setting-dock-position:visible').get_by_role('radio', name='Bottom', exact=True).check()
            expect(clock_settings.locator('.setting-time-format:visible')).to_be_visible()
            expect(date).not_to_be_checked()
            expect(seconds).not_to_be_checked()
            expect(clock.locator('.desktop-panel-date')).to_be_hidden()
            expect(clock.locator('.desktop-panel-weekday')).to_have_text('Tue')
            expect(clock.locator('.desktop-panel-time')).to_have_text('15:17')
            panel.locator('.setting-dock-position:visible').get_by_role('radio', name='Left side', exact=True).check()
            button('Date & time').click()
            expect(preview.locator('.desktop-panel-date')).to_have_text('11/17/2026')
            zone_denied = west.root / 'system-time-zone-denied'
            zone_denied.touch()
            set_zone('Asia/Shanghai')
            expect(panel.get_by_text(re.compile('Cannot save the server time zone:'))).to_be_visible()
            expect(zone).to_have_text('America/Phoenix')
            assert 'time_zone' not in read_desktop_settings(west_prefs)
            zone_denied.unlink()
            set_zone('Asia/Shanghai')
            expect(zone).to_have_text('Asia/Shanghai')
            expect(side_clock.locator('.desktop-panel-time')).to_have_text('06:17')
            expect(panel.locator('.time-zone-summary')).to_have_text('Server time zone · UTC+08:00')
            assert system_zone.read_text(encoding='utf-8').strip() == 'Asia/Shanghai'
            assert 'time_zone' not in read_desktop_settings(west_prefs)

            button('Region & language').click()
            expect(panel.locator('.setting-file-size-units:visible')).to_be_visible()
            expect(panel.locator('.setting-desktop-language:visible').get_by_role('radio', checked=True)).to_have_accessible_name('English')
            expect(panel.get_by_text(language_scope, exact=True)).to_be_visible()
            panel.locator('.setting-desktop-language:visible').get_by_role('radio', name='简体中文', exact=True).check()
            eventually(lambda: read_desktop_settings(west_prefs).get('language') == 'SimplifiedChinese')
            expect(button('区域与语言')).to_have_attribute('aria-pressed', 'true')
            expect(panel.get_by_text('仅影响当前 Arcnix 桌面的界面语言。服务器的系统语言独立管理。', exact=True)).to_be_visible()
            expect(panel.locator('.setting-file-size-units:visible')).to_be_visible()
            expect(page.locator('.workbench-tabs').get_by_role('button', name='Desktop', exact=True)).to_be_visible()
            expect(page.locator('.desktop-shortcuts').get_by_role('button', name='终端', exact=True)).to_be_visible()
            assert locale.read_text(encoding='utf-8') == system_locale
            log = (west.root / 'server-app-queries.jsonl').read_text(encoding='utf-8')
            assert '# arcnix:desktop:locale' not in log and '# arcnix:desktop:set-locale' not in log
            panel.locator('.setting-desktop-language:visible').get_by_role('radio', name='English', exact=True).check()
            expect(button('Region & language')).to_have_attribute('aria-pressed', 'true')

            set_desktop_mode(panel, 'Dark')
            page.locator('.workbench-tabs').get_by_role('button', name='Fullscreen desktop').click()
            page.set_viewport_size(dict(width=440, height=750))
            button('Dock').click()
            expect(seconds).to_be_visible()
            clock_settings.screenshot(path=str(out / 'compact-dock-clock.png'))
            button('Date & time').click()
            assert panel.locator('.desktop-preferences').evaluate('el => el.scrollWidth <= el.clientWidth + 1')
            panel.screenshot(path=str(out / 'compact-date-and-time.png'))
            open_zones()
            assert picker.evaluate('''card => {
                const box = card.getBoundingClientRect();
                return box.left >= 0 && box.top >= 0 && box.right <= innerWidth && box.bottom <= innerHeight;
            }''')
            assert results.evaluate('list => list.scrollHeight > list.clientHeight')
            picker.screenshot(path=str(out / 'compact-time-zone-picker.png'))
            search.press('Escape')
            expect(picker).to_have_count(0)
            expect(button('Software Updates')).to_have_count(0)
            page.set_viewport_size(dict(width=1440, height=980))
            page.get_by_role('button', name='Exit desktop fullscreen', exact=True).click()
            app.settings('Updates')
            update_panel = page.locator('.update-panel')
            expect(update_panel.locator('.preference-check').first).to_have_css(
                'color', page.locator('.app').evaluate('app => getComputedStyle(app).color'),
            )
            update_panel.get_by_role('button', name='Check now', exact=True).click()
            expect(update_panel.locator('.update-status')).to_have_text('No published release is available yet')
            app.servers()

            page.get_by_role('button', name='Disconnect', exact=True).click()
            app.connect(0)
            app.application('Settings')
            button('Date & time').click()
            east_prefs = east.root / 'home/arcnix/.arcnix/desktop.json'
            expect(zone).to_have_count(0)
            expect(panel.locator('.desktop-time-preview .desktop-panel-time')).to_have_text(re.compile(r'^4:02:[0-5]\d\s+AM$'))
            assert not (east.root / 'system-time-zone').exists()
            # A system file with no identifiable zone also leaves the picker hidden.
            (east.root / 'system-time-zone').touch()
            button('Date & time').click()
            expect(zone).to_have_count(0)
            button('Region & language').click()
            expect(panel.get_by_text(language_scope, exact=True)).to_be_visible()
            panel.locator('.setting-desktop-language:visible').get_by_role('radio', name='简体中文', exact=True).check()
            eventually(lambda: read_desktop_settings(east_prefs).get('language') == 'SimplifiedChinese')
            expect(button('区域与语言')).to_have_attribute('aria-pressed', 'true')
            assert json.loads(app.config.read_text(encoding='utf-8'))['config']['settings']['language'] == 'English'
            assert not errors, errors
            print('Vertical Dock clocks, system time zones, Arcnix desktop language and application Updates passed.')


if __name__ == '__main__':
    main()
