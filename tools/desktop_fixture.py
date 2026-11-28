"""Owned desktop/SSH sandboxes shared by workspace integration checks."""
from pathlib import Path
import json, os, shutil, socket, subprocess, sys, tempfile, time, urllib.request

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'artifacts/python'))
from playwright.sync_api import sync_playwright, expect, Locator


_locator_click = Locator.click
def _traced_locator_click(locator, *args, **kwargs):
    trace = getattr(locator.page, '_arcnix_click_trace', None)
    if trace is None:
        return _locator_click(locator, *args, **kwargs)
    locator.page._arcnix_click_sequence += 1
    sequence = locator.page._arcnix_click_sequence
    def record(phase):
        with trace.open('a',encoding='utf-8') as output:
            output.write(json.dumps(dict(at=time.time_ns()//1_000_000,kind='locator-click',phase=phase,sequence=sequence))+'\n')
    record('start')
    try:
        return _locator_click(locator, *args, **kwargs)
    finally:
        record('end')


def executable():
    data = json.loads(subprocess.check_output(['cargo', 'metadata', '--no-deps', '--format-version', '1'], cwd=ROOT))
    return Path(data['target_directory']) / 'debug/arcnix.exe'


def preview_run_directory(out):
    path = out.resolve()
    for root in (ROOT / 'artifacts/manual-preview', ROOT / 'artifacts/panel-preview'):
        if root in path.parents:
            return root / path.relative_to(root).parts[0]
    return None


def eventually(probe, timeout=15):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            value = probe()
            if value:
                return value
        except (OSError, ValueError, AssertionError):
            pass
        time.sleep(.05)
    raise AssertionError('Timed out waiting for the isolated test condition')


def expect_radio_options(radios, labels):
    expect(radios).to_have_count(len(labels))
    for index, label in enumerate(labels):
        expect(radios.nth(index)).to_have_accessible_name(label)


def set_desktop_mode(panel, mode):
    panel.get_by_role('button', name='Appearance', exact=True).click()
    scheme = panel.get_by_role('radiogroup', name='Color scheme', exact=True)
    scheme.get_by_role('radio', name=mode, exact=True).check()
    expect(scheme.get_by_role('radio', checked=True)).to_have_accessible_name(mode)


class Peer:
    def __init__(self, out, io_delay=0, browser_port=0, browser_web=None):
        self.out = out
        self.io_delay = io_delay
        self.browser_port = browser_port
        if browser_web is None:
            browser_web = preview_run_directory(out) is not None
        self.browser_web = browser_web
    def __enter__(self):
        self.out.mkdir(parents=True, exist_ok=True)
        ready = self.out / f'peer-{time.time_ns()}.json'
        self.log = (self.out / 'peer.log').open('w')
        command = [sys.executable, str(ROOT / 'tools/sftp_fixture.py'), '--ready', str(ready),
                   '--io-delay', str(self.io_delay), '--browser-port', str(self.browser_port)]
        if self.browser_web:
            command.append('--browser-web')
        self.process = subprocess.Popen(command, cwd=ROOT, stdout=self.log, stderr=self.log)
        self.data = eventually(lambda: json.loads(ready.read_text()))
        self.root = Path(self.data['root'])
        return self
    def profile(self, name, **values):
        host, port = self.data['address'].rsplit(':', 1)
        assert host == '127.0.0.1'
        return dict(id=name, name=name, host=host, port=int(port), username='arcnix', auth='PrivateKey', private_key=self.data['client_key_ed25519'], initial_remote_path='/', **values)
    def config(self, profiles, local, **settings):
        host, port = self.data['address'].rsplit(':', 1)
        return dict(version=1, config=dict(servers=profiles, groups=[], trusted_hosts=[dict(host=host, port=int(port), fingerprint=self.data['host_key_rsa_fingerprint'])], local_directory=str(local), settings=dict(language='English', theme='Light', show_hidden_files=True, resource_interval_seconds=3, **settings)))
    def __exit__(self, *args):
        self.process.terminate(); self.process.wait(timeout=10); self.log.close()


class Desktop:
    def __init__(self, out, config, binary=None, extra_env=None, runtime=None, *, viewport=(1440, 980)):
        self.out = out; self.out.mkdir(parents=True, exist_ok=True)
        self.config = self.out/'config.json'
        if config is not None:
            self.config.write_text(json.dumps(config), encoding='utf-8')
        self.binary = binary or executable(); self.extra_env = extra_env or {}
        self.preview_run = preview_run_directory(out)
        if self.preview_run and not Path(self.binary).resolve().is_relative_to(self.preview_run):
            # Keep the preview executable and sidecar together for the lifetime of the run.
            source = Path(self.binary)
            directory = Path(tempfile.mkdtemp(prefix='runtime-', dir=self.out))
            self.binary = Path(shutil.copy2(source, directory / source.name))
            sidecar = source.with_name('libmpv-2.dll')
            if sidecar.is_file():
                shutil.copy2(sidecar, directory / sidecar.name)
        self.shared_runtime = runtime
        self.viewport = viewport
    def __enter__(self):
        self.runtime = self.shared_runtime or sync_playwright().start()
        self.log = (self.out/'desktop.log').open('a', encoding='utf-8')
        self.launch()
        return self
    def launch(self):
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
        endpoint = f'http://127.0.0.1:{port}'
        env = os.environ.copy(); env.update(self.extra_env)
        # A process-wide override also redirects the SSH browser into the UI profile.
        if self.preview_run or self.extra_env.get('WEBVIEW2_USER_DATA_FOLDER') == '':
            env.pop('WEBVIEW2_USER_DATA_FOLDER', None)
        elif 'WEBVIEW2_USER_DATA_FOLDER' not in self.extra_env:
            env['WEBVIEW2_USER_DATA_FOLDER'] = str(self.out/f'webview-{time.time_ns()}')
        env['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'] = f'--remote-debugging-port={port} --remote-debugging-address=127.0.0.1'
        self.process = subprocess.Popen([str(self.binary), '--config', str(self.config)], cwd=ROOT, env=env, stdout=self.log, stderr=self.log)
        eventually(lambda: urllib.request.urlopen(endpoint+'/json/version', timeout=1).close() or True, timeout=20)
        self.browser = self.runtime.chromium.connect_over_cdp(endpoint)
        self.page = self.browser.contexts[0].pages[0]; self.page.set_default_timeout(10000)
        if self.viewport is not None:
            width, height = self.viewport
            self.page.set_viewport_size(dict(width=width, height=height))
        expect(self.page.locator('.navigation-rail')).to_be_visible()
        if os.environ.get('ARCNIX_TEST_TRACE_INPUT') == '1':
            Locator.click = _traced_locator_click
            self.page._arcnix_click_trace = self.out/f'actions-{time.time_ns()}.jsonl'
            self.page._arcnix_click_sequence = 0
            trace = self.out/f'input-{time.time_ns()}.jsonl'
            def record_input(event):
                with trace.open('a',encoding='utf-8') as output:
                    output.write(json.dumps(event,ensure_ascii=False)+'\n')
            self.page.expose_function('__arcnixTraceInput',record_input)
            self.page.evaluate("""() => {
              const write = value => window.__arcnixTraceInput({at:Date.now(),...value});
              let inputSequence=0, lastInput=null;
              for (const kind of ['pointerdown','pointerup','click','focusin','keydown']) {
                document.addEventListener(kind,event => {
                  const target=event.target.closest?.('button');
                  if(!target || kind==='keydown' && !['Enter','Escape','Tab',' '].includes(event.key))return;
                  const input={kind,sequence:++inputSequence,classes:target.className,name:target.getAttribute('aria-label'),
                    trusted:event.isTrusted,x:event.clientX,y:event.clientY,detail:event.detail,
                    pointer:event.pointerType,key:kind==='keydown'?event.key:undefined,
                    focused:document.hasFocus(),visible:document.visibilityState};
                  if(kind==='click'||kind==='keydown')lastInput=input;
                  write(input);
                },true);
              }
              const transport=window.chrome?.webview;
              if(transport){
                const original=transport.postMessage.bind(transport);
                const wrapped=message => {
                  try {
                    const payload=JSON.parse(message);
                    if(payload.type==='event' && payload.event.kind==='activate')
                      write({kind:'ipc-activate',node:payload.node,input:lastInput?.sequence});
                  }catch{}
                  return original(message);
                };
                transport.postMessage=wrapped;
                write({kind:'ipc-trace',enabled:transport.postMessage===wrapped});
              }
              let previous=null;
              new MutationObserver(() => {
                const active=[...document.querySelectorAll('.navigation-rail .active')].map(x=>x.className);
                const current=JSON.stringify(active);
                if(previous!==current){previous=current;write({kind:'navigation',active});}
              })
                .observe(document.querySelector('.navigation-rail'),{attributes:true,attributeFilter:['class'],subtree:true});
            }""")
    def connect(self, index=0):
        self.page.locator('.host-select').nth(index).click()
        self.workspace('Home')
        self.page.locator('.overview-transfer').click()
        # Fixture profiles use an unencrypted key, which connects without a credential dialog.
        expect(self.page.locator('.connection-state')).to_have_text('SSH connected')
        expect(self.page.get_by_role('dialog', name='Connect to server', exact=True)).to_have_count(0)
    def connect_media_preview(self):
        self.page.locator('.host-select').click()
        self.page.locator('.overview-transfer').click()
        connect = self.page.get_by_role('dialog').get_by_role('button', name='连接', exact=True)
        connected = self.page.locator('.connection-state').filter(has_text='SSH 已连接')
        expect(connect.or_(connected).first).to_be_visible()
        if connect.is_visible():
            connect.click()
        expect(connected).to_be_visible()
    def workspace(self, label):
        tabs = self.page.locator('.workbench-tabs')
        expect(tabs).to_be_visible()
        button = tabs.get_by_role('button', name=label, exact=True)
        if button.count():
            if button.get_attribute('aria-pressed') != 'true':
                button.click()
        else:
            tabs.get_by_role('button', name='Home', exact=True).click()
            self.page.locator('.overview-connect' if label == 'Desktop' else '.overview-transfer').click()
    def application(self, label):
        self.workspace('Desktop')
        launcher = self.page.locator('.desktop-dock').get_by_role('button', name=label, exact=True)
        if not launcher.count():
            launcher = self.apps().get_by_role('button', name=label, exact=True)
        elif launcher.get_attribute('aria-pressed') == 'true':
            overview = self.page.locator('.window-overview')
            if overview.count():
                overview.locator('.overview-exit').click()
            return
        launcher.click()
    def apps(self):
        self.workspace('Desktop')
        self.page.locator('.desktop-dock').get_by_role('button', name='Launcher', exact=True).click()
        return self.page.get_by_role('dialog', name='Launcher', exact=True)
    def show_desktop(self):
        self.page.locator('.desktop-dock').get_by_role('button', name='Launcher', exact=True).click(button='right')
        menu = self.page.locator('.desktop-context-menu')
        menu.get_by_role('button', name='Show desktop', exact=True).or_(
            menu.get_by_role('button', name='Restore windows', exact=True)).click()
    def code(self):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            self.page.wait_for_timeout(100)
            for context in self.browser.contexts:
                for page in context.pages:
                    if page.url.startswith('http://arcnixcode.localhost/'):
                        page.set_default_timeout(10000)
                        return page
        raise AssertionError('Code WebView did not open')

    def replace_code(self, value):
        page = self.code()
        area = page.locator('.monaco-editor textarea.inputarea').first
        area.focus()
        area.press('Control+a')
        page.keyboard.insert_text(value)
        return area

    def settings(self, section):
        self.page.locator('.settings-link').click()
        self.page.locator('.context-links').get_by_role('button', name=section, exact=True).click()
    def servers(self):
        self.page.locator('.rail-servers').click()
    def file(self, name, remote=True):
        return self.page.locator('.remote-slot:visible' if remote else '.local-slot:visible').get_by_role('button', name=name, exact=True)
    def action(self, name):
        self.page.locator('.file-context-menu:visible').get_by_role('button', name=name, exact=True).click()
    def open_in(self, destination):
        menu = self.page.locator('.file-context-menu:visible')
        menu.get_by_role('button', name='Open in', exact=True).hover()
        menu.locator('.file-open-submenu').get_by_role('button', name=destination, exact=True).click()
    def screenshot(self, name):
        self.page.screenshot(path=str(self.out/name))
    def stop(self):
        try: self.browser.close()
        except Exception: pass
        if self.process.poll() is None:
            self.process.terminate(); self.process.wait(timeout=10)
    def __exit__(self, exc, *args):
        if exc:
            try:
                self.screenshot('failure.png')
                (self.out/'failure.txt').write_text(self.page.locator('body').inner_text(), encoding='utf-8')
            except Exception: pass
        self.stop()
        if self.shared_runtime is None:
            self.runtime.stop()
        self.log.close()


def close_dialog(dialog):
    dialog.get_by_role('button', name='Close dialog', exact=True).click()
    expect(dialog).not_to_be_visible()
