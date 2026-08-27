#!/usr/bin/env python3
"""Verify Shue's project license policy and generated distribution metadata."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parent.parent
IGNORED_DIRECTORIES = {".git", ".unlazy", "licenses", "target"}
FORMER_LICENSE = "Apa" + "che"


def fail(message: str) -> None:
    raise SystemExit(f"MIT-only license policy verification failed: {message}")


def contains_former_license(value: str) -> bool:
    return FORMER_LICENSE.casefold() in value.casefold()


def first_party_text_files() -> list[Path]:
    files: list[Path] = []
    for path in ROOT.rglob("*"):
        if any(part in IGNORED_DIRECTORIES for part in path.relative_to(ROOT).parts):
            continue
        if path.is_file() and not path.is_symlink():
            try:
                path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            files.append(path)
    return sorted(files)


def load_release_helper():
    path = ROOT / "scripts/release.py"
    spec = importlib.util.spec_from_file_location("shue_release_policy", path)
    if spec is None or spec.loader is None:
        fail("unable to load release helper")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main() -> int:
    positive_control = "MIT OR " + FORMER_LICENSE + "-2.0"
    if not contains_former_license(positive_control):
        fail("former-license absence checker failed its positive control")
    if contains_former_license("MIT License"):
        fail("former-license absence checker rejected its negative control")

    violations = [
        path.relative_to(ROOT).as_posix()
        for path in first_party_text_files()
        if contains_former_license(path.read_text(encoding="utf-8"))
    ]
    if violations:
        fail(f"former project-license references remain in {violations!r}")

    root_license_files = sorted(path.name for path in ROOT.glob("LICENSE*") if path.is_file())
    if root_license_files != ["LICENSE-MIT"]:
        fail(f"unexpected top-level license files: {root_license_files!r}")

    with (ROOT / "Cargo.toml").open("rb") as manifest:
        workspace = tomllib.load(manifest)["workspace"]
    if workspace.get("package", {}).get("license") != "MIT":
        fail("workspace.package.license is not exactly MIT")
    for manifest_path in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        with manifest_path.open("rb") as manifest:
            package = tomllib.load(manifest)["package"]
        if package.get("license") != {"workspace": True}:
            fail(f"{manifest_path.relative_to(ROOT)} does not inherit the workspace license")

    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    if "Shue is licensed under the MIT License. See `LICENSE-MIT`." not in readme:
        fail("README does not declare the MIT-only project license")

    release = load_release_helper()
    expected_archive_files = (
        "README.md",
        "LICENSE-MIT",
        "THIRD-PARTY-LICENSES.txt",
    )
    if release.ARCHIVE_FILES != expected_archive_files:
        fail(f"release archive license members are {release.ARCHIVE_FILES!r}")
    checksums = {
        release.archive_name("1.2.3", target): "a" * 64 for target in release.TARGETS
    }
    formula = release.formula_text("owner/shue", "1.2.3", checksums)
    if '  license "MIT"' not in formula:
        fail("generated Homebrew formula does not declare MIT")
    if 'doc.install "LICENSE-MIT", "THIRD-PARTY-LICENSES.txt"' not in formula:
        fail("generated Homebrew formula does not install the MIT license and notices")
    if contains_former_license(formula):
        fail("generated Homebrew formula retains the former project license")

    print("MIT-only license policy verification passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
