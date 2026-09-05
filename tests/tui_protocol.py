#!/usr/bin/env python3
"""Vixen PTY/Kitty regressions; no graphical terminal or third-party modules.

This models only the output operations Vixen uses, and rejects unexpected
operations. It is independent of the Odin decoder/geometry helpers. Real
Ghostty/Kitty presentation and interaction still require manual checks.
"""
import base64
import contextlib
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
from urllib.parse import urlencode

ROOT = Path(__file__).resolve().parents[1]
CSI = re.compile(rb"\x1b\[([0-9;?]*)([@-~])")


class Screen:
    def __init__(self, send, cols, rows, cw, ch, typeahead=False, replies=True):
        self.send = send
        self.cols, self.rows, self.cw, self.ch = cols, rows, cw, ch
        self.typeahead = typeahead
        self.replies = replies
        self.buf = bytearray()
        self.alt = False
        self.entered = self.restored = False
        self.hidden = self.paste = False
        self.row = self.col = 1
        self.cells = {}
        self.images = {}
        self.ids = set()
        self.frames = 0
        self.deletes = 0
        self.current = None
        self.payload = bytearray()

    def feed(self, chunk):
        self.buf.extend(chunk)
        while self.buf:
            if self.buf[0] != 27:
                c = self.buf.pop(0)
                if self.alt:
                    assert 32 <= c <= 126, f"unexpected terminal text/control byte: {c}"
                    assert 1 <= self.row <= self.rows, (self.row, self.rows)
                    assert 1 <= self.col < self.cols, f"text overflow at {self.col}/{self.cols}"
                    self.cells[self.row, self.col] = chr(c)
                    self.col += 1
                continue
            if len(self.buf) < 3:
                return
            if self.buf.startswith(b"\x1b_G"):
                end = self.buf.find(b"\x1b\\", 3)
                if end < 0:
                    return
                packet = bytes(self.buf[3:end])
                del self.buf[:end + 2]
                self.graphics(packet)
                continue
            match = CSI.match(self.buf)
            if not match:
                # Every output escape must be CSI or a Kitty packet.
                if self.buf.startswith(b"\x1b[") and not any(64 <= c <= 126 for c in self.buf[2:]):
                    return
                raise AssertionError(f"unexpected escape: {bytes(self.buf[:80])!r}")
            params, final = match.groups()
            del self.buf[:match.end()]
            self.csi(params.decode(), final.decode())

    def csi(self, params, final):
        if final in ("h", "l"):
            assert params in ("?1049", "?25", "?2004"), params
            enabled = final == "h"
            if params == "?1049":
                self.alt = enabled
                self.entered |= enabled
                self.restored |= not enabled
                self.row = self.col = 1
            elif params == "?25":
                self.hidden = not enabled
            else:
                self.paste = enabled
        elif final == "t":
            assert params in ("14", "16"), params
            if not self.replies:
                return
            # Fragment replies and place a real key before the first reply.
            prefix = b"u" if self.typeahead else b""
            self.typeahead = False
            if params == "16":
                reply = f"\x1b[6;{self.ch};{self.cw}t".encode()
            else:
                reply = f"\x1b[4;{self.rows*self.ch};{self.cols*self.cw}t".encode()
            self.send(prefix + reply[:4])
            time.sleep(0.005)
            self.send(reply[4:])
        elif final == "H":
            values = [int(x) if x else 1 for x in params.split(";")]
            self.row, self.col = values if len(values) == 2 else (1, 1)
            assert 1 <= self.row <= self.rows and 1 <= self.col <= self.cols, (values, self.cols, self.rows)
        elif final == "m":
            assert params in ("0", "7"), params
        elif final == "K":
            assert params in ("", "0"), params
            for col in range(self.col, self.cols + 1):
                self.cells.pop((self.row, col), None)
        elif final == "J":
            assert params == "2", f"unexpected erase mode {params!r}"
            self.cells.clear()
        else:
            raise AssertionError(f"unexpected CSI {params}{final}")

    def graphics(self, packet):
        header, sep, payload = packet.partition(b";")
        assert sep
        attrs = dict(item.split("=", 1) for item in header.decode().split(","))
        if attrs.get("a") == "d":
            assert attrs.get("d") == "I", "must delete only the owned image"
            assert not payload
            self.images.pop(int(attrs["i"]), None)
            self.deletes += 1
            return
        assert len(payload) <= 4096, "oversized Kitty chunk"
        if attrs.get("a") == "T":
            assert self.current is None, "interleaved image transfers"
            assert attrs.get("C") == "1", "image placement would move the cursor"
            assert attrs.get("p") == "1" and int(attrs.get("i", "0")) > 0
            assert attrs.get("q") == "2" and attrs.get("f") == "100"
            self.current = attrs
            self.payload.clear()
        assert self.current is not None, "continuation without an image"
        self.payload.extend(payload)
        if attrs.get("m") == "1":
            return
        png = base64.b64decode(self.payload, validate=True)
        assert png[:8] == b"\x89PNG\r\n\x1a\n" and png[12:16] == b"IHDR"
        width, height = struct.unpack(">II", png[16:24])
        a = self.current
        assert (width, height) == (int(a["s"]), int(a["v"]))
        assert (width, height) == (self.cols*self.cw, (self.rows-2)*self.ch), (width, height, self.cols, self.rows)
        assert 0 < width <= 4096 and 0 < height <= 4096
        assert (int(a["c"]), int(a["r"])) == (self.cols, self.rows-2)
        assert (self.row, self.col) == (1, 1)
        image_id = int(a["i"])
        self.images[image_id] = (width, height)
        self.ids.add(image_id)
        assert len(self.images) == len(self.ids) == 1, "images accumulating across frames"
        self.frames += 1
        self.current = None

    def line(self, row):
        return "".join(self.cells.get((row, c), " ") for c in range(1, self.cols + 1))


