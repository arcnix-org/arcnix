"""Fixed application replies for the owned loopback SSH server; never runs shell."""
import json
import os
import shlex
import time
import base64
import re
import hashlib
import tempfile
import threading
from pathlib import Path

def apply_task(server,record):
    script=record.get('application_script','')
    if not script:return
    root=server.fixture_root
    if '# arcnix:redis:' in script and (root / 'redis-enabled').exists():
        from redis_fixture import cluster_for
        cluster = cluster_for(root)
        target = 'docker' if 'docker ' in script else 'host'
        if '# arcnix:redis:stop' in script:
            cluster.set_running(target, False)
        elif '# arcnix:redis:start' in script or '# arcnix:redis:restart' in script:
            cluster.set_running(target, True)
        return
    decoded=[]
    for value in re.findall(r'[A-Za-z0-9+/]{40,}={0,2}',script):
        try:decoded.append(base64.b64decode(value).decode())
        except (ValueError,UnicodeError):pass
    if '# arcnix:compose:install' in script:
        manifest=next(json.loads(v) for v in decoded if v.startswith('{') and '"kind"' in v and '"image"' in v)
        yaml=next(v for v in decoded if v.startswith('services:\n'))
        directory=root/'home'/record['application_user']/'.arcnix/apps'/manifest['name']
        directory.mkdir(parents=True,exist_ok=True)
        (directory/'arcnix.json').write_text(json.dumps(manifest))
        (directory/'compose.yaml').write_text(yaml)
        if manifest['kind']=='nginx':
            (directory/'html').mkdir(exist_ok=True);(directory/'html/index.html').write_text('Nginx is ready\n')
        seed=root/'docker-fixture.json'
        if seed.exists():
            data=json.loads(seed.read_text());name='arcnix-'+manifest['name']+'-application-1';identifier=hashlib.sha256(name.encode()).hexdigest()
            row=dict(id=identifier,name=name,image=manifest['image'],state='running',status='Up 1 second',ports=f"{manifest['address']}:{manifest['port']}->80/tcp",mounts='',networks='bridge',compose_project='arcnix-'+manifest['name'],compose_service='application',compose_working_dir=f"/home/{record['application_user']}/.arcnix/apps/{manifest['name']}",details=dict(restart='unless-stopped',retries=0,memory=0,nano_cpus=0,network='bridge',ports={},bindings={},mounts=[]))
            data['containers']=[v for v in data['containers'] if v['id']!=identifier]+[row];seed.write_text(json.dumps(data))
    if '# arcnix:startup:install' in script:
        unit=next(v for v in decoded if v.startswith('[Unit]\n'))
        name=re.search(r'Description=Arcnix startup ([a-z0-9-]+)',unit).group(1)
        server.services['arcnix-startup-'+name+'.service']='inactive'
        state=root/'services-startup.json';values=json.loads(state.read_text()) if state.exists() else {}
        values['arcnix-startup-'+name+'.service']='enabled';state.write_text(json.dumps(values))
    if '# arcnix:compose:manage' in script and ' down --timeout 10' in script:
        name = re.search(r'\.removed-([a-z0-9-]+)\.XXXXXX', script).group(1)
        projects = root/'home'/record['application_user']/'.arcnix/apps'
        archive = Path(tempfile.mkdtemp(prefix=f'.removed-{name}.', dir=projects))
        (projects/name).rename(archive/'project')
        seed = root/'docker-fixture.json'
        data = json.loads(seed.read_text())
        data['containers'] = [row for row in data['containers'] if row.get('compose_project') != 'arcnix-'+name]
        seed.write_text(json.dumps(data))
    marker=re.search(r'# arcnix:software:definition ([A-Za-z0-9+/=]+)',script)
    if marker:
        definition=json.loads(base64.b64decode(marker.group(1)))
        state=root/'software-fixture.json';values=json.loads(state.read_text()) if state.exists() else dict(native=[dict(name='curl',version='8.5',summary='Transfer tool')],snap=[])
        source='snap' if definition['source']=='Snap' else 'native'
        for name in definition['names']:
            values[source]=[v for v in values[source] if v['name']!=name]
            if definition['action']!='Remove':values[source].append(dict(name=name,version='1.0-fixture',summary=definition['source']))
            if name == 'nginx' and definition['action'] == 'Remove' and not (root/'software-manager').exists():
                type(server).nginx_installed = False
            if name in ('docker', 'docker.io', 'docker-ce', 'moby-engine'):
                if definition['action'] == 'Install':
                    seed = root/'docker-fixture.json'
                    if not seed.exists():
                        seed.write_text(json.dumps(dict(version='28.5.1',containers=[],volumes=[])))
                    (root/'docker-cli-removed').unlink(missing_ok=True)
                if definition['action'] == 'Remove':
                    (root/'docker-cli-removed').touch()
        state.write_text(json.dumps(values))


_sync_lock = threading.Lock()
_desktop_header = b'arcnix-desktop-v1\n'


