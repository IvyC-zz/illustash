"""#68 owned native host: synthetic packages, pipe EOF and parent crashes only."""
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import unittest
from tests import test_native_lifecycle as life
from tests.test_read_http import free_port, wait_until
from tools.verify_read_http import fetch


class DesktopLifecycleTest(unittest.TestCase):
    setUpClass = classmethod(life.NativeLifecycleTest.setUpClass.__func__)
    package = life.NativeLifecycleTest.package
    request = life.NativeLifecycleTest.request
    stop = life.NativeLifecycleTest.stop
    setUp = life.NativeLifecycleTest.setUp

    def start(self, path, extra=(), command=None):
        log = tempfile.TemporaryFile()
        args = [str(self.binary), 'http', '--native', '--no-auto-sync', '--library', str(path),
                '--port', str(self.port), '--root', str(self.resources), '--no-names', '--no-index-check', *extra]
        env = {**self.env, 'ILLUSTASH_DESKTOP_STDIN': '1'}
        process = subprocess.Popen(args, env=env, stdin=subprocess.PIPE, stdout=log, stderr=log, **life.SPAWN)
        process.stdin.write(b"\x01"); process.stdin.flush()
        self.addCleanup(log.close)
        self.addCleanup(lambda: process.stdin.close() if not process.stdin.closed else None)
        self.addCleanup(self.stop, process)
        wait_until(lambda: fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['status'] == 200, timeout=10)
        return process, log

    def assert_port_released(self):
        with socket.socket() as listener:
            # Windows 的 SO_REUSEADDR 允许绑定仍在监听的端口，那样检查就失效了。
            if os.name != 'nt':
                listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('127.0.0.1', self.port))

    def progress(self, log):
        """Startup stages, checked to precede the ready line and never go back."""
        log.seek(0)
        lines = log.read().decode().splitlines()
        ready = next(i for i, line in enumerate(lines) if line.startswith('图库已启动：'))
        progress = [(i, int(line.split('：', 1)[1].split(' ', 1)[0]), line.split(' ', 1)[1])
                    for i, line in enumerate(lines) if line.startswith('启动进度：')]
        self.assertTrue(progress)
        self.assertTrue(all(i < ready for i, _, _ in progress))
        values = [v for _, v, _ in progress]
        self.assertEqual(values, sorted(values))
        self.assertEqual(values[0], 0)
        return {stage for _, _, stage in progress}

    def test_startup_progress_precedes_ready_and_never_goes_back(self):
        # A normal opening runs no full integrity check before it is ready (#212).
        self.assertFalse(any('检查图库数据' in stage for stage in self.progress(self.log)))
        # After a crash the next opening checks both databases first, and reports it.
        self.process.kill(); self.process.wait(5)
        _, log = self.start(self.lib.path)
        self.assertIn('上次没有正常关闭，正在检查图库数据（source.sqlite）', self.progress(log))

    def test_eof_waits_and_releases_port_after_library_switch(self):
        second = self.package('second')
        response = self.request('/api/libraries/open', {'path': str(second.path)})
        self.assertEqual(response['status'], 200)
        self.process.stdin.close()
        self.assertEqual(self.process.wait(10), 0)
        self.assert_port_released()
        self.assertFalse(self.sentinel.exists())

    def test_explicit_port_conflict_fails_without_killing_existing_host(self):
        args = [str(self.binary), 'http', '--native', '--library', str(self.lib.path),
                '--port', str(self.port), '--root', str(self.resources), '--no-names', '--no-index-check']
        result = subprocess.run(args, env=self.env, capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertIsNone(self.process.poll())
        self.assertEqual(self.request('/api/bootstrap')['status'], 200)

    def test_parent_crash_closes_pipe_and_native_host_exits(self):
        self.process.stdin.close()
        self.process.wait(10)
        code = ('import json,subprocess,sys; '
                'p=subprocess.Popen(json.loads(sys.argv[1]),stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); '
                'p.stdin.write(bytes([1])); p.stdin.flush(); print(p.pid,flush=True); sys.stdin.read()')
        args = [str(self.binary), 'http', '--native', '--library', str(self.lib.path),
                '--port', str(self.port), '--root', str(self.resources), '--no-names', '--no-index-check', '--no-auto-sync']
        parent = subprocess.Popen([sys.executable, '-c', code, json.dumps(args)],
                                  env={**self.env, 'ILLUSTASH_DESKTOP_STDIN': '1'},
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        self.addCleanup(parent.stdin.close)
        self.addCleanup(parent.stdout.close)
        self.addCleanup(lambda: parent.kill() if parent.poll() is None else None)
        child_pid = int(parent.stdout.readline())
        wait_until(lambda: fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['status'] == 200, timeout=10)
        parent.kill(); parent.wait(5)
        def released():
            try:
                self.assert_port_released(); return True
            except OSError:
                return False
        try:
            wait_until(released, timeout=10)
        finally:
            if not released():
                os.kill(child_pid, getattr(signal, 'SIGKILL', signal.SIGTERM))
        self.assertFalse(self.sentinel.exists())


from tests import test_native_research as research

@unittest.skipIf(os.name == 'nt', 'Windows 上原生 worker 按设计拒绝启动（app_tasks），不适用')
class DesktopResearchLifecycleTest(unittest.TestCase):
    setUpClass = DesktopLifecycleTest.__dict__['setUpClass']
    package = DesktopLifecycleTest.package
    request = DesktopLifecycleTest.request
    stop = DesktopLifecycleTest.stop
    start = DesktopLifecycleTest.start
    setUp = research.NativeResearchTest.setUp
    restart = research.NativeResearchTest.restart
    finished = research.NativeResearchTest.finished
    state = research.NativeResearchTest.state

    def test_eof_during_worker_waits_and_removes_worker_group(self):
        self.mode.write_text('hang')
        self.pid.unlink(missing_ok=True)
        self.assertEqual(self.request('/api/similar', {})['status'], 200)
        wait_until(self.pid.exists, timeout=5)
        pid = int(self.pid.read_text())
        self.process.stdin.close()
        self.assertEqual(self.process.wait(10), 0)
        wait_until(lambda: not research.group_exists(pid), timeout=5)
        DesktopLifecycleTest.assert_port_released(self)

    def test_parent_crash_during_worker_removes_worker_group(self):
        self.process.stdin.close(); self.process.wait(10)
        self.mode.write_text('hang'); self.pid.unlink(missing_ok=True)
        code = ('import json,subprocess,sys; '
                'p=subprocess.Popen(json.loads(sys.argv[1]),stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); '
                'p.stdin.write(bytes([1])); p.stdin.flush(); print(p.pid,flush=True); sys.stdin.read()')
        args = [str(self.binary), 'http', '--native', '--library', str(self.lib.path),
                '--port', str(self.port), '--root', str(self.resources), '--no-names', '--no-index-check', '--no-auto-sync']
        parent = subprocess.Popen([sys.executable, '-c', code, json.dumps(args)],
                                  env={**self.env, 'ILLUSTASH_DESKTOP_STDIN': '1'}, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        def cleanup():
            if parent.poll() is None: parent.kill()
            parent.wait(5); parent.stdin.close(); parent.stdout.close()
        self.addCleanup(cleanup)
        host_pid = int(parent.stdout.readline())
        def stop_host_if_needed():
            if self.host_gone(): return
            try: os.kill(host_pid, signal.SIGTERM)
            except ProcessLookupError: pass
        self.addCleanup(stop_host_if_needed)
        wait_until(lambda: fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['status'] == 200, timeout=10)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.request('/api/similar', {})['status'], 200)
        wait_until(self.pid.exists, timeout=5)
        worker_pid = int(self.pid.read_text())
        parent.kill(); parent.wait(5)
        wait_until(lambda: not research.group_exists(worker_pid), timeout=10)
        wait_until(lambda: self.host_gone(), timeout=10)

    def host_gone(self):
        try:
            DesktopLifecycleTest.assert_port_released(self); return True
        except OSError:
            return False
