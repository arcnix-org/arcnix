"""Verify server-tree connection indicators with simultaneous loopback SSH sessions."""
import socket
import time

from desktop_fixture import Desktop, Peer, ROOT, expect


def main():
    out = ROOT / 'artifacts/server-connection-tree' / f'run-{time.time_ns()}'
    with Peer(out / 'peer') as peer, socket.socket() as delayed:
        # Hold the SSH banner so the connecting state can be observed deterministically.
        delayed.bind(('127.0.0.1', 0))
        delayed.listen()
        config = peer.config([
            peer.profile('API', group_id='prod'),
            peer.profile('Database', group_id='prod'),
            peer.profile('Idle', group_id='other'),
            peer.profile('Ungrouped'),
            dict(peer.profile('Pending'), port=delayed.getsockname()[1]),
        ], out)
        config['config']['groups'] = [
            dict(id='prod', name='Production'), dict(id='other', name='Other'),
        ]
        with Desktop(out / 'desktop', config) as app:
            page = app.page
            tree = page.locator('.server-tree')
            toggle = page.get_by_role('checkbox', name='Connected only')
            search = page.get_by_role('textbox', name='Search', exact=True)
            folder = tree.locator('.tree-folder').filter(
                has=page.get_by_role('button', name='Production', exact=True))
            badge = folder.locator('.group-connection')

            def server(name):
                return tree.locator('.host-row').filter(has=page.get_by_text(name, exact=True))

            def action(name, command):
                server(name).locator('.host-more').click()
                server(name).get_by_role('button', name=command, exact=True).click()

            def connect(name):
                action(name, 'Connect to server')
                expect(server(name).locator('.host-connection')).to_have_text('Connected')

            toggle.check()
            expect(tree.locator('.tree-no-results')).to_have_text('No matching connected servers')
            toggle.uncheck()
            action('Pending', 'Connect to server')
            expect(server('Pending').locator('.host-connection')).to_have_text('Connecting')
            toggle.check()
            expect(tree.locator('.host-row')).to_have_count(0)
            toggle.uncheck()
            action('Pending', 'Cancel connection')
            expect(server('Pending').locator('.host-connection')).to_be_hidden()

            for name in ('API', 'Database', 'Ungrouped'):
                connect(name)
            expect(badge).to_have_text('2 connected')
            folder.locator('.group-button').click()
            expect(folder.locator('.host-row')).to_have_count(0)
            expect(badge).to_be_visible()
            toggle.check()
            expect(tree.locator('.tree-folder-name')).to_have_text(['Production'])
            expect(tree.locator('.host-name')).to_have_text(['API', 'Database', 'Ungrouped'])
            search.fill('Database')
            expect(tree.locator('.host-name')).to_have_text(['Database'])
            expect(badge).to_have_text('2 connected')
            search.fill('Other')
            expect(tree.locator('.tree-no-results')).to_be_visible()
            search.fill('')
            toggle.uncheck()
            expect(folder.locator('.host-row')).to_have_count(0)
            toggle.check()
            action('API', 'Disconnect')
            expect(badge).to_have_text('1 connected')
            action('Database', 'Disconnect')
            expect(folder).to_have_count(0)
            expect(tree.locator('.host-name')).to_have_text(['Ungrouped'])
            action('Ungrouped', 'Disconnect')
            expect(tree.locator('.tree-no-results')).to_be_visible()
            toggle.uncheck()
            expect(tree.locator('.tree-folder-name')).to_have_text(['Production', 'Other'])


if __name__ == '__main__':
    main()