def desktop_settings_dir(legacy):
    # A regular pointer models the production symlink on Windows hosts that
    # cannot create symbolic links without Developer Mode.
    try:
        revision = legacy.with_name('.desktop.fixture-current').read_text(encoding='ascii')
    except FileNotFoundError:
        live = legacy.with_name('desktop')
        return live if live.is_dir() else None
    return legacy.parent / 'desktop-revisions' / revision


def read_desktop_settings(legacy):
    directory = desktop_settings_dir(legacy)
    if directory is None:
        return json.loads(legacy.read_bytes()) if legacy.exists() else {}
    def load(path):
        return json.loads(path.read_bytes()) if path.exists() else {}
    prefs = load(directory / 'system.json')
    appearance = load(directory / 'preferences/desktop.json')
    appearance['dock'] = load(directory / 'apps/dock.json')
    prefs['appearance'] = appearance
    windows = {}
    for path in (directory / 'apps').glob('*.json'):
        if path.stem == 'dock':
            continue
        value = load(path)
        if 'window' in value:
            name = {'web_browser': 'browser', 'text_editor': 'editor'}.get(path.stem, path.stem)
            windows[name] = value['window']
        if path.stem in ('files', 'terminal'):
            prefs[path.stem] = value.get('settings', {})
        elif path.stem == 'web_browser':
            prefs['browser_bookmarks'] = value.get('settings', {}).get('bookmarks', [])
        elif path.stem == 'monitor':
            interval = value.get('settings', {}).get('resource_interval_seconds')
            if interval is not None:
                prefs['resource_interval_seconds'] = interval
    prefs['windows'] = windows
    return prefs


def desktop_mode(preferences):
    appearance = preferences.get('appearance', {})
    if 'theme' in appearance:
        return appearance.get('overrides', {}).get('mode', 'System')
    themes = appearance.get('themes', {})
    selection = themes.get('selection', {})
    value = selection.get('value')
    kind = selection.get('kind')
    if kind == 'preset':
        return 'System'
    if kind in ('saved', 'custom'):
        custom = themes['saved'][value] if kind == 'saved' else value
        return custom.get('overrides', {}).get('mode', 'System')
    return preferences.get('theme', 'System')


def desktop_packet(legacy, paths, update=None):
    directory = desktop_settings_dir(legacy)
    if directory is None:
        return legacy.read_bytes() if legacy.exists() else None
    packet = bytearray(_desktop_header)
    for path in paths:
        file = directory / path
        if update and path == update[0]:
            data = update[1]
        else:
            data = file.read_bytes() if file.exists() else b''
        packet.extend(f'{len(data)}\n'.encode('ascii'))
        packet.extend(data)
    return bytes(packet)


def publish_desktop(legacy, paths, data):
    assert data.startswith(_desktop_header)
    revisions = legacy.parent / 'desktop-revisions'
    revisions.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix='generation.', dir=revisions))
    offset = len(_desktop_header)
    for path in paths:
        end = data.index(b'\n', offset)
        size = int(data[offset:end])
        offset = end + 1
        target = directory / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data[offset:offset + size])
        offset += size
    assert offset == len(data)
    temporary = legacy.with_name('.desktop.fixture-next')
    temporary.write_text(directory.name, encoding='ascii')
    os.replace(temporary, legacy.with_name('.desktop.fixture-current'))


