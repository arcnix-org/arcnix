"""Validate settings ownership, empty feedback, real I/O errors and saved choices."""
import json, time, re
from desktop_fixture import ROOT, Desktop, Peer, expect, expect_radio_options, eventually


def check_file_checkbox(app, theme):
    app.settings('Files')
    checkbox = app.page.get_by_role('checkbox', name='Use thousands separators', exact=True)
    expect(checkbox).to_be_enabled()
    appearance = checkbox.evaluate("""input => {
        const label = getComputedStyle(input.closest('label'));
        const panel = getComputedStyle(input.closest('.settings-section'));
        return [getComputedStyle(input).colorScheme, input.getBoundingClientRect().width,
                label.color, panel.backgroundColor];
    }""")
    scheme, width, foreground, background = appearance
    def luminance(rgb):
        channels = [int(n) / 255 for n in re.findall(r'\d+', rgb)[:3]]
        linear = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
        return sum(c * w for c, w in zip(linear, (0.2126, 0.7152, 0.0722)))
    low, high = sorted(map(luminance, (foreground, background)))
    assert scheme == theme and round(width) == 16 and (high + 0.05) / (low + 0.05) >= 4.5, appearance
    before = checkbox.is_checked()
    checkbox.focus()
    checkbox.press('Space')
    assert checkbox.is_checked() != before
    if checkbox.is_checked():
        assert checkbox.evaluate("e => getComputedStyle(e, '::before').content !== 'none'")
        app.screenshot(f'file-checkbox-checked-{theme}.png')
    checkbox.set_checked(before)
    app.screenshot(f'file-checkbox-{theme}.png')


