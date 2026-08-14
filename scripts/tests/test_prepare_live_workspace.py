from __future__ import annotations

import importlib.util
from collections import namedtuple
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "prepare_live_workspace", ROOT / "scripts" / "ci" / "prepare_live_workspace.py"
)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)
prepare_workspace = MODULE.prepare_workspace
GIB = MODULE.GIB
DiskUsage = namedtuple("DiskUsage", "total used free")


class PrepareLiveWorkspaceTests(unittest.TestCase):
    def test_runner_temp_default_is_lane_scoped(self) -> None:
        with tempfile.TemporaryDirectory() as temp, tempfile.TemporaryDirectory() as workspace:
            with mock.patch.object(
                MODULE.shutil,
                "disk_usage",
                return_value=DiskUsage(total=20 * GIB, used=1, free=19 * GIB),
            ):
                live = prepare_workspace(
                    lane="streaming",
                    root_input=None,
                    runner_temp=temp,
                    workspace=workspace,
                    minimum_free_gib=0,
                    home=None,
                )
            self.assertEqual(live.root, Path(temp).resolve() / "griffr-live-e2e" / "streaming")
            self.assertEqual(live.minimum_free_bytes, 6 * GIB)

    def test_explicit_operator_floor_overrides_lane_default(self) -> None:
        with tempfile.TemporaryDirectory() as temp, tempfile.TemporaryDirectory() as workspace:
            with mock.patch.object(
                MODULE.shutil,
                "disk_usage",
                return_value=DiskUsage(total=20 * GIB, used=1, free=19 * GIB),
            ):
                live = prepare_workspace(
                    lane="archive-sample",
                    root_input=temp,
                    runner_temp=None,
                    workspace=workspace,
                    minimum_free_gib=9,
                    home=None,
                )
            self.assertEqual(live.minimum_free_bytes, 9 * GIB)

    def test_low_space_fails_before_live_io(self) -> None:
        with tempfile.TemporaryDirectory() as temp, tempfile.TemporaryDirectory() as workspace:
            with mock.patch.object(
                MODULE.shutil,
                "disk_usage",
                return_value=DiskUsage(total=8 * GIB, used=5 * GIB, free=3 * GIB),
            ):
                with self.assertRaisesRegex(RuntimeError, "at least 6 GiB free"):
                    prepare_workspace(
                        lane="streaming",
                        root_input=temp,
                        runner_temp=None,
                        workspace=workspace,
                        minimum_free_gib=0,
                        home=None,
                    )

    def test_workspace_and_descendants_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as workspace:
            nested = Path(workspace) / "live"
            with self.assertRaisesRegex(ValueError, "separate from the checked-out workspace"):
                prepare_workspace(
                    lane="lifecycle",
                    root_input=str(nested),
                    runner_temp=None,
                    workspace=workspace,
                    minimum_free_gib=0,
                    home=None,
                )

    def test_parent_of_workspace_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            workspace = Path(temp) / "repo"
            workspace.mkdir()
            with self.assertRaisesRegex(ValueError, "separate from the checked-out workspace"):
                prepare_workspace(
                    lane="lifecycle",
                    root_input=temp,
                    runner_temp=None,
                    workspace=str(workspace),
                    minimum_free_gib=0,
                    home=None,
                )


if __name__ == "__main__":
    unittest.main()