class Browser:
    def __init__(self, binary, root, url, cols=40, rows=10, cw=10, ch=20, pixels=True, typeahead=False, replies=True):
        self.master, self.slave = pty.openpty()
        self.pixels = pixels
        self.root = root
        self.raw = bytearray()
        self.screen = Screen(self.send, cols, rows, cw, ch, typeahead, replies)
        self.winsize(cols, rows, cw, ch)
        self.original = termios.tcgetattr(self.slave)
        env = dict(os.environ, TERM=os.environ.get("VIXEN_TEST_TERM", "xterm-256color"))
        env.pop("KITTY_WINDOW_ID", None)
        self.proc = subprocess.Popen([str(binary), "browse", "--profile", str(root), url],
                                     cwd=ROOT, env=env, stdin=self.slave, stdout=self.slave,
                                     stderr=self.slave, start_new_session=True)

    def winsize(self, cols, rows, cw, ch):
        xp, yp = (cols*cw, rows*ch) if self.pixels else (0, 0)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, xp, yp))

    def send(self, data):
        while data:
            n = os.write(self.master, data)
            data = data[n:]

    def pump(self, timeout=0.05):
        ready, _, _ = select.select([self.master], [], [], timeout)
        if ready:
            chunk = os.read(self.master, 65536)
            self.raw.extend(chunk)
            self.screen.feed(chunk)

    def until(self, predicate, timeout=8):
        deadline = time.monotonic() + timeout
        while not predicate():
            assert time.monotonic() < deadline, "PTY condition timed out"
            self.pump()
            if self.proc.poll() is not None and not predicate():
                raise AssertionError(f"browser exited early: {self.proc.returncode}")

    def settle(self, seconds=0.25):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.pump(0.02)

    def start(self):
        self.until(lambda: self.screen.frames > 0)
        self.settle()

    def resize(self, cols, rows, cw=None, ch=None):
        self.settle()
        s = self.screen
        cw, ch = cw or s.cw, ch or s.ch
        frames, deletes = s.frames, s.deletes
        s.cols, s.rows, s.cw, s.ch = cols, rows, cw, ch
        self.winsize(cols, rows, cw, ch)
        if rows > 2 and cols*cw <= 4096 and (rows-2)*ch <= 4096:
            self.until(lambda: s.frames > frames)
        else:
            self.until(lambda: s.deletes > deletes)
            assert not s.images
        self.settle()

    def log(self):
        path = self.root / "tui.log"
        return path.read_text() if path.exists() else ""

    def quit(self, key=b"q"):
        self.send(key)
        self.until(lambda: self.screen.restored)
        self.proc.wait(timeout=5)
        assert self.proc.returncode == 0
        assert termios.tcgetattr(self.slave) == self.original, "termios not restored"
        assert not self.screen.hidden and not self.screen.paste and not self.screen.images
        assert self.screen.current is None

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=2)
        os.close(self.master)
        os.close(self.slave)