def reply(server, channel, command):
    marker = command.splitlines()[0]
    if not marker.startswith(('# arcnix:browser:repair ', '# arcnix:sync:', '# arcnix:apps:', '# arcnix:desktop:', '# arcnix:docker:', '# arcnix:disks:', '# arcnix:compose:', '# arcnix:software:', '# arcnix:network:', '# arcnix:processes:', '# arcnix:redis:')):
        return False
    root = server.fixture_root
    with (root / 'server-app-queries.jsonl').open('a', encoding='utf-8') as log:
        log.write(json.dumps(dict(user=server.username, command=command)) + '\n')

    def send(value='', status=0, error=''):
        if value:
            channel.sendall(value.encode() if isinstance(value, str) else value)
        if error:
            channel.sendall_stderr(error.encode())
        channel.send_exit_status(status)
        channel.close()
        return True

    if marker.startswith('# arcnix:browser:repair '):
        if (root / 'browser-forwarding-admin-required').exists():
            if "exec /usr/bin/sudo -S -p '' --" not in command:
                return send(status=1, error='Administrator access is required to change SSH forwarding.')
            supplied = bytearray()
            while not supplied.endswith(b'\n') and len(supplied) <= 4096:
                fragment = channel.recv(4096)
                if not fragment:
                    break
                supplied.extend(fragment)
            if supplied != b'fixture-admin-secret\n':
                return send(status=1, error='Administrator authentication failed.')
            assert 'fixture-admin-secret' not in command
        assert marker == '# arcnix:browser:repair account'
        allowed = root / 'browser-forwarding-repaired.json'
        with _sync_lock:
            accounts = json.loads(allowed.read_text()) if allowed.exists() else {}
            accounts[server.username] = True
            temporary = allowed.with_suffix('.stage')
            temporary.write_text(json.dumps(accounts))
            os.replace(temporary, allowed)
        return send('ARCNIX_BROWSER_FORWARDING_READY_V1\n')

    seed = root / 'docker-fixture.json'
    if marker in ('# arcnix:desktop:restart', '# arcnix:desktop:shutdown'):
        return send()
    if marker == '# arcnix:desktop:device-info':
        return send('os\tFixture Linux\nversion\t42\narch\taarch64\nkernel\t6.12\n'
                    'cpu\tFixture CPU\ncores\t4\nlogical\t8\nmemory\t8589934592\n')
    if marker == '# arcnix:processes:read':
        if (root / 'processes-unavailable').exists():
            return send(status=127, error='ps is unavailable')
        end = 5300 if (root / 'large-process-inventory').exists() else 350
        workers = ''.join(f'{pid} 1000 0.0 512 fixture-idle-{pid}\n' for pid in range(300, end))
        return send('1 0 0.0 1024 systemd\n200 1000 1.2 6144 fixture-web\n'
                    '202 1000 13.5 4096 fixture-batch worker\n' + workers)
    if marker == '# arcnix:network:read':
        if (root / 'network-unavailable').exists():
            return send(status=127, error='iproute2 is required.')
        interfaces = [
            dict(ifindex=2, ifname='eth0', operstate='UP', addr_info=[
                dict(local='192.0.2.42', prefixlen=24),
                dict(local='2001:db8::42', prefixlen=64),
            ]),
            dict(ifindex=3, ifname='eth1', operstate='DOWN'),
        ]
        return send(
            'ARCNIX_NETWORK_V1\n' + json.dumps(interfaces) + '\nARCNIX_IPV4_ROUTES\n'
            'default via 192.0.2.1 dev eth0 proto dhcp metric 100\nARCNIX_IPV6_ROUTES\n'
            'default via fe80::1 dev eth0 proto ra metric 256\nARCNIX_DNS\n'
            'OK\n# Resolver configuration\nnameserver 192.0.2.53\n'
            'nameserver fe80::53%eth0\n\nARCNIX_NETWORK_END\n'
        )
    if marker == '# arcnix:redis:helper':
        data = bytearray()
        while True:
            chunk = channel.recv(65536)
            if not chunk:
                break
            data.extend(chunk)
        from redis_fixture import handle_helper_request
        status, out, err = handle_helper_request(root, bytes(data))
        return send(out, status=status, error=err)
    if marker == '# arcnix:apps:discover':
        return send('ARCNIX_APPS_V1\n' + ('docker\ncompose\n' if seed.exists() and not (root/'docker-cli-removed').exists() else '') + ('snap\n' if (root/'software-snap-enabled').exists() else '') + ('tmux\n' if (root/'tmux-enabled').exists() else '') + ('redis\n' if (root/'redis-enabled').exists() else ''))
    locale = root / 'system-locale'
    if marker == '# arcnix:desktop:locale':
        if not locale.exists():
            return send('ARCNIX_LOCALE_V1\n\n')
        return send('ARCNIX_LOCALE_V1\n/etc/default/locale\n' + locale.read_text(encoding='utf-8'))
    if marker == '# arcnix:desktop:locale-list':
        return send('C\nC.utf8\nen_US.utf8\nzh_CN.utf8\n')
    if marker == '# arcnix:desktop:set-locale':
        if (root / 'system-locale-denied').exists():
            return send(status=1, error='Permission denied changing the fixture locale')
        assert locale.exists()
        lang = re.search(r"^lang='([^']+)'$", command, re.M)[1]
        settings = [line for line in locale.read_text(encoding='utf-8').splitlines()
                    if not line.startswith(('LANG=', 'LC_MESSAGES=', 'LANGUAGE='))]
        locale.write_text('\n'.join([f'LANG={lang}', *settings, '']), encoding='utf-8')
        return send('')
    time_zone = root / 'system-time-zone'
    if marker == '# arcnix:desktop:time-zone':
        system = time_zone.exists()
        current = time_zone.read_text(encoding='utf-8').strip() if system else ''
        extra = root / 'time-zone-choices.txt'
        return send(f"ARCNIX_TIME_ZONE_V1\n{'yes' if system else 'no'}\n{current}\n"
                    'America/Phoenix\tUS\nAsia/Shanghai\tCN\nEtc/UTC\n'
                    + (extra.read_text(encoding='utf-8') if extra.exists() else ''))
    if marker == '# arcnix:desktop:set-time-zone':
        if (root / 'system-time-zone-denied').exists():
            return send(status=1, error='Permission denied changing the fixture time zone')
        assert time_zone.exists()
        zone = re.search(r"^zone='([^']+)'$", command, re.M)[1]
        offsets = {'America/Phoenix': '-0700', 'Asia/Shanghai': '+0800', 'Etc/UTC': '+0000'}
        abbreviations = {'America/Phoenix': 'MST', 'Asia/Shanghai': 'CST', 'Etc/UTC': 'UTC'}
        time_zone.write_text(zone + '\n', encoding='utf-8')
        clock_file = root / 'desktop-clock.json'
        clock = json.loads(clock_file.read_text(encoding='utf-8')) if clock_file.exists() else {}
        clock_file.write_text(json.dumps(dict(clock, offset=offsets[zone], zone=abbreviations[zone])), encoding='utf-8')
        return send('')
    preferences = root / 'home' / server.username / '.arcnix' / 'desktop.json'
    if marker.startswith('# arcnix:desktop:domains:'):
        runner = shlex.split(command.split('\n', 1)[1])
        assert runner[:-1] == ['exec', '/bin/sh', '-c']
        command = runner[-1]
        paths = re.search(r"^domains='([^']+)'$", command, re.M)[1].split()
        if (root / 'sync-offline').exists():
            return send(status=1, error='Fixture sync interrupted')
        if marker.endswith(':read'):
            with _sync_lock:
                current = desktop_packet(preferences, paths)
            return send(b'M\n' if current is None else b'F\n' + current)
        data = bytearray()
        while chunk := channel.recv(65536):
            data.extend(chunk)
        if (root / 'desktop-write-denied').exists():
            (root / 'desktop-write-denied-attempted').touch()
            return send(status=1, error='Permission denied writing desktop preferences')
        hold = root / 'desktop-write-hold'
        deadline = time.monotonic() + 10
        while hold.exists() and time.monotonic() < deadline:
            time.sleep(.025)
        if marker.endswith(':file'):
            base, expected, path = re.search(
                r"^publish_file '([^']+)' '([^']+)' '([^']+)'$", command, re.M).groups()
            payload_hash = re.search(r"^prepare_file '([^']+)'$", command, re.M)[1]
            assert path in paths
        else:
            base, expected = re.search(r"^publish_commit '([^']+)' '([^']+)'$", command, re.M).groups()
            path = None
            payload_hash = expected
        assert hashlib.sha256(data).hexdigest() == payload_hash
        with _sync_lock:
            snapshot = desktop_packet(preferences, paths)
            current = hashlib.sha256(snapshot).hexdigest() if snapshot is not None else 'missing'
            if current == expected:
                result = 'OK\n'
            elif current != base:
                result = 'STALE\n'
            else:
                if path is None:
                    publish_desktop(preferences, paths, data)
                else:
                    next_packet = desktop_packet(preferences, paths, (path, data))
                    assert hashlib.sha256(next_packet).hexdigest() == expected
                    target = desktop_settings_dir(preferences) / path
                    temporary = target.with_suffix('.stage')
                    temporary.write_bytes(data)
                    os.replace(temporary, target)
                if (root / 'trace-task-queries').exists():
                    with (root/'desktop-writes.jsonl').open('a') as out:
                        out.write(json.dumps(read_desktop_settings(preferences))+'\n')
                result = 'OK\n'
        return send(result)
    if marker.startswith('# arcnix:sync:'):
        client = re.search(r'^child ([0-9a-f]{32})$', command, re.M)
        target = (preferences.parent / 'browser' / 'profiles' / client[1] / 'cookies.enc'
                  if client else preferences)
        if (root / 'sync-offline').exists():
            return send(status=1, error='Fixture sync interrupted')
        if marker == '# arcnix:sync:read':
            with _sync_lock:
                value = b'F\n' + target.read_bytes() if target.exists() else b'M\n'
            return send(value)
        data = bytearray()
        while chunk := channel.recv(65536):
            data.extend(chunk)
        if target == preferences and (root / 'desktop-write-denied').exists():
            (root / 'desktop-write-denied-attempted').touch()
            return send(status=1, error='Permission denied writing desktop preferences')
        expected = re.findall(r"\[ \"\$current\" = '([a-f0-9]{64}|missing)' \]", command)
        digest = hashlib.sha256(data).hexdigest()
        assert len(expected) == 2 and expected[0] == digest
        with _sync_lock:
            current = hashlib.sha256(target.read_bytes()).hexdigest() if target.exists() else 'missing'
            if current == digest:
                result = 'OK\n'
            elif current != expected[1]:
                result = 'STALE\n'
            else:
                if target == preferences and (root / 'trace-task-queries').exists():
                    with (root/'desktop-writes.jsonl').open('a') as out:
                        out.write(json.dumps(json.loads(data))+'\n')
                target.parent.mkdir(parents=True, exist_ok=True)
                temporary = target.with_suffix('.stage')
                temporary.write_bytes(data)
                os.replace(temporary, target)
                result = 'OK\n'
        return send(result)
    if marker == '# arcnix:desktop:clock':
        state = root / 'desktop-clock.json'
        clock = json.loads(state.read_text(encoding='utf-8')) if state.exists() else {}
        return send(f"{clock.get('unix_seconds', int(time.time()))} {clock.get('offset', '+0000')} {clock.get('zone', '')}\n")
    if marker == '# arcnix:desktop:home':
        return send(f'/home/{server.username}')
    if marker == '# arcnix:desktop:wallpapers':
        (preferences.parent / 'wallpapers').mkdir(parents=True, exist_ok=True)
        return send(f'/home/{server.username}/.arcnix/wallpapers')
    for record in list(server.hosted_tasks.values()):
        server.task_finished(record)
    if marker == '# arcnix:disks:read':
        gib = 1024**3
        devices = dict(blockdevices=[
            dict(path='/dev/vda', type='disk', size=64*gib, model='Fixture disk', children=[
                {'path': '/dev/vda1', 'type': 'part', 'size': 56*gib, 'fstype': 'ext4', 'maj:min': '252:1'},
                dict(path='/dev/vda2', type='part', size=8*gib, fstype='swap'),
            ]),
            dict(path='/dev/vdb', type='disk', size=256*gib, model='Data disk', children=[
                dict(path='/dev/vdb1', type='part', size=256*gib, fstype='ext4'),
            ]),
        ])
        mounts = dict(filesystems=[
            {'source': '/dev/root', 'target': '/', 'fstype': 'ext4', 'size': 56*gib, 'used': 16*gib, 'avail': 40*gib, 'maj:min': '252:1'},
            dict(source='/dev/vdb1', target='/data', fstype='ext4', size=256*gib, used=32*gib, avail=224*gib),
            dict(source='proc', target='/proc', fstype='proc'),
        ])
        return send('ARCNIX_DISKS_V1\n'+json.dumps(devices)+'\nARCNIX_FILESYSTEMS\n'+json.dumps(mounts)+'\nARCNIX_DISKS_END\n')
    if marker == '# arcnix:compose:list':
        directory=root/'home'/server.username/'.arcnix/apps'
        lines=['ARCNIX_COMPOSE_V1',base64.b64encode(f'/home/{server.username}/.arcnix/apps'.encode()).decode()]
        for path in sorted(directory.glob('*/arcnix.json')):
            lines += ['ARCNIX_PROJECT',base64.b64encode(path.read_bytes()).decode(),base64.b64encode(path.with_name('compose.yaml').read_bytes()).decode()]
        return send('\n'.join(lines+['ARCNIX_COMPOSE_END'])+'\n')
    if marker == '# arcnix:software:candidates':
        names = shlex.split(command.splitlines()[1])[2:]
        if 'snap info' in command:
            available = {'docker', 'caddy', 'htop', 'certbot'} if (root/'software-snap-enabled').exists() else set()
        elif 'dnf ' in command:
            available = {'moby-engine', 'nginx', 'redis', 'postgresql-server', 'git', 'htop', 'certbot'}
        else:
            available = {'docker.io', 'nginx', 'caddy', 'redis-server', 'postgresql', 'git', 'htop', 'certbot'}
        return send(''.join(name+'\n' for name in names if name in available))
    if marker == '# arcnix:software:query':
        state=root/'software-fixture.json'
        data=json.loads(state.read_text()) if state.exists() else dict(native=[dict(name='curl',version='8.5',summary='Transfer tool')],snap=[dict(name='hello-world',version='6.4',summary='latest/stable canonical -')])
        if 'snap ' in command:
            rows=data['snap']
            if 'find ' in command: rows=[dict(name='hello-world',version='6.4',summary='canonical - Hello world')]
            return send('Name Version Rev Tracking Publisher Notes\n'+''.join(f"{v['name']} {v['version']} 1 latest/stable canonical -\n" for v in rows))
        return send(''.join(f"{v['name']}\t{v['version']}\t{v['summary']}\n" for v in data['native']))
    # The logs command merges the CLI's stderr into stdout, so its errors arrive there.
    merged = command.rstrip().endswith(' 2>&1')
    def fail(status, error):
        return send(error, status=status) if merged else send(status=status, error=error)
    if not seed.exists():
        return send(status=127, error='Docker is not installed')
    if (root / 'docker-denied').exists():
        return fail(1, 'permission denied connecting to the Docker socket at unix:///var/run/docker.sock')
    data = json.loads(seed.read_text())
    kind = marker.rsplit(':', 1)[-1]
    if kind == 'snapshot':
        rows = ['ARCNIX_DOCKER_V1', data['version'], 'ARCNIX_CONTAINERS']
        rows += [json.dumps({key: row.get(key, '') for key in ['id', 'name', 'image', 'state', 'status', 'ports', 'mounts', 'networks', 'compose_project', 'compose_service', 'compose_working_dir']}) for row in data['containers']]
        rows += ['ARCNIX_VOLUMES']
        rows += [json.dumps({key: row[key] for key in ['name', 'driver', 'scope']}) for row in data['volumes']]
        rows += ['ARCNIX_DOCKER_END']
        if (root / 'docker-truncated').exists():
            rows.pop()
        return send('\n'.join(rows) + '\n')
    if kind == 'images':
        return send(json.dumps(dict(id='sha256:'+'a'*64, repository='nginx', tag='alpine', size='25 MB'))+'\n')
    arguments = shlex.split(command.splitlines()[-1])
    if merged:
        arguments.pop()
    target = arguments[-1]
    if kind == 'volume':
        volume = next(row for row in data['volumes'] if row['name'] == target)
        return send(json.dumps(volume))
    container = next((row for row in data['containers'] if row['id'] == target), None)
    if not container:
        return fail(1, 'No such container')
    if kind == 'inspect':
        detail = dict(container['details'], id=target, name='/' + container['name'], image=container['image'], state=container['state'])
        return send(json.dumps(detail))
    if kind == 'stats':
        return send(json.dumps(dict(cpu='0.12%', memory='6 MiB / 256 MiB', network='3 kB / 6 kB', block='0 B / 4 kB', processes='3'))+'\n')
    if kind == 'logs':
        if (root / 'docker-slow').exists():
            time.sleep(1.2)
        stdout, stderr = container.get('logs', ''), container.get('stderr_logs', '')
        tail = int(arguments[arguments.index('--tail') + 1])
        if merged:
            # Docker keeps both streams in one timestamped log, so a shared pipe receives them in time order.
            lines = stdout.splitlines(keepends=True) + stderr.splitlines(keepends=True)
            return send(''.join(sorted(lines, key=lambda line: line.split(' ', 1)[0])[-tail:]))
        return send(''.join(stdout.splitlines(keepends=True)[-tail:]), error=''.join(stderr.splitlines(keepends=True)[-tail:]))
    if (root / 'docker-action-failed').exists():
        return send(status=1, error='Docker fixture operation failed')
    if kind in ['start', 'restart', 'stop', 'pause', 'unpause']:
        container['state'] = {'stop': 'exited', 'pause': 'paused'}.get(kind, 'running')
        container['status'] = {'stop': 'Exited (0)', 'pause': 'Up 1 second (Paused)'}.get(kind, 'Up 1 second')
    elif kind == 'remove':
        if container['state'] not in ('exited', 'created', 'dead'):
            return fail(1, 'Container must be stopped before removal')
        data['containers'].remove(container)
    elif kind == 'configure':
        policy = arguments[arguments.index('--restart') + 1]
        name, _, retries = policy.partition(':')
        container['details'].update(restart=name, retries=int(retries or 0))
        if '--cpus' in arguments:
            container['details']['nano_cpus'] = int(float(arguments[arguments.index('--cpus') + 1]) * 1_000_000_000)
        if '--memory' in arguments:
            container['details']['memory'] = int(arguments[arguments.index('--memory') + 1][:-1]) * 1024 * 1024
    else:
        raise AssertionError('Unexpected Docker fixture action ' + kind)
    seed.write_text(json.dumps(data))
    return send(target + '\n')


