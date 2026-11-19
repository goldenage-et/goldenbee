"""Regression checks for deadlock, readiness, timeout, and descendant cleanup."""

import ctypes
import os
import signal
from pathlib import Path
import sys
import tempfile
import time
import unittest

from launcher import Process


class Lifecycle(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0

    def test_drains_stdout_and_stderr_before_waiting(self):
        code = "import os; os.write(1,b'x'*200000); os.write(2,b'y'*200000)"
        with Process([sys.executable, "-c", code], os.getcwd(), limit=2) as child:
            child.finish()
            self.assertEqual(len(child.output["stdout"]), 200000)
            self.assertEqual(len(child.output["stderr"]), 200000)

    def test_readiness_failure_kills_and_reaps_child(self):
        with self.assertRaises(AssertionError):
            with Process([sys.executable, "-c", "import time; time.sleep(60)"],
                         os.getcwd(), limit=2) as child:
                pid = child.child.pid
                child.until("never ready", seconds=0.1)
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_deadline_kills_and_reaps_grandchild(self):
        with tempfile.TemporaryDirectory() as directory:
            pidfile = Path(directory) / "child.pid"
            code = ("import subprocess,time,pathlib; "
                    "p=subprocess.Popen(['sleep','60']); "
                    f"pathlib.Path({str(pidfile)!r}).write_text(str(p.pid)); "
                    "print('ready',flush=True); time.sleep(60)")
            with self.assertRaises((TimeoutError, AssertionError)):
                with Process([sys.executable, "-c", code], directory, limit=0.3) as child:
                    child.until("ready", seconds=0.2)
                    descendant = int(pidfile.read_text())
                    child.finish()
                    raise AssertionError("deadline must fail")
            with self.assertRaises(ProcessLookupError):
                os.kill(descendant, 0)

    def test_cleanup_reaps_descendant_that_starts_its_own_session(self):
        with tempfile.TemporaryDirectory() as directory:
            pidfile = Path(directory) / "detached.pid"
            code = ("import subprocess,time,pathlib; "
                    "p=subprocess.Popen(['sleep','60'],start_new_session=True); "
                    f"pathlib.Path({str(pidfile)!r}).write_text(str(p.pid)); "
                    "print('ready',flush=True); time.sleep(60)")
            try:
                with Process([sys.executable, "-c", code], directory, limit=2) as child:
                    child.until("ready")
                    descendant = int(pidfile.read_text())
                with self.assertRaises(ProcessLookupError):
                    os.kill(descendant, 0)
            finally:
                # The RED run must not leak its deliberately detached fixture.
                if pidfile.exists():
                    pid = int(pidfile.read_text())
                    try:
                        os.kill(pid, signal.SIGKILL)
                        os.waitpid(pid, 0)
                    except (ProcessLookupError, ChildProcessError):
                        pass


if __name__ == "__main__":
    unittest.main()