@contextlib.contextmanager
def browser(*args, **kwargs):
    b = Browser(*args, **kwargs)
    try:
        yield b
    except BaseException:
        print(f"PTY tail: {bytes(b.raw[-1200:])!r}\nLog tail: {b.log()[-2000:]}", file=sys.stderr)
        raise
    finally:
        b.close()


@contextlib.contextmanager
def server(root):
    portfile = root / "port"
    with (root / "server.log").open("wb") as log:
        proc = subprocess.Popen([sys.executable, "corpus/testserver.py", str(portfile)],
                                cwd=ROOT, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 5
            while not portfile.exists() or not portfile.read_text():
                assert proc.poll() is None and time.monotonic() < deadline, "server startup failed"
                time.sleep(0.01)
            yield f"http://127.0.0.1:{int(portfile.read_text())}"
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=2)


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "vixen").resolve()
    (ROOT / ".tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vixen-pty-", dir=ROOT / ".tmp") as tmp:
        root = Path(tmp)
        with server(root) as base:
            with browser(binary, root / "geometry", base + "/relayout?" + "x"*300) as b:
                b.start()
                frames, count = b.screen.frames, len(b.raw)
                b.send(b"x\x1b[99~g")  # ignored input and already-at-top
                b.settle(0.4)
                assert b.screen.frames == frames and len(b.raw) == count, "no-op redraw"
                b.send(b"j")
                b.until(lambda: b.screen.frames > frames)
                for cols, rows in [(100, 30), (17, 4), (1, 1), (64, 18), (1000, 1000), (40, 10)]:
                    b.resize(cols, rows)
                b.resize(40, 10, 12, 24)  # font/pixel size change, same cells
                b.quit()
            print("PASS pty geometry, idle resize/redraw, image replacement, restoration")

            with browser(binary, root / "queries", base + "/relayout", pixels=False, typeahead=True) as b:
                b.start()
                assert b.screen.line(b.screen.rows).startswith("URL:"), "typeahead lost to metrics reply"
                frames = b.screen.frames
                b.send(b"\x1b[200~" + (base + "/form\nq").encode() + b"\x1b[201~")
                b.settle()
                assert b.screen.frames == frames, "URL editing uploaded page pixels"
                b.quit(b"\x03")
            print("PASS pty fragmented metrics, preserved typeahead, paste, URL-editor quit")

            with browser(binary, root / "fallback", base + "/relayout", cw=8, ch=16, pixels=False, replies=False) as b:
                b.start()
                b.resize(50, 12)
                b.send(b"\x1b")
                b.settle(0.2)
                b.quit()
            print("PASS pty missing metric replies use bounded fallback; lone Escape expires")

            with browser(binary, root / "forms", base + "/form", cols=80, rows=24) as b:
                b.start()
                b.send(b"\t\t\x1b[Z")  # q -> submit -> q
                b.send(b"\xc3")
                b.settle(0.2)  # a slow UTF-8 continuation must not time out
                b.send(b"\xa9")
                for byte in "hi日本😀".encode():
                    b.send(bytes([byte]))
                    time.sleep(0.01)
                b.settle()
                b.resize(55, 15)
                b.resize(90, 25)
                b.send(b"\r")
                expected = urlencode({"q": "éhi日本😀", "src": "web"})
                b.until(lambda: expected in b.log())
                b.quit()
            print("PASS pty fragmented Unicode, reverse tab, focus/value preservation, submission")

            with browser(binary, root / "hangup", base + "/relayout") as b:
                b.start()
                os.close(b.master)
                b.master = os.open(os.devnull, os.O_RDONLY)  # cleanup still owns one fd
                b.proc.wait(timeout=5)
                assert b.proc.returncode == 0, "hangup did not terminate cleanly"
            print("PASS pty hangup exits instead of redrawing in a busy loop")

            p = subprocess.run([str(binary), "browse", "--profile", str(root / "headless"), base + "/form"],
                               cwd=ROOT, stdin=subprocess.DEVNULL, capture_output=True, timeout=10)
            assert p.returncode != 0 and b"terminal required" in p.stderr
            assert b"\x1b_G" not in p.stdout
            print("PASS non-terminal interactive invocation fails explicitly")


if __name__ == "__main__":
    main()