def windows_desktop_reply(server, channel):
    with channel.makefile("rb", -1) as stream:
        length = int.from_bytes(stream.read(4), "little")
        if not 1 <= length <= 16384:
            raise ValueError("Invalid fixture script length")
        script = stream.read(length).decode("utf-8")
        if "[IO.File]::Replace" not in script or "desktop.v1" not in script:
            raise ValueError("Unknown desktop sync provider")
        maximum = int(re.search(r"(?m)^\$limit=(\d+)$", script)[1])
        target = server.fixture_root / "windows-desktop.v1"
        with server.hosted_tasks_lock:
            current = target.read_bytes() if target.exists() else None
            if "$commit=$false" in script:
                channel.sendall(b"M\n" if current is None else b"F\n" + current)
                return
            size = int(re.search(r"(?m)^\$length=(\d+)$", script)[1])
            base = re.search(r"(?m)^\$base='([a-f0-9]+|missing)'$", script)[1]
            next_revision = re.search(r"(?m)^\$next='([a-f0-9]+)'$", script)[1]
            if not 1 <= size <= maximum:
                raise ValueError("Invalid fixture data length")
            data = stream.read(size)
            if stream.read(1) or hashlib.sha256(data).hexdigest() != next_revision:
                raise ValueError("Invalid fixture data")
            revision = "missing" if current is None else hashlib.sha256(current).hexdigest()
            if next_revision == revision:
                channel.sendall(b"OK\n")
            elif base != revision:
                channel.sendall(b"STALE\n")
            else:
                target.write_bytes(data)
                channel.sendall(b"OK\n")


