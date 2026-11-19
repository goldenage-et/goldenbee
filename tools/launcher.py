#!/usr/bin/env python3
"""Bounded, real native Launcher CLI/HTTP acceptance (Linux, no Gateway)."""

import argparse
import ctypes
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import threading
import time
import urllib.request


def descendants():
    """Snapshot only this dedicated driver's descendants, including new sessions."""
    processes = {}
    for path in Path("/proc").glob("[0-9]*/stat"):
        try:
            fields = path.read_text().rsplit(")", 1)[1].split()
            processes[int(path.parent.name)] = (int(fields[1]), fields[19])
        except (OSError, ValueError, IndexError):
            continue  # A process may exit during the snapshot.
    owned = {os.getpid()}
    while True:
        children = {pid for pid, (parent, _) in processes.items() if parent in owned}
        expanded = owned | children
        if expanded == owned:
            break
        owned = expanded
    owned.remove(os.getpid())
    return {pid: processes[pid][1] for pid in owned}


ACTIVE = threading.Lock()


class Process:
    """Own one process group; drain both pipes while observing deadlines."""

    def __init__(self, args, cwd, limit=30, max_output=4 * 1024 * 1024):
        if not ACTIVE.acquire(blocking=False):
            raise RuntimeError("the dedicated Launcher driver owns one process tree at a time")
        self.deadline = time.monotonic() + limit
        self.max_output = max_output
        self.child = None
        self.selector = None
        self.watchdog = None
        self.output = {"stdout": bytearray(), "stderr": bytearray()}
        try:
            self.child = subprocess.Popen(
                args, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, start_new_session=True, bufsize=0,
            )
            self.selector = selectors.DefaultSelector()
            for name in self.output:
                pipe = getattr(self.child, name)
                os.set_blocking(pipe.fileno(), False)
                self.selector.register(pipe, selectors.EVENT_READ, name)
            self.watchdog = threading.Timer(limit, self.kill_group)
            self.watchdog.daemon = True
            self.watchdog.start()
        except BaseException:
            if self.child is not None:
                self.close()
            else:
                ACTIVE.release()
            raise

    def kill_group(self):
        owned = descendants()
        # Keep process-group cleanup while the unreaped leader reserves its ID.
        if self.child.returncode is None:
            try:
                os.killpg(self.child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        for pid, start in owned.items():
            try:
                current = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
                if current[19] == start:  # Never signal a recycled PID.
                    os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, FileNotFoundError):
                pass

    def pump(self):
        if time.monotonic() >= self.deadline:
            raise TimeoutError("native Launcher child exceeded its hard deadline")
        for key, _ in self.selector.select(0.025):
            chunk = os.read(key.fileobj.fileno(), 65536)
            if not chunk:
                self.selector.unregister(key.fileobj)
            else:
                self.output[key.data].extend(chunk)
                if sum(map(len, self.output.values())) > self.max_output:
                    raise RuntimeError("child output exceeded its byte bound")

    def text(self):
        return "\n".join(bytes(value).decode(errors="replace") for value in self.output.values())

    def until(self, text, seconds=10):
        deadline = min(self.deadline, time.monotonic() + seconds)
        while text not in self.text():
            self.pump()
            if self.child.poll() is not None or time.monotonic() >= deadline:
                raise AssertionError(f"did not observe {text!r}\n{self.text()}")

    def send(self, text):
        # Inputs are deliberately tiny: they fit in an empty PIPE_BUF.
        os.write(self.child.stdin.fileno(), text.encode())

    def finish(self):
        while self.child.poll() is None or self.selector.get_map():
            self.pump()
        if self.child.returncode != 0:
            raise AssertionError(f"Launcher exit {self.child.returncode}\n{self.text()}")

    def close(self):
        # Kill the owned group even when its leader already exited. Adopt and
        # reap grandchildren rather than leaving zombies to an unrelated init.
        try:
            if self.watchdog is not None:
                self.watchdog.cancel()
                if self.watchdog.is_alive():
                    self.watchdog.join(timeout=1)
            self.kill_group()
            self.child.wait(timeout=2)
            deadline = time.monotonic() + 2
            while True:
                self.kill_group()
                try:
                    pid, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if pid == 0:
                    if time.monotonic() >= deadline:
                        raise TimeoutError("owned Launcher descendants did not reap")
                    time.sleep(0.01)
        finally:
            if self.selector is not None:
                self.selector.close()
            for pipe in (self.child.stdin, self.child.stdout, self.child.stderr):
                if pipe:
                    pipe.close()
            ACTIVE.release()

    def __enter__(self):
        return self

    def __exit__(self, kind, value, traceback):
        try:
            if kind is not None:
                print(self.text(), file=sys.stderr, flush=True)
        finally:
            self.close()


def request(base, path, token, body=None):
    headers = {"Authorization": f"Bearer {token}"}
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, headers=headers)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(req, timeout=3) as response:
        return response.status, response.read(2 * 1024 * 1024)


def verify(binary, config, agent, steps, token, cwd):
    with Process([binary, "dev", "--config", config, "--agent", agent], cwd) as cli:
        cli.until("Type a message to chat.")
        for prompt, expected in steps:
            cli.send(prompt + "\n")
            cli.until(expected, seconds=15)
        cli.send("exit\n")
        cli.child.stdin.close()
        cli.finish()
        assert "Goodbye." in cli.text(), cli.text()
        print(cli.text(), flush=True)

    with Process([binary, "serve", "--config", config, "--agent", agent,
                  "--addr", "127.0.0.1:0"], cwd) as web:
        web.until("native ADK server listening on ")
        line = next(line for line in web.text().splitlines() if "listening on " in line)
        base = line.split("listening on ", 1)[1].strip()
        assert request(base, "/ui/", token)[0] == 200
        status, apps = request(base, "/api/apps", token)
        assert status == 200 and agent in json.loads(apps), apps
        for prompt, expected in steps:
            status, result = request(base, "/api/run", token, {
                "appName": agent, "userId": "user", "sessionId": "release-web",
                "newMessage": {"role": "user", "parts": [{"text": prompt}]},
            })
            events = json.loads(result)
            final = "\n".join(
                "".join(part.get("text", "") for part in
                        (event.get("content") or {}).get("parts", []))
                for event in events
            )
            assert status == 200 and expected in final, result
            print(final, flush=True)
        os.kill(web.child.pid, signal.SIGTERM)
        web.finish()
        print(web.text(), flush=True)
    print(f"PASS full native Launcher CLI+web: {agent}", flush=True)


def main():
    # PR_SET_CHILD_SUBREAPER: only adopt descendants of this test driver.
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "cannot establish child ownership")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--config", required=True)
    parser.add_argument("--agent", required=True)
    parser.add_argument("--input", action="append")
    parser.add_argument("--expect", action="append")
    parser.add_argument("--token", default="dev-token")
    args = parser.parse_args()
    prompts = args.input or ["full launcher canary"]
    expected = args.expect or ["echo: full launcher canary"]
    if len(prompts) != len(expected):
        parser.error("each --input requires one --expect")
    verify(str(Path(args.binary).resolve()), str(Path(args.config).resolve()),
           args.agent, list(zip(prompts, expected)), args.token, os.getcwd())


if __name__ == "__main__":
    main()
