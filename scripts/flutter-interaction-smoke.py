#!/usr/bin/env python3
"""Drive the release FlView through native Linux input and verify BrowserCore state."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import threading
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402


MAX_NODES = 4096
STATUS_PREFIX = "Interaction status|"
BROWSER_STATUS_PREFIX = "Browser status|"
# A recreated document can lose up to one line when its root extent is clamped
# against the freshly reported Flutter viewport.
ROOT_RESTORE_CLAMP_TOLERANCE = 16.0


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", required=True)
    parser.add_argument("--library", required=True)
    parser.add_argument("--url", required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--pointer", required=True)
    parser.add_argument("--ibus", default="ibus")
    parser.add_argument("--ibus-engine", default="anthy")
    parser.add_argument("--timeout", type=float, default=45.0)
    return parser.parse_args()


def app_accessibles(process_id: int) -> list[Atspi.Accessible]:
    nodes: list[Atspi.Accessible] = []
    pending = [Atspi.get_desktop(index) for index in range(Atspi.get_desktop_count())]
    visited = 0
    while pending and visited < MAX_NODES:
        node = pending.pop()
        visited += 1
        try:
            node_process_id = node.get_process_id()
            count = min(node.get_child_count(), MAX_NODES - visited)
            pending.extend(node.get_child_at_index(index) for index in range(count))
            if node_process_id == process_id:
                nodes.append(node)
        except Exception:
            continue
    return nodes


def named_accessible(process_id: int, name: str) -> Atspi.Accessible | None:
    for node in app_accessibles(process_id):
        try:
            if node.get_name() == name:
                return node
        except Exception:
            continue
    return None


def current_status(process_id: int) -> str | None:
    statuses: list[str] = []
    for node in app_accessibles(process_id):
        try:
            name = node.get_name() or ""
            if name.startswith(STATUS_PREFIX):
                statuses.append(name)
        except Exception:
            continue
    return max(statuses, key=len) if statuses else None


def current_browser_status(process_id: int) -> str | None:
    for node in app_accessibles(process_id):
        try:
            name = node.get_name() or ""
            if name.startswith(BROWSER_STATUS_PREFIX):
                return name.removeprefix(BROWSER_STATUS_PREFIX)
        except Exception:
            continue
    return None


def accessible_name_sample(process_id: int) -> list[str]:
    names: list[str] = []
    for node in app_accessibles(process_id):
        try:
            name = node.get_name() or ""
            if name and name not in names:
                names.append(name)
        except Exception:
            continue
        if len(names) >= 80:
            break
    return names


def has_focused_role(process_id: int, role: str) -> bool:
    for node in app_accessibles(process_id):
        try:
            if (
                node.get_role_name() == role
                and node.get_state_set().contains(Atspi.StateType.FOCUSED)
            ):
                return True
        except Exception:
            continue
    return False


def wait_for(
    process: subprocess.Popen[str],
    timeout: float,
    description: str,
    probe,
):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            output = process.stdout.read() if process.stdout else ""
            raise SystemExit(
                f"Flutter shell exited before {description} ({process.returncode})\n{output}"
            )
        last = probe()
        if last:
            return last
        time.sleep(0.15)
    raise SystemExit(
        f"timed out waiting for {description}; last observation: {last!r}; "
        f"accessible names: {accessible_name_sample(process.pid)!r}"
    )


def status_fields(status: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for part in status.split("|")[1:]:
        key, separator, value = part.partition("=")
        if separator:
            fields[key] = value
    return fields


def status_number(fields: dict[str, str], name: str) -> float:
    value = fields.get(name, "")
    if not re.fullmatch(r"-?(?:\d+(?:\.\d*)?|\.\d+)", value):
        raise SystemExit(f"interaction status has invalid {name}: {value!r}")
    return float(value)


def status_number_matches(
    fields: dict[str, str], name: str, expected: float, tolerance: float = 0.01
) -> bool:
    return abs(status_number(fields, name) - expected) <= tolerance


def run_wtype(args: argparse.Namespace, *command: str) -> None:
    subprocess.run(
        [args.wtype, *command],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        timeout=10,
    )


def navigate_via_chrome(
    args: argparse.Namespace,
    process: subprocess.Popen[str],
    address: str,
) -> None:
    for x, y in [(500, 75)] * 3 + [
        (x, y) for y in (60, 75, 90) for x in (300, 500, 700, 900)
    ]:
        run_pointer(args, "click", str(x), str(y))
        deadline = time.monotonic() + 0.4
        while time.monotonic() < deadline:
            if has_focused_role(process.pid, "text"):
                break
            time.sleep(0.03)
        else:
            continue
        break
    else:
        raise SystemExit("native pointer scan did not focus the address field")
    run_wtype(args, "-M", "ctrl", "-k", "l", "-m", "ctrl")
    time.sleep(0.2)
    run_wtype(args, "-M", "ctrl", "-k", "a", "-m", "ctrl")
    time.sleep(0.2)
    run_wtype(args, "-d", "1", address)
    run_wtype(args, "-k", "Return")


def history_shortcut(args: argparse.Namespace, key: str) -> None:
    run_wtype(args, "-M", "alt", "-k", key, "-m", "alt")


def activate_ibus_engine(args: argparse.Namespace, engine: str) -> None:
    subprocess.run(
        [args.ibus, "engine", engine],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=10,
    )
    for _ in range(20):
        active_engine = subprocess.run(
            [args.ibus, "engine"],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()
        if active_engine == engine:
            return
        time.sleep(0.1)
    raise SystemExit(f"failed to activate IBus engine {engine!r}")


def ime_input(args: argparse.Namespace, codepoint: str) -> None:
    # Anthy preedit enters through IBus/GTK's real Wayland FlView IM context;
    # no AT-SPI setText or Vixen DispatchTextInput shortcut is used.
    romaji = {"306b": "ni", "1f98a": "kitsune"}[codepoint]
    run_wtype(args, "-d", "150", romaji)
    time.sleep(0.5)
    run_wtype(args, "-k", "Return")


def run_pointer(args: argparse.Namespace, *command: str) -> None:
    subprocess.run(
        [args.pointer, *command],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        timeout=10,
    )


def focus_with_pointer(
    args: argparse.Namespace,
    process: subprocess.Popen[str],
    target: str,
    candidates: list[tuple[int, int]] | None = None,
) -> tuple[int, int]:
    points = candidates or [
        (x, y) for x in (80, 160, 240) for y in range(120, 501, 15)
    ]
    for x, y in points:
        run_pointer(args, "click", str(x), str(y))
        deadline = time.monotonic() + 0.25
        while time.monotonic() < deadline:
            status = current_status(process.pid)
            if status:
                fields = status_fields(status)
                if fields.get("focus") == target:
                    return (x, y)
            time.sleep(0.03)
    raise SystemExit(
        f"native pointer scan did not focus {target}; status={current_status(process.pid)!r}"
    )


def main() -> int:
    args = arguments()
    app = Path(args.app).resolve()
    library = Path(args.library).resolve()
    if not app.is_file() or not os.access(app, os.X_OK):
        raise SystemExit(f"interaction app is not executable: {app}")
    if not library.is_file():
        raise SystemExit(f"interaction native library is missing: {library}")

    previous_engine = subprocess.run(
        [args.ibus, "engine"],
        check=False,
        capture_output=True,
        text=True,
        timeout=10,
    ).stdout.strip()
    direct_engine = "xkb:us::eng"
    activate_ibus_engine(args, direct_engine)

    env = os.environ.copy()
    env.update(
        {
            "GDK_BACKEND": "wayland",
            "GTK_A11Y": "1",
            "NO_AT_BRIDGE": "0",
            "LIBGL_ALWAYS_SOFTWARE": "1",
            "GTK_IM_MODULE": "ibus",
            "IBUS_ENABLE_SYNC_MODE": "1",
            "VIXEN_FFI_LIBRARY": str(library),
            "VIXEN_PROFILE_PATH": str(
                Path.cwd() / ".tmp" / "interaction-profile" / "profile.redb"
            ),
            "VIXEN_START_URL": (Path.cwd() / "fixtures" / "dom" / "basic.html")
            .resolve()
            .as_uri(),
        }
    )
    profile_dir = Path(env["VIXEN_PROFILE_PATH"]).parent
    profile_dir.mkdir(parents=True, exist_ok=True)
    Path(env["VIXEN_PROFILE_PATH"]).unlink(missing_ok=True)
    stop_fifo = profile_dir / "stopped-navigation.html"
    stop_fifo.unlink(missing_ok=True)
    fifo_opened = threading.Event()
    fifo_release = threading.Event()
    fifo_thread: threading.Thread | None = None
    process = subprocess.Popen(
        [str(app)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        wait_for(
            process,
            args.timeout,
            "initial controlled page",
            lambda: named_accessible(process.pid, "DOM Basic"),
        )
        wait_for(
            process,
            10,
            "browser status semantics",
            lambda: current_browser_status(process.pid),
        )
        navigate_via_chrome(args, process, args.url)
        wait_for(
            process,
            args.timeout,
            "native input semantics",
            lambda: named_accessible(process.pid, "Native input"),
        )
        wait_for(
            process,
            10,
            "contenteditable semantics",
            lambda: named_accessible(process.pid, "Native editor"),
        )
        wait_for(
            process,
            10,
            "nested scroll semantics",
            lambda: named_accessible(process.pid, "Nested scroll area"),
        )

        activate_ibus_engine(args, args.ibus_engine)
        input_x, input_y = focus_with_pointer(args, process, "input")
        time.sleep(1)
        ime_input(args, "306b")
        time.sleep(0.5)
        input_probe = current_status(process.pid)
        if not input_probe or status_fields(input_probe).get("inputComposition") != (
            "true:true:true"
        ):
            run_wtype(args, "-M", "ctrl", "j", "-m", "ctrl")
            time.sleep(0.5)
            ime_input(args, "306b")
        input_status = wait_for(
            process,
            10,
            "native input IME commit",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_fields(status).get("input", "") != ""
                and status_fields(status).get("inputComposition") == "true:true:true"
                else None
            ),
        )
        input_value = status_fields(input_status)["input"]

        editor_candidates = [
            (x, input_y + offset)
            for offset in range(40, 81, 5)
            for x in (input_x, max(1, input_x - 80), input_x + 80)
        ]
        focus_with_pointer(args, process, "editor", editor_candidates)
        time.sleep(1)
        ime_input(args, "1f98a")
        editor_status = wait_for(
            process,
            10,
            "contenteditable IME commit",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_fields(status).get("editor", "") != "draft"
                and status_fields(status).get("editorComposition") == "true:true:true"
                else None
            ),
        )
        editor_fields = status_fields(editor_status)
        if editor_fields.get("input") != input_value:
            raise SystemExit(f"native input text did not survive blur: {editor_status}")
        editor_value = editor_fields["editor"]

        scroll_candidates = [
            (x, input_y + offset)
            for offset in range(90, 166, 5)
            for x in (input_x, max(1, input_x - 80), input_x + 80)
        ]
        scroll_x, scroll_y = focus_with_pointer(
            args, process, "scroll", scroll_candidates
        )
        initial = status_fields(editor_status)
        initial_inner = status_number(initial, "inner")
        initial_root = status_number(initial, "root")
        initial_wheel_count = status_number(initial, "wheelCount")
        run_pointer(args, "wheel", str(scroll_x), str(scroll_y), "40")
        first_scroll = wait_for(
            process,
            5,
            "native nested wheel scroll",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_number(status_fields(status), "wheelCount")
                > initial_wheel_count
                and status_number(status_fields(status), "inner") > initial_inner
                else None
            ),
        )
        first_fields = status_fields(first_scroll)
        if status_number(first_fields, "root") != initial_root:
            raise SystemExit(f"first nested wheel unexpectedly moved the root: {first_scroll}")

        before_cancel_inner = status_number(first_fields, "inner")
        before_cancel_root = status_number(first_fields, "root")
        before_cancel_wheels = status_number(first_fields, "wheelCount")
        run_wtype(args, "c")
        wait_for(
            process,
            5,
            "native wheel cancellation mode",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_fields(status).get("cancelWheel") == "true"
                else None
            ),
        )
        run_pointer(args, "wheel", str(scroll_x), str(scroll_y), "40")
        canceled = wait_for(
            process,
            5,
            "cancelled native wheel event",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_number(status_fields(status), "wheelCount")
                > before_cancel_wheels
                and status_number(status_fields(status), "canceledWheelCount") >= 1
                else None
            ),
        )
        canceled_fields = status_fields(canceled)
        if (
            status_number(canceled_fields, "inner") != before_cancel_inner
            or status_number(canceled_fields, "root") != before_cancel_root
        ):
            raise SystemExit(f"cancelled native wheel changed scroll offsets: {canceled}")
        run_wtype(args, "c")

        run_pointer(args, "wheel", str(scroll_x), str(scroll_y), "1000")
        chained = wait_for(
            process,
            8,
            "native nested-to-root wheel chaining",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_number(status_fields(status), "inner")
                == status_number(status_fields(status), "innerMax")
                and status_number(status_fields(status), "root") > before_cancel_root
                else None
            ),
        )
        chained_fields = status_fields(chained)
        if (
            chained_fields.get("input") != input_value
            or chained_fields.get("editor") != editor_value
        ):
            raise SystemExit(
                f"native editing values did not survive later interaction: {chained}"
            )

        restored_inner = status_number(chained_fields, "inner")
        restored_root = status_number(chained_fields, "root")
        first_load = status_number(chained_fields, "load")
        history_shortcut(args, "Left")
        wait_for(
            process,
            10,
            "native back navigation",
            lambda: named_accessible(process.pid, "DOM Basic"),
        )
        history_shortcut(args, "Right")
        forward_status = wait_for(
            process,
            15,
            "native forward navigation with restored scrolling",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_number(status_fields(status), "load") != first_load
                and status_number_matches(
                    status_fields(status), "inner", restored_inner
                )
                and status_number_matches(
                    status_fields(status),
                    "root",
                    restored_root,
                    ROOT_RESTORE_CLAMP_TOLERANCE,
                )
                else None
            ),
        )
        forward_fields = status_fields(forward_status)
        forward_load = status_number(forward_fields, "load")
        restored_root = status_number(forward_fields, "root")

        run_wtype(args, "-M", "ctrl", "r", "-m", "ctrl")
        reload_status = wait_for(
            process,
            15,
            "native reload with restored scrolling",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_number(status_fields(status), "load") != forward_load
                and status_number_matches(
                    status_fields(status), "inner", restored_inner
                )
                and status_number_matches(status_fields(status), "root", restored_root)
                else None
            ),
        )
        reload_fields = status_fields(reload_status)

        os.mkfifo(stop_fifo, 0o600)

        def hold_fifo_open() -> None:
            with stop_fifo.open("wb", buffering=0):
                fifo_opened.set()
                fifo_release.wait(args.timeout)

        fifo_thread = threading.Thread(target=hold_fifo_open, daemon=True)
        fifo_thread.start()
        activate_ibus_engine(args, direct_engine)
        navigate_via_chrome(args, process, stop_fifo.resolve().as_uri())
        wait_for(
            process,
            10,
            "active gated file navigation",
            lambda: (
                status
                if fifo_opened.is_set()
                and (status := current_browser_status(process.pid)) not in {None, "Done"}
                else None
            ),
        )
        stopped_browser_status = None
        for x, y in [(93, 77)] * 3:
            run_pointer(args, "click", str(x), str(y))
            deadline = time.monotonic() + 0.75
            while time.monotonic() < deadline:
                status = current_browser_status(process.pid)
                if status in {"Stopped", "Navigation cancelled"}:
                    stopped_browser_status = status
                    break
                time.sleep(0.05)
            if stopped_browser_status is not None:
                break
        if stopped_browser_status is None:
            raise SystemExit(
                "native stop control did not cancel the gated navigation; "
                f"status={current_browser_status(process.pid)!r}"
            )
        stopped_page_status = wait_for(
            process,
            10,
            "visible page recovery after stop",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_number(status_fields(status), "load")
                == status_number(reload_fields, "load")
                and status_number_matches(
                    status_fields(status), "inner", restored_inner
                )
                and status_number_matches(status_fields(status), "root", restored_root)
                else None
            ),
        )
        stopped_fields = status_fields(stopped_page_status)
        print(
            "native interaction ok:",
            f"input={input_value!r}",
            f"editor={editor_value!r}",
            f"inner={stopped_fields['inner']}",
            f"root={stopped_fields['root']}",
            f"loads={stopped_fields['load']}",
            f"stop={stopped_browser_status!r}",
            f"wheels={chained_fields['wheelCount']}",
            f"canceled={chained_fields['canceledWheelCount']}",
        )
        return 0
    finally:
        fifo_release.set()
        if fifo_thread is not None:
            fifo_thread.join(timeout=5)
        stop_fifo.unlink(missing_ok=True)
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        subprocess.run(
            [args.ibus, "engine", previous_engine or direct_engine],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
        )


if __name__ == "__main__":
    sys.exit(main())
