#!/usr/bin/env python3
"""Focused tests for the native Linux interaction harness and CI contract."""

from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path
import subprocess
import sys
import types
import unittest
from unittest import mock


sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


def load_harness() -> types.ModuleType:
    gi = types.ModuleType("gi")
    gi.require_version = lambda *_args: None
    repository = types.ModuleType("gi.repository")
    repository.Atspi = object()
    gi.repository = repository
    sys.modules["gi"] = gi
    sys.modules["gi.repository"] = repository

    path = ROOT / "scripts" / "flutter-interaction-smoke.py"
    spec = importlib.util.spec_from_file_location("flutter_interaction_smoke", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load interaction harness from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


HARNESS = load_harness()


class InteractionHarnessTests(unittest.TestCase):
    def args(self) -> argparse.Namespace:
        return argparse.Namespace(ibus="ibus", wtype="wtype")

    def test_mozc_is_the_default_ibus_engine(self) -> None:
        argv = [
            "flutter-interaction-smoke.py",
            "--app",
            "app",
            "--library",
            "library",
            "--url",
            "file:///fixture",
            "--pointer",
            "pointer",
        ]
        with mock.patch.object(sys, "argv", argv):
            self.assertEqual(HARNESS.arguments().ibus_engine, "mozc-jp")

    def test_app_viewport_rejects_unbounded_shell_values(self) -> None:
        args = self.args()
        args.app = "app"
        args.app_headless_window = False
        args.app_viewport = "1280x720;command"
        with self.assertRaisesRegex(SystemExit, "--app-viewport must be WIDTHxHEIGHT"):
            HARNESS.application_command(args)

        args.app_viewport = "4097x720"
        with self.assertRaisesRegex(SystemExit, "--app-viewport must be WIDTHxHEIGHT"):
            HARNESS.application_command(args)

    def test_app_viewport_is_forwarded_without_a_shell(self) -> None:
        args = self.args()
        args.app = "app"
        args.app_headless_window = True
        args.app_viewport = "1280x720"
        self.assertEqual(
            HARNESS.application_command(args),
            [
                str(Path("app").resolve()),
                "--vixen-headless-window",
                "--vixen-viewport=1280x720",
            ],
        )

    def test_activate_ibus_engine_waits_until_selected(self) -> None:
        active = iter(["xkb:us::eng\n", "mozc-jp\n"])

        def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            stdout = "" if len(command) == 3 else next(active)
            return subprocess.CompletedProcess(command, 0, stdout=stdout)

        with (
            mock.patch.object(HARNESS.subprocess, "run", side_effect=run) as runner,
            mock.patch.object(HARNESS.time, "sleep"),
        ):
            HARNESS.activate_ibus_engine(self.args(), "mozc-jp")

        self.assertEqual(runner.call_args_list[0].args[0], ["ibus", "engine", "mozc-jp"])
        self.assertEqual(runner.call_count, 3)

    def test_activate_ibus_engine_fails_closed(self) -> None:
        result = subprocess.CompletedProcess(
            ["ibus", "engine"], 0, stdout="xkb:us::eng\n"
        )
        with (
            mock.patch.object(HARNESS.subprocess, "run", return_value=result),
            mock.patch.object(HARNESS.time, "sleep"),
            self.assertRaisesRegex(SystemExit, "failed to activate IBus engine 'mozc-jp'"),
        ):
            HARNESS.activate_ibus_engine(self.args(), "mozc-jp")

    def test_ime_input_uses_real_romaji_preedit_and_commit(self) -> None:
        with (
            mock.patch.object(HARNESS, "run_wtype") as run_wtype,
            mock.patch.object(HARNESS.time, "sleep"),
        ):
            HARNESS.ime_input(self.args(), "3042")

        self.assertEqual(
            run_wtype.call_args_list,
            [
                mock.call(self.args(), "-d", "150", "a"),
                mock.call(self.args(), "-k", "Return"),
            ],
        )

    def test_mozc_warmup_cancels_the_server_start_preedit(self) -> None:
        with (
            mock.patch.object(HARNESS, "run_wtype") as run_wtype,
            mock.patch.object(HARNESS.time, "sleep"),
        ):
            HARNESS.warm_mozc(self.args())

        self.assertEqual(
            run_wtype.call_args_list,
            [
                mock.call(self.args(), "-d", "150", "a"),
                mock.call(self.args(), "-k", "Escape"),
            ],
        )

    def test_wait_timeout_includes_process_diagnostics(self) -> None:
        process = mock.Mock(pid=17, _vixen_output_lines=["renderer failed\n"])
        process.poll.return_value = None
        del process._vixen_output_lock
        with (
            mock.patch.object(HARNESS, "accessible_name_sample", return_value=[]),
            self.assertRaisesRegex(SystemExit, "process output: renderer failed"),
        ):
            HARNESS.wait_for(process, 0, "renderer", lambda: None)


class LinuxCiContractTests(unittest.TestCase):
    def test_noble_uses_available_accessibility_and_ime_packages(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        self.assertIn("runs-on: ubuntu-24.04", workflow)
        self.assertIn("at-spi2-core \\", workflow)
        self.assertIn("ibus-gtk3 \\", workflow)
        self.assertIn("ibus-mozc \\", workflow)
        self.assertNotIn("ibus-anthy", workflow)

    def test_noble_mozc_starts_in_hiragana_mode(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        self.assertIn('name: "mozc-jp"', workflow)
        self.assertIn("active_on_launch: True", workflow)

    def test_release_smoke_uses_drm_independent_wlroots_renderer(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        interaction_step = workflow.split(
            "- name: Native Wayland IME and nested-scroll smoke", maxsplit=1
        )[1].split("- name: Create deterministic release archive", maxsplit=1)[0]
        self.assertIn("WLR_BACKENDS=headless", interaction_step)
        self.assertIn("WLR_RENDERER=pixman", interaction_step)
        self.assertNotIn("WLR_RENDERER=gles2", interaction_step)

    def test_local_smoke_requires_the_same_ibus_engine(self) -> None:
        justfile = (ROOT / "justfile").read_text()
        recipe = justfile.split("linux-interaction-smoke:", maxsplit=1)[1].split(
            "# First R5 rendered-automation checkpoint", maxsplit=1
        )[0]
        self.assertIn("mozc-jp", recipe)
        self.assertNotIn("anthy", recipe.lower())


if __name__ == "__main__":
    unittest.main()