def main():
    out=ROOT/'artifacts/settings-structure-ui'/str(time.time_ns()); out.mkdir(parents=True)
    checks=[]
    with Peer(out) as peer:
        (out/'settings-check.txt').write_text('settings fixture')
        config=peer.config([peer.profile('Settings fixture')],out,updates=dict(automatic_checks=False,automatic_install=False))
        config['config']['settings']['file_associations'] = {'docx': {'opened_with': ['office', 'future_app']}}
        with Desktop(out,config) as app:
            page=app.page
            app.settings('General')
            debug = page.get_by_role('switch', name='Debug mode', exact=True)
            diagnostic = page.locator('.config-diagnostic')
            expect(debug).not_to_be_checked()
            expect(diagnostic).not_to_be_visible()
            debug.check()
            expect(diagnostic).to_contain_text('future_app')
            debug.uncheck()
            expect(diagnostic).not_to_be_visible()
            assert json.loads(app.config.read_text(encoding='utf-8'))['config']['settings']['file_associations']['docx']['opened_with'] == ['office']
            debug.check()
            diagnostic.get_by_role('button').click()
            page.locator('.setting-theme:visible').get_by_role('radio', name='Light', exact=True).check()
            check_file_checkbox(app, 'light')
            checks.append('Unknown file handlers are repaired, and the configuration diagnostic follows the Debug mode switch')
            expected={
                'General':['Language','Theme','Diagnostics','Configuration backup'],
                'Connection':['Timeout and keepalive'],
                'Files':['Hidden files','File sizes','Dates and times','Default apps'],
                'Transfers':['Concurrent transfers','Existing files'], 'Monitoring':['Resource refresh interval'],
                'Logging':['Session activity','Local log'], 'Updates':['Updates'], 'About':['Application'],
            }
            for category,titles in expected.items():
                app.settings(category)
                expect(page.locator('.page-header-title')).to_have_text(category)
                expect(page.locator('.settings-section:visible .settings-section-title')).to_have_text(titles)
                expect(page.locator('.settings-page .software-error:visible')).to_have_count(0)
                if category!='Updates': expect(page.locator('.update-panel')).not_to_be_visible()
            expect(page.locator('.context-links').get_by_role('button',name='Appearance',exact=True)).to_have_count(0)
            expect(page.locator('.context-links').get_by_role('button').last).to_have_text('About')
            checks.append('Each settings category owns its intended sections, with update controls under Updates')
            app.settings('About'); expect(page.locator('.about-identity .subtle')).to_have_text(re.compile(r'Version \d+\.\d+\.\d+'))
            page.locator('.about-identity .subtle').dblclick(position={'x': 8, 'y': 8})
            assert page.evaluate('window.getSelection().toString().trim()') == 'Version'
            expect(page.get_by_role('button',name='Download update',exact=True)).not_to_be_visible()
            expect(page.get_by_role('button',name='Restart and install',exact=True)).not_to_be_visible()
            app.screenshot('about-light.png')
            expect(page.get_by_role('button',name='Project on GitHub',exact=True)).to_be_visible()
            expect(page.get_by_role('button',name='Official website · arcnix.org',exact=True)).to_be_visible()
            expect(page.get_by_role('button',name='Check now',exact=True)).not_to_be_visible()
            checks.append('About displays the installed version and repository link, and is the last settings category')
            app.settings('General')
            page.get_by_role('button', name='Back up configuration…', exact=True).click()
            backup = page.get_by_role('dialog', name='Back up configuration', exact=True)
            backup.get_by_role('textbox', name='Backup folder', exact=True).fill(str(out))
            backup.get_by_role('button', name='Review configuration backup', exact=True).click()
            backup.get_by_role('button', name='Save configuration backup', exact=True).click()
            view_activity = page.get_by_role('button', name='View operation log', exact=True)
            view_activity.click()
            expect(view_activity).not_to_be_visible()
            open_log = page.get_by_role('button', name='Open local log', exact=True)
            expect(open_log).to_be_visible()
            app.screenshot('activity-local-log.png')
            open_log.focus()
            open_log.press('Enter')
            expect(page.locator('.local-log-feedback')).to_contain_text('Automatically save logs to this computer')
            assert not (out/'logs').exists()
            app.settings('Logging')
            restore_log = page.get_by_role('button', name='Restore logging defaults', exact=True)
            expect(restore_log).to_be_disabled()
            expect(page.locator('.settings-section:visible .settings-restore')).to_have_count(0)
            page.get_by_role('button', name='View session activity', exact=True).click()
            expect(page.locator('.notifications-page')).to_be_visible()
            app.settings('Logging')
            export=page.get_by_role('button',name='Export session log…',exact=True)
            assert export.bounding_box()['width']<300
            expect(page.get_by_role('textbox',name='Save logs in',exact=True)).not_to_be_visible()
            page.get_by_role('switch',name='Automatically save logs to this computer',exact=True).check()
            expect(page.locator('.local-log-feedback')).not_to_be_visible()
            open_log.click()
            expect(page.locator('.local-log-feedback')).to_contain_text('next activity')
            directory=page.get_by_role('textbox',name='Save logs in',exact=True)
            expect(directory).to_have_value(str(out/'logs'))
            directory.press('Control+a')
            assert directory.evaluate('el => el.value.slice(el.selectionStart, el.selectionEnd)') == str(out/'logs')
            expect(page.get_by_role('button',name='Apply',exact=True)).to_be_disabled()
            app.screenshot('logging-light.png')
            invalid = out/'invalid-logs'
            (invalid/'arcnix.log').mkdir(parents=True)
            directory.fill(str(invalid)); page.get_by_role('button',name='Apply',exact=True).click()
            open_log.click()
            expect(page.locator('.local-log-feedback')).to_contain_text(str(invalid/'arcnix.log'))
            bad=out/'not-a-directory'; bad.write_text('preserve this file')
            directory.fill(str(bad)); page.get_by_role('button',name='Apply',exact=True).click()
            app.servers(); app.connect(); app.settings('Logging')
            expect(page.locator('.log-error')).to_contain_text('Log file error:',timeout=10000)
            assert page.locator('.log-error').inner_text().strip() and bad.read_text()=='preserve this file'
            good=out/'valid-logs'
            directory.fill(str(good)); page.get_by_role('button',name='Apply',exact=True).click()
            expect(page.locator('.log-error')).not_to_be_visible()
            app.servers(); app.file('settings-check.txt',False).click(button='right'); app.action('Copy selected paths')
            eventually(lambda: (good/'arcnix.log').is_file())
            app.settings('Logging'); expect(page.locator('.log-error')).not_to_be_visible()
            checks.append('Logging separates display/export from optional file output, collapses inactive options, uses a compact export action, shows real I/O errors only when present and clears recovered errors')
            checks.append('Local log actions in Activity and Logging explain missing files and report the configured path on errors')
            app.settings('Files')
            sizes=page.locator('.settings-section').filter(has=page.get_by_text('File sizes',exact=True))
            expect_radio_options(sizes.get_by_role('radiogroup',name='File size units').get_by_role('radio'), ['Decimal · KB / MB', 'IEC · KiB / MiB'])
            expect_radio_options(sizes.get_by_role('radiogroup',name='Decimal places').get_by_role('radio'), ['0', '1', '2'])
            preview = sizes.locator('.preference-preview')
            expect(preview).to_have_text('Preview: 123.5 MB')
            sizes.locator('.setting-file-size-units:visible').get_by_role('radio', name='IEC · KiB / MiB', exact=True).check()
            sizes.locator('.setting-decimal-places:visible').get_by_role('radio', name='2', exact=True).check()
            expect(preview).to_have_text('Preview: 117.74 MiB')
            dates=page.locator('.settings-section').filter(has=page.get_by_text('Dates and times',exact=True))
            dates.locator('.setting-date-format:visible').get_by_role('radio', name='ISO 8601', exact=True).check()
            sizes.locator('.settings-card-heading').get_by_role('button', name='Restore file size defaults', exact=True).click()
            expect(preview).to_have_text('Preview: 123.5 MB')
            eventually(lambda: json.loads(app.config.read_text(encoding='utf-8'))['config']['settings']['display']['units']=='Decimal')
            assert json.loads(app.config.read_text(encoding='utf-8'))['config']['settings']['display']['dates']=='Iso'
            app.screenshot('files-light.png')
            app.settings('General'); page.locator('.setting-theme:visible').get_by_role('radio', name='Dark', exact=True).check()
            check_file_checkbox(app, 'dark')
            app.settings('Logging'); app.screenshot('logging-dark.png')
            page.set_viewport_size(dict(width=850,height=780)); app.screenshot('logging-narrow.png')
            assert page.locator('.settings-page').evaluate('(el)=>el.scrollWidth<=el.clientWidth')
            app.settings('General'); page.locator('.setting-language:visible').get_by_role('radio', name='简体中文', exact=True).check()
            page.locator('.context-links').get_by_role('button',name='关于',exact=True).click()
            expect(page.locator('.page-header-title')).to_have_text('关于')
            expect(page.locator('.about-identity .subtle')).to_have_text(re.compile(r'版本 \d+\.\d+\.\d+'))
            app.screenshot('about-chinese-dark.png')
            checks.append('File size previews switch units and precision and reset to decimal with one place; settings support dark/light themes, narrow windows and live Chinese translation')
            app.stop(); app.launch(); page=app.page
            expect(page.locator('.config-diagnostic')).not_to_be_visible()
            assert json.loads(app.config.read_text(encoding='utf-8'))['config']['settings']['debug_mode']
            page.locator('.settings-link').click(); page.locator('.context-links').get_by_role('button',name='日志设置',exact=True).click()
            expect(page.get_by_role('switch',name='自动保存日志到本机',exact=True)).to_be_checked()
            track = page.locator('.log-save-switch .sapid-switch-track')
            expect(track).not_to_have_css('background-color', 'rgba(0, 0, 0, 0)')
            expect(page.get_by_role('textbox',name='保存文件夹',exact=True)).to_have_value(str(good))
            app.screenshot('logging-chinese-on.png')
            checks.append('A fresh process restores the same language, theme and file log preferences after the navigation restructure')
            current_log = good/'arcnix.log'
            previous_bytes = current_log.read_bytes()
            page.get_by_role('checkbox', name='在新记录前显示时间', exact=True).uncheck()
            page.get_by_role('combobox', name='单个文件大小上限', exact=True).select_option(label='50 MiB')
            page.get_by_role('combobox', name='保留的旧日志文件数', exact=True).select_option(label='5')
            page.get_by_role('textbox', name='保存文件夹', exact=True).fill(str(out/'unsaved'))
            before_restore = json.loads(app.config.read_bytes())['config']['settings']
            restore_log = page.get_by_role('button', name='恢复日志默认设置', exact=True)
            restore_log.click()
            expect(restore_log).to_be_disabled()
            expect(page.get_by_role('switch', name='自动保存日志到本机', exact=True)).not_to_be_checked()
            expect(page.get_by_role('checkbox', name='在新记录前显示时间', exact=True)).to_be_checked()
            expect(page.get_by_role('textbox', name='保存文件夹', exact=True)).not_to_be_visible()
            after_restore = json.loads(app.config.read_bytes())['config']['settings']
            assert after_restore.pop('logging') == dict(timestamps=True, write_file=False, directory=None, max_size_mib=10, retained_files=3)
            before_restore.pop('logging')
            assert after_restore == before_restore and current_log.read_bytes().startswith(previous_bytes)
            app.screenshot('logging-chinese-off.png')
            app.settings('常规')
            page.locator('.setting-theme:visible').get_by_role('radio', name='浅色', exact=True).check()
            app.settings('日志设置')
            page.screenshot(path=str(out/'logging-chinese-light.png'), animations='disabled')
            autosave = page.get_by_role('switch', name='自动保存日志到本机', exact=True)
            autosave.focus()
            autosave.press('Space')
            expect(autosave).to_be_checked()
            expect(track).not_to_have_css('background-color', 'rgba(0, 0, 0, 0)')
            expect(page.get_by_role('textbox', name='保存文件夹', exact=True)).to_have_value(str(out/'logs'))
            page.screenshot(path=str(out/'logging-chinese-light-on.png'), animations='disabled')
            checks.append('One scoped reset restores all log preferences and clears the folder draft while preserving saved log contents and other settings')
    result=dict(passed=len(checks),checks=checks,artifacts=str(out)); (out/'results.json').write_text(json.dumps(result,indent=2)); print(json.dumps(result))


if __name__=='__main__':main()
