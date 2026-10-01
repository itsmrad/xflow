#!/usr/bin/env python3
"""Safety invariants for the private environment and bounded status reader."""
import os
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from unittest.mock import patch

import baseline


class SafetyTests(unittest.TestCase):
    def test_environment_excludes_desktop_and_credentials(self):
        with patch.dict(os.environ, {'DISPLAY': ':0', 'WAYLAND_DISPLAY': 'wayland-0',
                                    'GROQ_API_KEY': 'test-only', 'DBUS_SESSION_BUS_ADDRESS': 'real-bus'}):
            with baseline.isolated() as (env, path):
                self.assertNotIn('DISPLAY', env)
                self.assertNotIn('WAYLAND_DISPLAY', env)
                self.assertNotIn('GROQ_API_KEY', env)
                self.assertNotEqual(env['DBUS_SESSION_BUS_ADDRESS'], 'real-bus')
                self.assertTrue(str(path).startswith(env['XDG_RUNTIME_DIR']))
                config = (Path(env['HOME']).parent / 'bus.conf').read_text()
                self.assertNotIn('servicedir', config)
                self.assertNotIn('include', config)
                self.assertTrue(Path(env['DBUS_SESSION_BUS_ADDRESS'].split('=', 1)[1]).is_socket())
            self.assertFalse(path.parent.parent.exists())

    def assert_invalid_status(self, payload, message):
        with tempfile.TemporaryDirectory(prefix='xflow-reader-test-') as temp:
            path = Path(temp) / 'socket'
            with socket.socket(socket.AF_UNIX) as server:
                server.bind(str(path))
                server.listen(1)

                def reply():
                    with server.accept()[0] as connection:
                        connection.recv(1024)
                        try:
                            connection.sendall(payload)
                        except BrokenPipeError:
                            pass

                thread = threading.Thread(target=reply)
                thread.start()
                try:
                    with self.assertRaisesRegex(RuntimeError, message):
                        baseline.headless.request_status(path)
                finally:
                    thread.join(timeout=2)
                self.assertFalse(thread.is_alive())

    def test_status_reader_rejects_oversized_frame(self):
        self.assert_invalid_status(b'x' * (baseline.headless.MAX_MESSAGE_BYTES + 1), 'exceeded')

    def test_status_reader_rejects_active_daemon(self):
        self.assert_invalid_status(b'{"ok":true,"state":"listening"}\n', 'non-idle')


if __name__ == '__main__':
    unittest.main()
