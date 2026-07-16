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
        release_steps = workflow.split(
            "- name: Native Wayland IME and nested-scroll smoke", maxsplit=1
        )[1].split("- name: Upload Linux release assets", maxsplit=1)[0]
        self.assertEqual(release_steps.count("WLR_BACKENDS=headless"), 2)
        self.assertEqual(release_steps.count("WLR_RENDERER=pixman"), 2)
        self.assertNotIn("WLR_RENDERER=gles2", release_steps)

    def test_gtk4_release_uses_isolated_build_directory(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        release_job = workflow.split("linux-release:", maxsplit=1)[1].split(
            "release:", maxsplit=1
        )[0]
        self.assertIn("build/linux-gtk4/x64/release/bundle", release_job)
        self.assertNotIn("build/linux/x64/release/bundle", release_job)

        hello_pubspec = (
            ROOT / "fixtures" / "artifact-size" / "flutter_hello" / "pubspec.yaml"
        ).read_text()
        self.assertIn("linux-gtk-default: gtk4", hello_pubspec)

    def test_gtk4_release_installs_pinned_engine_artifact(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        release_job = workflow.split("linux-release:", maxsplit=1)[1].split(
            "release:", maxsplit=1
        )[0]
        self.assertIn("scripts/install_flutter_gtk4_sdk.py", release_job)
        self.assertIn("328b829d35a3a5d7a00e0c2f0e97eb8cc0d97188", release_job)
        self.assertIn("fc1ad955f16467c959e3cd8079b760d5af0984aa", release_job)
        self.assertNotIn("http:flutter-beta", release_job)

        installer = (ROOT / "scripts" / "install_flutter_gtk4_sdk.py").read_text()
        self.assertIn("github.com/adonm/flutter-dev/releases/download", installer)
        self.assertIn("flutter-engine-gtk4-", installer)
        self.assertIn(
            "61cafba174d24e2c4f73e416cb98c0b33a0ca751b99bf0d9c42cf2c4f1f44add",
            installer,
        )
        self.assertNotIn("__CI_LIBRARY_SHA256__", installer)
        self.assertIn("linux-x64-release", installer)
        self.assertIn("libflutter_linux_gtk4.so", installer)
        self.assertIn("libgtk-4.so.1", installer)
        self.assertIn("libgtk-3.so.0", installer)

        for script in ("flutter-at-spi-smoke.py", "flutter-interaction-smoke.py"):
            smoke = (ROOT / "scripts" / script).read_text()
            self.assertIn('"GTK_A11Y": "atspi"', smoke)
            self.assertNotIn('"GTK_A11Y": "1"', smoke)

    def test_local_smoke_requires_the_same_ibus_engine(self) -> None:
        justfile = (ROOT / "justfile").read_text()
        recipe = justfile.split("linux-interaction-smoke:", maxsplit=1)[1].split(
            "# First R5 rendered-automation checkpoint", maxsplit=1
        )[0]
        self.assertIn("mozc-jp", recipe)
        self.assertNotIn("anthy", recipe.lower())


if __name__ == "__main__":
    unittest.main()
