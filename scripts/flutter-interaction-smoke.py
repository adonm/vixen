#!/usr/bin/env python3
"""Drive the release FlView through native Linux input and verify BrowserCore state."""

from __future__ import annotations

import argparse
from collections import deque
import math
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
MAX_APP_DIMENSION = 4096
MAX_APP_VIEWPORT_BYTES = 64 * 1024 * 1024
STATUS_PREFIX = "Interaction status|"
BROWSER_STATUS_PREFIX = "Browser status|"
RENDER_COMMIT_RE = re.compile(
    r"Vixen renderer presented context=(\d+) document=(\d+) "
    r"commit=(\d+) scroll_y=(none|-?(?:\d+(?:\.\d*)?|\.\d+))"
)
# A recreated document can lose up to one line when its root extent is clamped
# against the freshly reported Flutter viewport.
ROOT_RESTORE_CLAMP_TOLERANCE = 16.0


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", required=True)
    parser.add_argument("--app-headless-window", action="store_true")
    parser.add_argument("--app-viewport")
    parser.add_argument("--library", required=True)
    parser.add_argument("--url", required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--pointer", required=True)
    parser.add_argument("--ibus", default="ibus")
    parser.add_argument("--ibus-engine", default="mozc-jp")
    parser.add_argument("--timeout", type=float, default=45.0)
    return parser.parse_args()


def application_command(args: argparse.Namespace) -> list[str]:
    command = [str(Path(args.app).resolve())]
    if args.app_headless_window:
        command.append("--vixen-headless-window")
    if args.app_viewport is None:
        return command
    match = re.fullmatch(r"([1-9][0-9]*)x([1-9][0-9]*)", args.app_viewport)
    if match is None:
        raise SystemExit("--app-viewport must be WIDTHxHEIGHT within renderer bounds")
    width, height = (int(value) for value in match.groups())
    if (
        width > MAX_APP_DIMENSION
        or height > MAX_APP_DIMENSION
        or width * height * 4 > MAX_APP_VIEWPORT_BYTES
    ):
        raise SystemExit("--app-viewport must be WIDTHxHEIGHT within renderer bounds")
    command.append(f"--vixen-viewport={width}x{height}")
    return command


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


def accessible_action_evidence(
    node: Atspi.Accessible,
    *,
    role,
    required_states: tuple,
    action_name: str,
):
    if node.get_role() != role:
        raise ValueError(
            f"role was {node.get_role_name()!r}, expected {role!r}"
        )
    states = node.get_state_set()
    missing = [state for state in required_states if not states.contains(state)]
    if missing:
        raise ValueError(f"missing required AT-SPI states: {missing!r}")
    rect = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
    bounds = (rect.x, rect.y, rect.width, rect.height)
    if (
        not all(math.isfinite(float(value)) for value in bounds)
        or rect.width <= 0
        or rect.height <= 0
    ):
        raise ValueError(f"AT-SPI bounds are not finite and positive: {bounds!r}")
    action = node.get_action_iface()
    actions = [
        action.get_action_name(index) for index in range(action.get_n_actions())
    ]
    expected = action_name.casefold()
    index = next(
        (
            index
            for index, name in enumerate(actions)
            if isinstance(name, str) and name.casefold() == expected
        ),
        None,
    )
    if index is None:
        raise ValueError(
            f"AT-SPI action {action_name!r} is missing from {actions!r}"
        )
    return action, index, bounds


def named_accessible_action(process_id: int, name: str, action_name: str):
    node = named_accessible(process_id, name)
    if node is None:
        return None
    try:
        action, index, bounds = accessible_action_evidence(
            node,
            role=Atspi.Role.TEXT,
            required_states=(
                Atspi.StateType.EDITABLE,
                Atspi.StateType.VISIBLE,
                Atspi.StateType.SHOWING,
            ),
            action_name=action_name,
        )
        return node, action, index, bounds
    except (Exception, ValueError):
        return None


def accessible_centers(process_id: int, name: str) -> list[tuple[int, int]]:
    centers: list[tuple[int, int]] = []
    for node in app_accessibles(process_id):
        try:
            if node.get_name() != name:
                continue
            rect = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
            if rect.width <= 0 or rect.height <= 0:
                continue
            center = (round(rect.x + rect.width / 2), round(rect.y + rect.height / 2))
            if center not in centers:
                centers.append(center)
        except Exception:
            continue
    return centers


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
            output = process_output(process)
            raise SystemExit(
                f"Flutter shell exited before {description} ({process.returncode})\n{output}"
            )
        last = probe()
        if last:
            return last
        time.sleep(0.15)
    raise SystemExit(
        f"timed out waiting for {description}; last observation: {last!r}; "
        f"accessible names: {accessible_name_sample(process.pid)!r}; "
        f"process output: {process_output(process)[-8000:]}"
    )


def process_output(process: subprocess.Popen[str]) -> str:
    lines = getattr(process, "_vixen_output_lines", [])
    lock = getattr(process, "_vixen_output_lock", None)
    if lock is None:
        return "".join(lines)
    with lock:
        return "".join(lines)


def renderer_commits(
    process: subprocess.Popen[str],
) -> list[tuple[int, int, int, float | None]]:
    commits: list[tuple[int, int, int, float | None]] = []
    for match in RENDER_COMMIT_RE.finditer(process_output(process)):
        scroll = None if match[4] == "none" else float(match[4])
        commits.append((int(match[1]), int(match[2]), int(match[3]), scroll))
    return commits


def wait_for_renderer_commit(
    process: subprocess.Popen[str],
    timeout: float,
    description: str,
    *,
    after_commit: int = 0,
    scroll_y: float | None = None,
) -> tuple[int, int, int, float | None]:
    def probe():
        for commit in reversed(renderer_commits(process)):
            if commit[2] <= after_commit:
                continue
            if scroll_y is not None and (
                commit[3] is None or abs(commit[3] - scroll_y) > 0.01
            ):
                continue
            return commit
        return None

    return wait_for(process, timeout, description, probe)


def wait_for_quiescent_renderer_commit(
    process: subprocess.Popen[str],
    timeout: float,
    description: str,
    *,
    after_commit: int = 0,
    quiet_seconds: float = 0.4,
) -> tuple[int, int, int, float | None]:
    deadline = time.monotonic() + timeout
    candidate = None
    changed_at = time.monotonic()
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise SystemExit(
                f"Flutter shell exited before {description} ({process.returncode})\n"
                f"{process_output(process)}"
            )
        commits = renderer_commits(process)
        latest = commits[-1] if commits and commits[-1][2] > after_commit else None
        if latest != candidate:
            candidate = latest
            changed_at = time.monotonic()
        elif candidate is not None and time.monotonic() - changed_at >= quiet_seconds:
            return candidate
        time.sleep(0.05)
    raise SystemExit(
        f"timed out waiting for {description}; commits={renderer_commits(process)!r}; "
        f"process output: {process_output(process)[-8000:]}"
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
        (x, y)
        for y in (60, 75, 90, 105, 120, 135, 150, 165)
        for x in (300, 500, 700, 900)
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
        raise SystemExit(
            "native pointer scan did not focus the address field; "
            f"accessible names: {accessible_name_sample(process.pid)!r}; "
            f"process output: {process_output(process)[-4000:]}"
        )
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
    # Mozc preedit enters through IBus/GTK's real Wayland FlView IM context;
    # no AT-SPI setText or Vixen DispatchTextInput shortcut is used.
    romaji = {"306b": "ni", "3042": "a"}[codepoint]
    run_wtype(args, "-d", "150", romaji)
    time.sleep(0.5)
    run_wtype(args, "-k", "Return")


def warm_mozc(args: argparse.Namespace) -> None:
    # Noble's Mozc server starts on the first key. Prime it, cancelling that key
    # if it becomes preedit before the asserted composition begins.
    run_wtype(args, "-d", "150", "a")
    time.sleep(0.75)
    run_wtype(args, "-k", "Escape")
    time.sleep(0.25)


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
    accessible_name: str | None = None,
) -> tuple[int, int]:
    points = [] if accessible_name is None else accessible_centers(
        process.pid, accessible_name
    )
    points.extend(candidates or [
        (x, y) for x in (80, 160, 240) for y in range(105, 701, 15)
    ])
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
        f"native pointer scan did not focus {target}; "
        f"accessible_name={accessible_name!r}; points={points!r}; "
        f"status={current_status(process.pid)!r}; "
        f"process output: {process_output(process)[-8000:]}"
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
        application_command(args),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    output_lines: deque[str] = deque(maxlen=2048)
    output_lock = threading.Lock()

    def collect_output() -> None:
        if process.stdout is None:
            return
        for line in process.stdout:
            with output_lock:
                output_lines.append(line[-4096:])

    output_thread = threading.Thread(target=collect_output, daemon=True)
    setattr(process, "_vixen_output_lines", output_lines)
    setattr(process, "_vixen_output_lock", output_lock)
    output_thread.start()
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
        initial_commit = wait_for_renderer_commit(
            process, 10, "initial zero-offset Flutter renderer commit", scroll_y=0
        )

        activate_ibus_engine(args, args.ibus_engine)
        input_x, input_y = focus_with_pointer(
            args, process, "input", accessible_name="Native input"
        )
        time.sleep(1)
        warm_mozc(args)
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

        before_atspi_commit = wait_for_quiescent_renderer_commit(
            process,
            10,
            "quiescent pre-AT-SPI Flutter renderer commit",
            after_commit=initial_commit[2],
        )
        _, atspi_action, atspi_action_index, atspi_bounds = wait_for(
            process,
            10,
            "native editor AT-SPI role/state/bounds/focus action",
            lambda: named_accessible_action(process.pid, "Native editor", "Focus"),
        )
        if not atspi_action.do_action(atspi_action_index):
            raise SystemExit("native editor AT-SPI Focus action was rejected")
        atspi_status = wait_for(
            process,
            10,
            "AT-SPI focus DOM effect",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_fields(status).get("focus") == "editor"
                and status_fields(status).get("input") == input_value
                else None
            ),
        )
        after_atspi_commit = wait_for_renderer_commit(
            process,
            10,
            "AT-SPI focus Flutter renderer commit",
            after_commit=before_atspi_commit[2],
            scroll_y=before_atspi_commit[3],
        )
        if after_atspi_commit[:2] != before_atspi_commit[:2]:
            raise SystemExit(
                "AT-SPI focus commit changed context/document identity: "
                f"{before_atspi_commit!r} -> {after_atspi_commit!r}"
            )
        if status_fields(atspi_status).get("focus") != "editor":
            raise SystemExit(f"AT-SPI focus did not reach the DOM: {atspi_status}")
        time.sleep(1)
        ime_input(args, "3042")
        time.sleep(1)
        scroll_candidates = [
            (x, input_y + offset)
            for offset in range(90, 166, 5)
            for x in (input_x, max(1, input_x - 80), input_x + 80)
        ]
        scroll_x, scroll_y = focus_with_pointer(
            args,
            process,
            "scroll",
            scroll_candidates,
            accessible_name="Nested scroll area",
        )
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
        first_commit = wait_for_renderer_commit(
            process,
            10,
            "nested-wheel Flutter renderer commit",
            after_commit=initial_commit[2],
            scroll_y=initial_root,
        )

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
        canceled_commit = wait_for_renderer_commit(
            process,
            10,
            "cancelled-wheel Flutter renderer commit",
            after_commit=first_commit[2],
            scroll_y=before_cancel_root,
        )
        run_wtype(args, "c")

        run_wtype(args, "s")
        script_scroll = wait_for(
            process,
            5,
            "script root scroll effect",
            lambda: (
                status
                if (status := current_status(process.pid))
                and status_fields(status).get("cancelWheel") == "false"
                and status_number(status_fields(status), "root") > before_cancel_root
                else None
            ),
        )
        script_fields = status_fields(script_scroll)
        script_root = status_number(script_fields, "root")
        script_commit = wait_for_renderer_commit(
            process,
            10,
            "script-scroll Flutter renderer commit",
            after_commit=canceled_commit[2],
            scroll_y=script_root,
        )

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
        chained_root = status_number(chained_fields, "root")
        chained_commit = wait_for_renderer_commit(
            process,
            10,
            "root-wheel Flutter renderer commit",
            after_commit=script_commit[2],
            scroll_y=chained_root,
        )
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
            f"atspi={before_atspi_commit[2]}>{after_atspi_commit[2]}",
            f"atspiBounds={atspi_bounds!r}",
            f"commits={initial_commit[2]}>{first_commit[2]}>{canceled_commit[2]}"
            f">{script_commit[2]}>{chained_commit[2]}",
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
        output_thread.join(timeout=5)
        subprocess.run(
            [args.ibus, "engine", previous_engine or direct_engine],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
        )


if __name__ == "__main__":
    sys.exit(main())