def windows_docker_reply(server, channel, script):
    operation = script.splitlines()[0].rsplit(":", 1)[1]
    identifier = "a" * 64
    container = dict(id=identifier, name="fixture", image="nginx:alpine",
                     state=getattr(server, "windows_container_state", "exited"),
                     status="Fixture", ports="", mounts="", networks="")
    volume = dict(name="fixture_data", driver="local", scope="local")
    if "npipe:////./pipe/docker_engine " not in script:
        raise ValueError("Unexpected Docker endpoint")
    if operation == "snapshot":
        lines = ["ARCNIX_DOCKER_V1", "29.0.0", "ARCNIX_CONTAINERS",
                 json.dumps(container), "ARCNIX_VOLUMES", json.dumps(volume), "ARCNIX_DOCKER_END"]
    elif operation == "images":
        lines = [json.dumps(dict(id="sha256:" + identifier, repository="nginx", tag="alpine", size="25 MB"))]
    elif operation == "volume" and volume["name"] in script:
        lines = [json.dumps(dict(volume, mountpoint=r"C:\ProgramData\docker\volumes\fixture_data\_data"))]
    elif identifier not in script:
        raise ValueError("Unexpected Docker target")
    elif operation == "inspect":
        lines = [json.dumps(dict(id=identifier, name="/fixture", image=container["image"], state=container["state"],
                                 restart="no", retries=0, memory=0, nano_cpus=0, network="nat",
                                 ports={}, bindings={}, mounts=[]))]
    elif operation == "stats":
        lines = [json.dumps(dict(cpu="0.1%", memory="1 MiB", network="0 B", block="0 B", processes="2"))]
    elif operation == "logs":
        lines = ["fixture stdout", "fixture stderr"]
    elif operation == "start":
        server.windows_container_state = "running"
        lines = [identifier]
    else:
        raise ValueError("Unexpected Docker operation")
    channel.sendall(("\r\n".join(lines) + "\r\n").encode())


