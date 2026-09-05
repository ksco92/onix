"""Checks that a built wheel actually ships the type stub and py.typed marker.

Builds a real release wheel with maturin — the same `--release` publish.yml
uses — into a scratch directory and inspects its zip contents directly,
rather than trusting that maturin picking up ``deepdiff_rs.pyi`` at develop
time also means it is packaged into what actually ships.
"""

import shutil
import subprocess
import zipfile
from pathlib import Path

import pytest

CRATE_ROOT = Path(__file__).resolve().parent.parent


@pytest.fixture(scope="module")
def wheel_path(tmp_path_factory: pytest.TempPathFactory) -> Path:
    maturin = shutil.which("maturin")
    if maturin is None:
        pytest.skip("maturin not on PATH")

    out_dir = tmp_path_factory.mktemp("wheel_contents")
    subprocess.run(
        [maturin, "build", "--release", "--out", str(out_dir)],
        cwd=CRATE_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    (wheel,) = out_dir.glob("*.whl")
    return wheel


def test_wheel_ships_the_stub_and_py_typed_marker(wheel_path: Path) -> None:
    with zipfile.ZipFile(wheel_path) as wheel:
        names = set(wheel.namelist())

    assert "deepdiff_rs/__init__.pyi" in names
    assert "deepdiff_rs/py.typed" in names


def test_wheel_stub_content_matches_the_source_stub(wheel_path: Path) -> None:
    source = (CRATE_ROOT / "deepdiff_rs.pyi").read_text()
    with zipfile.ZipFile(wheel_path) as wheel:
        packaged = wheel.read("deepdiff_rs/__init__.pyi").decode()

    assert packaged == source
