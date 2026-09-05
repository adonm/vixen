#!/usr/bin/env python3
"""Profile precedence and outside-checkout CLI regressions (stdlib only)."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading


class Handler(BaseHTTPRequestHandler):
    user_agents = []

    def log_message(self, *args):
        pass

    def do_GET(self):
        self.user_agents.append(self.headers.get("User-Agent"))
        body = b"<html><title>Profile Test</title><p>profile fixture</p></html>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    root = Path(__file__).resolve().parents[1]
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else root / "vixen").resolve()
    (root / ".tmp").mkdir(exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="vixen-profiles-", dir=root / ".tmp") as tmp:
            work = Path(tmp)
            url = f"http://127.0.0.1:{server.server_port}/page"
            for name in ("home", "xdg", "legacy", "vixen", "explicit"):
                case = work / name
                case.mkdir()
                env = dict(os.environ)
                for key in ("VIXEN_PROFILE", "SPIKE_PROFILE", "XDG_CONFIG_HOME", "HOME"):
                    env.pop(key, None)
                env["HOME"] = str(case / "home")
                old = case / "home/.config/spikebrowser"
                old.mkdir(parents=True)
                sentinel = old / "keep"
                sentinel.write_text("existing user data")
                expected = case / "home/.config/vixen"
                if name != "home":
                    env["XDG_CONFIG_HOME"] = str(case / "config/nested")
                    expected = Path(env["XDG_CONFIG_HOME"]) / "vixen"
                if name in ("legacy", "vixen", "explicit"):
                    env["SPIKE_PROFILE"] = str(case / "legacy-profile")
                    expected = Path(env["SPIKE_PROFILE"])
                if name in ("vixen", "explicit"):
                    env["VIXEN_PROFILE"] = str(case / "vixen-profile")
                    expected = Path(env["VIXEN_PROFILE"])
                flags = []
                if name == "explicit":
                    expected = case / "explicit/nested/profile"
                    flags = ["--profile", str(expected)]
                for command in (["fetch"], ["browse", "--dump"]):
                    p = subprocess.run([str(binary), *command, *flags, url], cwd=case,
                                       env=env, capture_output=True, timeout=10)
                    assert p.returncode == 0, (name, command, p.stderr)
                    assert (expected / "storedb.sqlite").is_file(), (name, expected)
                    assert b"\x1b_G" not in p.stdout
                    if command[0] == "browse":
                        assert b"profile fixture" in p.stdout
                assert sentinel.read_text() == "existing user data"
                assert list(old.iterdir()) == [sentinel], "legacy profile modified implicitly"
                print(f"PASS profile {name}: both commands agree, old data untouched")

            blocked = work / "not-a-directory"
            blocked.write_text("keep")
            p = subprocess.run([str(binary), "fetch", "--profile", str(blocked), url],
                               cwd=work, capture_output=True, timeout=10)
            assert p.returncode != 0 and blocked.read_text() == "keep"
            env = dict(os.environ)
            for key in ("VIXEN_PROFILE", "SPIKE_PROFILE", "XDG_CONFIG_HOME", "HOME"):
                env.pop(key, None)
            p = subprocess.run([str(binary), "fetch", url], cwd=work, env=env,
                               capture_output=True, timeout=10)
            assert p.returncode != 0 and b"VIXEN_PROFILE" in p.stderr
            assert Handler.user_agents and all(x == "Vixen/0.1" for x in Handler.user_agents)
            print("PASS profile failures are explicit; Vixen user agent")
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()


if __name__ == "__main__":
    main()