def windows_task_reply(server, channel):
    with channel.makefile("rb", -1) as stream:
        script = stream.read(32769).decode("utf-8")
    if len(script) > 32768 or "Register-ScheduledTask" not in script or "-LogonType S4U" not in script:
        raise ValueError("Invalid native task script")

    def value(name):
        return re.search(r"(?m)^\$" + name + r"='([^']*)'$", script)[1]

    def literal(name):
        encoded = re.search(r"(?m)^\$" + name + r"=.*FromBase64String\('([A-Za-z0-9+/=]*)'\)", script)[1]
        return base64.b64decode(encoded).decode("utf-8")

    op, task = value("op"), value("id")
    expected_root = literal("expectedRoot")
    root = r"C:\Users\fixture\AppData\Local\Arcnix\native-tasks-v1"
    if expected_root and expected_root != root:
        raise ValueError("Server task root changed")
    fixture = server.fixture_root / "windows-native-tasks.json"
    now = int(time.time())
    header = "ARCNIX_NATIVE_TASKS_V1\t" + str(now) + "\t" + base64.b64encode(root.encode()).decode()

    def row(identifier, record):
        return "\t".join([identifier, str(record["created"]), str(len(record["log"])), record["state"]])

    with server.hosted_tasks_lock:
        tasks = json.loads(fixture.read_text()) if fixture.exists() else {}
        if op == "submit":
            if not re.fullmatch(r"verify-[a-f0-9]{32}", task):
                raise ValueError("Invalid native task identifier")
            source, digest = literal("source"), value("digest")
            if task in tasks:
                if (tasks[task]["source"], tasks[task]["digest"]) != (source, digest):
                    raise ValueError("Task ID reused for another request")
            else:
                actual = hashlib.sha256(b"arcnix-windows-test").hexdigest()
                if source == r"C:\fixture\long":
                    state, log = "running", "Starting verification"
                elif source == r"C:\fixture\short":
                    state = "succeeded" if not digest or digest == actual else "failed"
                    log = f"SHA-256: {actual}"
                    if state == "failed":
                        log += "\nThe checksum differs from the expected value."
                else:
                    raise ValueError("Unexpected fixture file path")
                tasks[task] = dict(created=now, source=source, digest=digest, state=state, log=log)
                fixture.write_text(json.dumps(tasks))
            lines = ["OK"]
        elif op == "stop" and task in tasks:
            if tasks[task]["state"] == "running":
                tasks[task]["state"] = "cancelled"
                fixture.write_text(json.dumps(tasks))
            lines = ["OK"]
        elif op == "detail" and task in tasks:
            data = base64.b64encode(tasks[task]["log"].encode()).decode()
            lines = [header, row(task, tasks[task]), data]
        elif op == "list":
            offset = int(re.search(r"(?m)^\$offset=(\d+)$", script)[1])
            selected = list(tasks.items())[offset:offset + 100]
            lines = [header] + [row(key, val) for key, val in selected] + [f"END\t{int(len(tasks) > offset + 100)}"]
        else:
            raise ValueError("Unexpected fixture task command")
    channel.sendall(("\r\n".join(lines) + "\r\n").encode())


