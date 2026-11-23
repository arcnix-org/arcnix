# Start pgAdmin's desktop-mode backend for one Arcnix window. Runs inside the
# managed component with its own data folder. The launch request arrives on
# stdin so the window key never appears in arguments or the parent environment.
import json
import os
import runpy
import sqlite3
import subprocess
import sys
import threading

GROUP = 'Arcnix'


def configuration(data):
    paths = {
        'DATA_DIR': data,
        'LOG_FILE': os.path.join(data, 'pgadmin4.log'),
        'SQLITE_PATH': os.path.join(data, 'pgadmin4.db'),
        'SESSION_DB_PATH': os.path.join(data, 'sessions'),
        'STORAGE_DIR': os.path.join(data, 'storage'),
        'AZURE_CREDENTIAL_CACHE_DIR': os.path.join(data, 'azure'),
        'KERBEROS_CCACHE_DIR': os.path.join(data, 'kerberos'),
    }
    settings = {
        **paths,
        'SERVER_MODE': False,
        'MASTER_PASSWORD_REQUIRED': False,
        # Passwords stay in pgAdmin's memory until this window closes.
        'USE_OS_SECRET_STORAGE': False,
        'ALLOW_SAVE_PASSWORD': False,
        'ALLOW_SAVE_TUNNEL_PASSWORD': False,
        # Arcnix chooses the tested version.
        'UPGRADE_CHECK_ENABLED': False,
    }
    path = os.path.join(data, 'config_distro.py')
    with open(path, 'w', encoding='utf-8') as file:
        for name, value in settings.items():
            file.write(f'{name} = {value!r}\n')
    os.environ['CONFIG_DISTRO_FILE_PATH'] = path
    return paths['SQLITE_PATH']


def update_server(database, server):
    """Point the saved entry at this session's tunnel. True when it exists."""
    if not os.path.exists(database):
        return False
    with sqlite3.connect(database, timeout=10) as connection:
        tables = {row[0] for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table'")}
        if not {'server', 'servergroup'} <= tables:
            return False
        changed = connection.execute(
            'UPDATE server SET host = ?, port = ?, maintenance_db = ?, username = ? '
            'WHERE id = (SELECT server.id FROM server JOIN servergroup '
            'ON servergroup.id = server.servergroup_id '
            'WHERE servergroup.name = ? AND server.name = ? ORDER BY server.id LIMIT 1)',
            ('127.0.0.1', server['port'], server['database'], server['user'],
             GROUP, server['name']))
        return changed.rowcount == 1


def add_server(root, data, server):
    path = os.path.join(data, 'servers.json')
    with open(path, 'w', encoding='utf-8') as file:
        json.dump({'Servers': {'1': {
            'Name': server['name'],
            'Group': GROUP,
            'Host': '127.0.0.1',
            'Port': server['port'],
            'MaintenanceDB': server['database'],
            'Username': server['user'],
            'ConnectionParameters': {'sslmode': 'prefer', 'connect_timeout': 15},
        }}}, file)
    for command in (['setup-db'], ['load-servers', path]):
        subprocess.run([sys.executable, '-s', '-B', os.path.join(root, 'setup.py'), *command],
                       stdin=subprocess.DEVNULL, check=True, cwd=data)


def watch_parent():
    # Arcnix holds the other end open; it closes even if Arcnix crashes.
    while os.read(0, 4096):
        pass
    os._exit(0)


def main():
    os.umask(0o077)
    request = json.loads(sys.stdin.buffer.readline())
    threading.Thread(target=watch_parent, daemon=True).start()
    root, data, server = request['root'], request['data'], request['server']
    database = configuration(data)
    if not update_server(database, server):
        add_server(root, data, server)
        if not update_server(database, server):
            raise SystemExit('pgAdmin did not save the server connection')
    os.environ['PGADMIN_INT_KEY'] = request['key']
    os.environ['PGADMIN_SERVER_MODE'] = 'OFF'
    sys.path.insert(0, root)
    program = os.path.join(root, 'pgAdmin4.py')
    sys.argv = [program]
    from werkzeug.serving import WSGIRequestHandler, make_server

    class RequestHandler(WSGIRequestHandler):
        def log_request(self, code='-', size='-'):
            # The initial WebView URL contains the private desktop key.
            pass

    # Keep the same socket from port selection through the last request.
    # The Socket.IO middleware is part of pgAdmin's initialized WSGI app.
    with make_server('127.0.0.1', 0, None, threaded=True,
                     request_handler=RequestHandler) as backend:
        port = backend.server_port
        os.environ['PGADMIN_INT_PORT'] = str(port)
        backend.app = runpy.run_path(program)['app']
        path = os.path.join(data, 'backend.ready')
        with open(path + '.tmp', 'w', encoding='ascii') as file:
            file.write(f"{request['launch']} {port}")
        os.replace(path + '.tmp', path)
        backend.serve_forever()


main()