def windows_inventory_reply(server, channel, command):
    """Decode only the encoded-command envelope and return synthetic Windows rows."""
    prefix = command.split(" ", 1)[0] + " -NoLogo -NoProfile -NonInteractive -EncodedCommand "
    status = 1
    try:
        if command == "uname -s":
            channel.sendall_stderr(b"uname is unavailable\r\n")
            status = 127
            return
        if not prefix.startswith(("powershell.exe ", "pwsh.exe ")) or not command.startswith(prefix) or len(command) >= 8191:
            channel.sendall_stderr(b"Unsupported fixture command\r\n")
            return
        script = base64.b64decode(command[len(prefix):], validate=True).decode("utf-16-le")
        if "[Environment]::OSVersion.Platform" in script:
            channel.sendall(b"ARCNIX_WINDOWS_HOST_V1\r\n")
            status = 0
            return
        if script.startswith("# ARCNIX_NATIVE_TASKS_V1"):
            windows_task_reply(server, channel)
            status = 0
            return
        if script.startswith("# ARCNIX_WINDOWS_DESKTOP_SYNC_V1"):
            windows_desktop_reply(server, channel)
            status = 0
            return
        if script.startswith("# arcnix:docker:"):
            windows_docker_reply(server, channel, script)
            status = 0
            return
        service_name = "FixtureService"
        service = dict(unit=service_name, display="Fixture Windows service",
                       state=server.windows_service_state, startup="Manual", pid=42, exit=0, can_stop=True)
        zones = ["UTC", "China Standard Time"]
        if "Set-TimeZone -Id" in script:
            target = re.search(r"FromBase64String\('([A-Za-z0-9+/=]+)'\)", script)
            zone = base64.b64decode(target[1]).decode() if target else ""
            if zone not in zones:
                channel.sendall_stderr(b"Invalid time zone in fixture request\r\n")
                return
            server.windows_time_zone = zone
            result = zone
        elif "Get-Command winget.exe" in script:
            result = dict(distribution="Windows 11", version="10.0.26100", architecture="64-bit", available=True, ps7=True)
        elif prefix.startswith("pwsh.exe ") and "Get-Module -ListAvailable Microsoft.WinGet.Client" in script:
            result = True
        elif prefix.startswith("pwsh.exe ") and "Get-WinGetPackage -Source winget" in script:
            result = [dict(name="Git.Git", version="2.52.0", summary="Git", installed=True)]
            if "Find-WinGetPackage -Query" in script:
                result.append(dict(name="BurntSushi.ripgrep.MSVC", version="15.1.0", summary="ripgrep", installed=False))
        elif "Get-Command docker.exe" in script:
            result = True
        elif "Get-TimeZone -ListAvailable" in script:
            result = dict(name=server.windows_time_zone, choices=zones)
        elif "ToUnixTimeSeconds" in script and "Offset.TotalMinutes" in script:
            result = dict(seconds=1768046400, offset_minutes=480 if server.windows_time_zone == zones[1] else 0)
        elif "Get-Partition" in script and "Win32_LogicalDisk" in script:
            efi = dict(number=1, size=100, kind="System", paths=[],
                       volume=dict(name=None, kind="FAT32", size=100, free=60))
            # The script only sends drive letters; the volume path checks that the parser drops others too.
            system = dict(number=2, size=2000, kind="Basic", paths=["C:\\", "\\\\?\\Volume{fixture}\\"],
                          volume=dict(name="System", kind="NTFS", size=2000, free=1500))
            # Win32_LogicalDisk also reports C:, which the parser must not list twice.
            result = dict(devices=[dict(number=0, model="Fixture disk", size=2100, partitions=[efi, system])],
                          other=[dict(target="C:\\", name="System", kind="NTFS", size=2000, free=1500),
                                 dict(target="E:\\", name="USB", kind="exFAT", size=512, free=256)])
        elif "Get-NetIPInterface" in script and "Get-NetIPAddress" in script:
            result = dict(interfaces=[dict(ifindex=1, ifname="Ethernet", operstate="Connected",
                                          addr_info=[dict(local="192.0.2.8", prefixlen=24)])],
                          routes=[dict(ipv6=False, next="192.0.2.1", interface="Ethernet", metric=5)],
                          dns=["192.0.2.2"])
        elif "Win32_PerfFormattedData_PerfProc_Process" in script:
            result = [dict(pid=42, program="fixture", cpu_percent=12, resident_bytes=1000)]
        elif "Get-NetTCPConnection" in script and "Get-NetUDPEndpoint" in script:
            result = dict(rows=[dict(protocol="tcp", address="::", port=8443, pid=42, program="fixture")],
                          warnings=["Fixture UDP access is unavailable."])
        elif "Get-WinEvent -ListLog" in script:
            result = dict(sources=["System", "Application"], warnings=[])
        elif "Get-WinEvent -FilterHashtable" in script:
            result = [dict(timestamp="2026-01-10T12:00:00Z", provider="FixtureProvider",
                           level=2, message="Fixture event")]
        elif "Win32_Service" in script:
            result = service if "$_.Name -ceq $name" in script else [service]
        elif "System.ServiceProcess.ServiceController" in script and "Start-Service -InputObject" in script:
            target = re.search(r"FromBase64String\('([A-Za-z0-9+/=]+)'\)", script)
            if not target or base64.b64decode(target[1]).decode() != service_name:
                channel.sendall_stderr(b"Invalid service in fixture request\r\n")
                return
            server.windows_service_state = "Running"
            result = True
        else:
            channel.sendall_stderr(b"Unsupported Windows fixture request\r\n")
            return
        channel.sendall(("ARCNIX_HOST_JSON_V1\r\n" + json.dumps(result) + "\r\n").encode())
        status = 0
    except (UnicodeError, ValueError):
        channel.sendall_stderr(b"Invalid Windows fixture request\r\n")
    finally:
        channel.send_exit_status(status)
        channel.close()
