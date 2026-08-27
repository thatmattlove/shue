#!/usr/bin/env python3
"""Build-release helpers shared by GitHub Actions and local verification."""

from __future__ import annotations

import argparse
import functools
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tomllib


ROOT = Path(__file__).resolve().parent.parent
NUMERIC_IDENTIFIER = r"(?:0|[1-9][0-9]*)"
PRERELEASE_IDENTIFIER = rf"(?:{NUMERIC_IDENTIFIER}|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
SEMVER = re.compile(
    rf"^{NUMERIC_IDENTIFIER}\."
    rf"{NUMERIC_IDENTIFIER}\."
    rf"{NUMERIC_IDENTIFIER}"
    rf"(?:-{PRERELEASE_IDENTIFIER}(?:\.{PRERELEASE_IDENTIFIER})*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
)
ARCHITECTURE_MARKERS = {
    "aarch64-apple-darwin": ("arm64",),
    "x86_64-apple-darwin": ("x86_64",),
    "aarch64-unknown-linux-musl": ("aarch64", "arm64"),
    "x86_64-unknown-linux-musl": ("x86-64", "x86_64"),
}
ARCHIVE_FILES = (
    "README.md",
    "LICENSE-MIT",
    "THIRD-PARTY-LICENSES.txt",
)
SOURCE_ARCHIVE_FILES = ARCHIVE_FILES[:-1]
LICENSE_FILE = re.compile(
    r"^(?:LICENSE|LICENCE|COPYING|COPYRIGHT|UNLICENSE|NOTICE)(?:[-._].*)?$",
    re.IGNORECASE,
)
PINNED_NOTICES = (
    (
        "PCRE2 10.46 consolidated license",
        "licenses/PCRE2-10.46-LICENCE.md",
        "9cf7ac6976099a1d856826d3ef1b093bd6b84489dc6100628ac79e740cf9885a",
    ),
    (
        "musl 1.2.3 COPYRIGHT",
        "licenses/MUSL-1.2.3-COPYRIGHT",
        "f9bc4423732350eb0b3f7ed7e91d530298476f8fec0c6c427a1c04ade22655af",
    ),
)
RUST_1_85_COPYRIGHT_SHA256 = (
    "252b04f034f7383e09401dc700c8b933e3e38818eaa8b2f09dd247542be92292"
)
FORMER_LICENSE_FILE = "LICENSE-A" + "PACHE"
LICENSE_DELEGATES = {
    "winapi-i686-pc-windows-gnu": (
        "winapi",
        frozenset({"LICENSE-MIT", FORMER_LICENSE_FILE}),
    ),
    "winapi-x86_64-pc-windows-gnu": (
        "winapi",
        frozenset({"LICENSE-MIT", FORMER_LICENSE_FILE}),
    ),
}


class ReleaseError(RuntimeError):
    """A release invariant was not satisfied."""


def workspace_version(root: Path = ROOT) -> str:
    with (root / "Cargo.toml").open("rb") as manifest:
        data = tomllib.load(manifest)
    try:
        version = data["workspace"]["package"]["version"]
    except (KeyError, TypeError) as error:
        raise ReleaseError("Cargo.toml has no workspace.package.version") from error
    if not isinstance(version, str) or not SEMVER.fullmatch(version):
        raise ReleaseError(f"workspace version is not valid SemVer: {version!r}")
    return version


def workspace_rust_version(root: Path = ROOT) -> str:
    with (root / "Cargo.toml").open("rb") as manifest:
        data = tomllib.load(manifest)
    try:
        version = data["workspace"]["package"]["rust-version"]
    except (KeyError, TypeError) as error:
        raise ReleaseError("Cargo.toml has no workspace.package.rust-version") from error
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", version):
        raise ReleaseError(f"workspace rust-version is invalid: {version!r}")
    return version


def validate_version(version: str) -> str:
    if not SEMVER.fullmatch(version):
        raise ReleaseError(f"invalid SemVer version: {version!r}")
    return version


def validate_target(target: str) -> str:
    if target not in TARGETS:
        raise ReleaseError(
            f"unsupported release target {target!r}; expected one of {', '.join(TARGETS)}"
        )
    return target


def archive_name(version: str, target: str) -> str:
    return f"shue-v{validate_version(version)}-{validate_target(target)}.tar.gz"


def validate_tag(tag: str, root: Path = ROOT) -> str:
    version = workspace_version(root)
    expected = f"v{version}"
    if tag != expected:
        raise ReleaseError(
            f"release tag {tag!r} does not match workspace version; expected {expected!r}"
        )
    return version


def run_output(command: list[str]) -> str:
    try:
        result = subprocess.run(
            command,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        output = getattr(error, "stdout", "") or ""
        detail = f": {output.strip()}" if output.strip() else ""
        raise ReleaseError(f"command failed: {' '.join(command)}{detail}") from error
    return result.stdout


def verify_binary(binary: Path, target: str, version: str) -> None:
    target = validate_target(target)
    version = validate_version(version)
    if not binary.is_file():
        raise ReleaseError(f"release binary does not exist: {binary}")
    if not os.access(binary, os.X_OK):
        raise ReleaseError(f"release binary is not executable: {binary}")

    # `file` normally prefixes its description with the input path. Using `-b`
    # prevents a target directory such as `aarch64-apple-darwin` from making a
    # wrong-architecture binary look valid.
    description = run_output(["file", "-b", str(binary)]).strip()
    if not any(marker in description.lower() for marker in ARCHITECTURE_MARKERS[target]):
        raise ReleaseError(
            f"binary architecture does not match {target}: {description}"
        )

    if target.endswith("apple-darwin"):
        linkage = run_output(["otool", "-L", str(binary)])
        forbidden = ("libpcre2", "/opt/homebrew/", "/usr/local/")
        matches = [value for value in forbidden if value in linkage]
        if matches:
            raise ReleaseError(
                "macOS binary has non-portable dynamic linkage "
                f"({', '.join(matches)}):\n{linkage.rstrip()}"
            )
    else:
        static_markers = ("statically linked", "static-pie linked")
        if not any(marker in description.lower() for marker in static_markers):
            raise ReleaseError(f"Linux release is not statically linked: {description}")
        program_headers = run_output(["readelf", "-l", str(binary)])
        if re.search(r"^\s*INTERP\s", program_headers, re.MULTILINE):
            raise ReleaseError("Linux release unexpectedly contains a dynamic interpreter")

    actual_version = run_output([str(binary), "--version"]).strip()
    expected_version = f"shue {version}"
    if actual_version != expected_version:
        raise ReleaseError(
            f"binary reported {actual_version!r}; expected {expected_version!r}"
        )


def normalized_tarinfo(name: str, mode: int, size: int, epoch: int) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.mode = mode
    info.size = size
    info.mtime = epoch
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    return info


def cargo_metadata(root: Path) -> dict:
    try:
        result = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--quiet"],
            cwd=root,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        detail = (getattr(error, "stderr", "") or "").strip()
        suffix = f": {detail}" if detail else ""
        raise ReleaseError(f"cargo metadata failed{suffix}") from error
    try:
        metadata = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ReleaseError("cargo metadata returned invalid JSON") from error
    if not isinstance(metadata, dict) or not isinstance(metadata.get("packages"), list):
        raise ReleaseError("cargo metadata returned an unexpected structure")
    return metadata


def normal_dependencies(metadata: dict) -> list[dict]:
    """Return every registry package reachable through a normal dependency edge."""

    packages = {package["id"]: package for package in metadata["packages"]}
    resolve = metadata.get("resolve")
    if not isinstance(resolve, dict) or not isinstance(resolve.get("nodes"), list):
        raise ReleaseError("cargo metadata did not include a dependency graph")
    nodes = {node["id"]: node for node in resolve["nodes"]}
    roots = [
        package["id"]
        for package in metadata["packages"]
        if package.get("name") == "shue" and package.get("source") is None
    ]
    if len(roots) != 1:
        raise ReleaseError("unable to identify the shue package in cargo metadata")

    visited: set[str] = set()
    pending = roots
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        node = nodes.get(package_id)
        if node is None:
            raise ReleaseError(f"cargo metadata is missing dependency node {package_id!r}")
        for dependency in node.get("deps", []):
            kinds = dependency.get("dep_kinds", [])
            if any(kind.get("kind") is None for kind in kinds):
                pending.append(dependency["pkg"])

    dependencies = [
        packages[package_id]
        for package_id in visited
        if packages[package_id].get("source") is not None
    ]
    return sorted(dependencies, key=lambda package: (package["name"], package["version"]))


def package_license_files(package: dict) -> list[Path]:
    package_root = Path(package["manifest_path"]).parent
    return sorted(
        (
            path
            for path in package_root.iterdir()
            if path.is_file() and LICENSE_FILE.fullmatch(path.name)
        ),
        key=lambda path: path.name.casefold(),
    )


def delegated_license_files(
    package: dict, dependencies: list[dict]
) -> tuple[dict, list[Path]]:
    """Return exact terms from a reviewed companion package or fail closed."""

    delegation = LICENSE_DELEGATES.get(package["name"])
    if delegation is None:
        raise ReleaseError(f"{package['name']} has no license files or reviewed delegate")
    delegate_name, required_files = delegation
    candidates = [
        candidate for candidate in dependencies if candidate["name"] == delegate_name
    ]
    if len(candidates) != 1:
        raise ReleaseError(
            f"{package['name']} license delegate {delegate_name!r} is not unique"
        )
    delegate = candidates[0]
    repository = package.get("repository")
    if not repository or delegate.get("repository") != repository:
        raise ReleaseError(
            f"{package['name']} and license delegate {delegate_name} have different repositories"
        )
    declared = package.get("license")
    if not declared or delegate.get("license") != declared:
        raise ReleaseError(
            f"{package['name']} and license delegate {delegate_name} have different terms"
        )
    available = {path.name: path for path in package_license_files(delegate)}
    missing = required_files - available.keys()
    if missing:
        raise ReleaseError(
            f"{package['name']} license delegate {delegate_name} is missing {sorted(missing)!r}"
        )
    return delegate, [available[name] for name in sorted(required_files)]


@functools.lru_cache(maxsize=None)
def third_party_licenses(root: Path) -> bytes:
    """Collect exact license files for all normal Rust dependencies and bundled PCRE2."""

    sections = [
        "THIRD-PARTY LICENSES AND NOTICES\n",
        "Shue itself is licensed under MIT; its top-level LICENSE-MIT file accompanies "
        "this notice. The terms reproduced below apply to third-party dependencies "
        "and runtime components included in the distribution.\n",
    ]
    dependencies = normal_dependencies(cargo_metadata(root))
    if not dependencies:
        raise ReleaseError("no third-party dependencies found for license collection")

    found_pcre2 = False
    for package in dependencies:
        package_root = Path(package["manifest_path"]).parent
        license_files = package_license_files(package)
        declared = package.get("license") or package.get("license_file") or "unspecified"
        sections.append("=" * 78 + "\n")
        sections.append(f"Package: {package['name']} {package['version']}\n")
        sections.append(f"Declared license: {declared}\n")
        if not license_files:
            delegate, license_files = delegated_license_files(package, dependencies)
            sections.append(
                "License files: reproduced from reviewed companion package "
                f"{delegate['name']} {delegate['version']}.\n"
            )
        for path in license_files:
            contents = path.read_text(encoding="utf-8").strip()
            sections.append(f"\n--- {path.name} ---\n{contents}\n")

        if package["name"] == "pcre2-sys":
            found_pcre2 = True
            version_header = package_root / "upstream/include/pcre2.h"
            header_contents = version_header.read_text(encoding="utf-8")
            if not (
                re.search(r"^#define PCRE2_MAJOR\s+10$", header_contents, re.MULTILINE)
                and re.search(r"^#define PCRE2_MINOR\s+46$", header_contents, re.MULTILINE)
            ):
                raise ReleaseError(
                    "bundled PCRE2 version changed; refresh its pinned consolidated license"
                )
            source = package_root / "upstream/src/pcre2_compile.c"
            if not source.is_file():
                raise ReleaseError("pcre2-sys does not contain its bundled PCRE2 source")
            contents = source.read_text(encoding="utf-8")
            notices = re.findall(r"/\*.*?\*/", contents, re.DOTALL)
            match = next(
                (
                    notice
                    for notice in notices
                    if "University of Cambridge" in notice
                    and "Redistribution and use" in notice
                ),
                None,
            )
            if match is None:
                raise ReleaseError("unable to extract the bundled PCRE2 license notice")
            sections.append(
                "\n--- Bundled PCRE2 license notice "
                "(upstream/src/pcre2_compile.c) ---\n"
            )
            sections.append(match.strip() + "\n")

            sljit_source = (
                package_root
                / "upstream/deps/sljit/sljit_src/sljitLir.c"
            )
            if not sljit_source.is_file():
                raise ReleaseError("pcre2-sys does not contain its bundled SLJIT source")
            sljit_contents = sljit_source.read_text(encoding="utf-8")
            sljit_match = re.match(r"/\*.*?\*/", sljit_contents, re.DOTALL)
            if sljit_match is None or "Zoltan Herczeg" not in sljit_match.group(0):
                raise ReleaseError("unable to extract the bundled SLJIT license notice")
            sections.append(
                "\n--- Bundled SLJIT license notice "
                "(upstream/deps/sljit/sljit_src/sljitLir.c) ---\n"
            )
            sections.append(sljit_match.group(0).strip() + "\n")

    if not found_pcre2:
        raise ReleaseError("pcre2-sys is missing from the release dependency graph")

    for title, relative_path, expected_digest in PINNED_NOTICES:
        notice_path = root / relative_path
        if not notice_path.is_file():
            raise ReleaseError(f"pinned notice is missing: {notice_path}")
        notice = notice_path.read_bytes()
        digest = hashlib.sha256(notice).hexdigest()
        if digest != expected_digest:
            raise ReleaseError(
                f"{title} changed (expected SHA-256 {expected_digest}, found {digest})"
            )
        sections.append("=" * 78 + "\n")
        sections.append(f"Bundled component: {title}\n")
        sections.append(
            f"\n--- {Path(relative_path).name} ---\n"
            + notice.decode("utf-8").strip()
            + "\n"
        )

    rust_version = workspace_rust_version(root)
    exact_rust_version = rust_version if rust_version.count(".") == 2 else f"{rust_version}.0"
    sysroot = Path(
        run_output(["rustc", f"+{exact_rust_version}", "--print", "sysroot"]).strip()
    )
    rust_copyright = sysroot / "share/doc/rust/COPYRIGHT"
    if not rust_copyright.is_file():
        raise ReleaseError(
            f"Rust {rust_version} COPYRIGHT is missing from toolchain: {rust_copyright}"
        )
    rust_notice = rust_copyright.read_bytes()
    rust_digest = hashlib.sha256(rust_notice).hexdigest()
    if exact_rust_version == "1.85.0" and rust_digest != RUST_1_85_COPYRIGHT_SHA256:
        raise ReleaseError(
            "Rust 1.85.0 COPYRIGHT does not match the reviewed toolchain notice"
        )
    sections.append("=" * 78 + "\n")
    sections.append(f"Runtime: Rust {exact_rust_version}\n")
    sections.append(
        "\n--- Rust toolchain COPYRIGHT ---\n"
        + rust_notice.decode("utf-8").strip()
        + "\n"
    )

    result = "\n".join(section.rstrip("\n") for section in sections).rstrip() + "\n"
    return result.encode("utf-8")


def create_archive(
    *,
    root: Path,
    binary: Path,
    target: str,
    version: str,
    output_directory: Path,
) -> Path:
    target = validate_target(target)
    version = validate_version(version)
    epoch_text = os.environ.get("SOURCE_DATE_EPOCH", "0")
    try:
        epoch = int(epoch_text)
    except ValueError as error:
        raise ReleaseError(f"SOURCE_DATE_EPOCH is not an integer: {epoch_text!r}") from error
    if epoch < 0:
        raise ReleaseError("SOURCE_DATE_EPOCH cannot be negative")

    input_paths = {"shue": binary}
    input_paths.update({name: root / name for name in SOURCE_ARCHIVE_FILES})
    for name, path in input_paths.items():
        if not path.is_file():
            raise ReleaseError(f"archive input {name!r} does not exist: {path}")
    inputs = {name: path.read_bytes() for name, path in input_paths.items()}
    inputs["THIRD-PARTY-LICENSES.txt"] = third_party_licenses(root)

    output_directory.mkdir(parents=True, exist_ok=True)
    output = output_directory / archive_name(version, target)
    top = output.name.removesuffix(".tar.gz")
    with output.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                directory = normalized_tarinfo(top, 0o755, 0, epoch)
                directory.type = tarfile.DIRTYPE
                archive.addfile(directory)
                for name, contents in inputs.items():
                    mode = 0o755 if name == "shue" else 0o644
                    info = normalized_tarinfo(f"{top}/{name}", mode, len(contents), epoch)
                    archive.addfile(info, io.BytesIO(contents))
    return output


def inspect_archive(path: Path, version: str, target: str) -> None:
    expected_name = archive_name(version, target)
    if path.name != expected_name:
        raise ReleaseError(f"unexpected archive name {path.name!r}; expected {expected_name!r}")
    top = expected_name.removesuffix(".tar.gz")
    expected = {
        top,
        f"{top}/shue",
        *(f"{top}/{name}" for name in ARCHIVE_FILES),
    }
    with tarfile.open(path, "r:gz") as archive:
        members = archive.getmembers()
        names = {member.name for member in members}
        if names != expected or len(members) != len(expected):
            raise ReleaseError(
                f"archive {path.name} contains {sorted(names)!r}; expected {sorted(expected)!r}"
            )
        by_name = {member.name: member for member in members}
        if not by_name[top].isdir():
            raise ReleaseError(f"archive root is not a directory in {path.name}")
        for name in expected - {top}:
            if not by_name[name].isfile():
                raise ReleaseError(f"archive member is not a regular file: {name}")
        if by_name[f"{top}/shue"].mode & 0o111 == 0:
            raise ReleaseError(f"archive binary is not executable in {path.name}")
        if by_name[f"{top}/shue"].size == 0:
            raise ReleaseError(f"archive binary is empty in {path.name}")


def write_checksums(directory: Path, version: str) -> Path:
    version = validate_version(version)
    expected = {archive_name(version, target): target for target in TARGETS}
    actual = {path.name: path for path in directory.glob("*.tar.gz")}
    if set(actual) != set(expected):
        missing = sorted(set(expected) - set(actual))
        extra = sorted(set(actual) - set(expected))
        raise ReleaseError(f"release archives mismatch; missing={missing!r}, extra={extra!r}")

    lines = []
    for name in sorted(expected):
        path = actual[name]
        inspect_archive(path, version, expected[name])
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        lines.append(f"{digest}  {name}\n")
    output = directory / "SHA256SUMS"
    output.write_text("".join(lines), encoding="utf-8")
    return output


def read_checksums(path: Path, version: str) -> dict[str, str]:
    expected = {archive_name(version, target) for target in TARGETS}
    checksums: dict[str, str] = {}
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        fields = line.split()
        if len(fields) != 2:
            raise ReleaseError(f"malformed checksum line {line_number} in {path}")
        digest, name = fields
        name = name.removeprefix("*")
        if not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ReleaseError(f"invalid SHA-256 on line {line_number} in {path}")
        if name in checksums:
            raise ReleaseError(f"duplicate checksum for {name!r} in {path}")
        checksums[name] = digest
    if set(checksums) != expected:
        missing = sorted(expected - set(checksums))
        extra = sorted(set(checksums) - expected)
        raise ReleaseError(f"checksums mismatch; missing={missing!r}, extra={extra!r}")
    return checksums


def semver_precedence(version: str) -> tuple[tuple[int, int, int], tuple | None]:
    """Return a comparison key implementing SemVer precedence (build data excluded)."""

    version = validate_version(version)
    without_build = version.split("+", 1)[0]
    core, separator, prerelease = without_build.partition("-")
    major, minor, patch = (int(value) for value in core.split("."))
    if not separator:
        return (major, minor, patch), None
    identifiers = tuple(
        (0, int(value)) if value.isdigit() else (1, value)
        for value in prerelease.split(".")
    )
    return (major, minor, patch), identifiers


def compare_versions(left: str, right: str) -> int:
    """Compare two valid SemVer strings by precedence, ignoring build metadata."""

    left_core, left_pre = semver_precedence(left)
    right_core, right_pre = semver_precedence(right)
    if left_core != right_core:
        return -1 if left_core < right_core else 1
    if left_pre is None or right_pre is None:
        if left_pre is right_pre:
            return 0
        return 1 if left_pre is None else -1
    for left_identifier, right_identifier in zip(left_pre, right_pre):
        if left_identifier == right_identifier:
            continue
        left_kind, left_value = left_identifier
        right_kind, right_value = right_identifier
        if left_kind != right_kind:
            return -1 if left_kind < right_kind else 1
        return -1 if left_value < right_value else 1
    if len(left_pre) == len(right_pre):
        return 0
    return -1 if len(left_pre) < len(right_pre) else 1


def formula_version(path: Path) -> str:
    if path.is_symlink() or not path.is_file():
        raise ReleaseError(f"Homebrew formula does not exist: {path}")
    source = path.read_text(encoding="utf-8")
    versions: set[str] = set()
    for target in TARGETS:
        pattern = re.compile(
            rf'releases/download/v([^/"\s]+)/shue-v([^/"\s]+)-{re.escape(target)}\.tar\.gz'
        )
        matches = pattern.findall(source)
        if len(matches) != 1:
            raise ReleaseError(
                f"Homebrew formula must contain exactly one canonical URL for {target}"
            )
        tag_version, archive_version = matches[0]
        if tag_version != archive_version:
            raise ReleaseError(
                f"Homebrew formula tag/archive versions disagree for {target}"
            )
        versions.add(validate_version(tag_version))
    if len(versions) != 1:
        raise ReleaseError("Homebrew formula URLs do not use one consistent version")
    version = versions.pop()

    explicit = re.findall(r'^\s*version\s+"([^"]+)"\s*$', source, re.MULTILINE)
    if len(explicit) > 1:
        raise ReleaseError("Homebrew formula contains multiple explicit versions")
    if explicit and validate_version(explicit[0]) != version:
        raise ReleaseError("Homebrew formula explicit version disagrees with its URLs")
    return version


def check_tap_update(candidate: Path, current: Path, expected_version: str) -> str:
    """Reject tap downgrades and non-idempotent replacements of one release."""

    expected_version = validate_version(expected_version)
    candidate_version = formula_version(candidate)
    if candidate_version != expected_version:
        raise ReleaseError(
            f"candidate formula is {candidate_version}, expected {expected_version}"
        )
    _, candidate_prerelease = semver_precedence(candidate_version)
    if candidate_prerelease is not None:
        raise ReleaseError(
            f"refusing to publish prerelease {candidate_version} to the stable tap"
        )
    if not current.exists():
        return "first release"
    current_version = formula_version(current)
    comparison = compare_versions(candidate_version, current_version)
    if comparison < 0:
        raise ReleaseError(
            f"refusing Homebrew tap downgrade from {current_version} to {candidate_version}"
        )
    if comparison == 0:
        if candidate_version != current_version:
            raise ReleaseError(
                "refusing ambiguous Homebrew update between equal-precedence versions "
                f"{current_version} and {candidate_version}"
            )
        if candidate.read_bytes() != current.read_bytes():
            raise ReleaseError(
                f"refusing to replace existing Homebrew formula for {candidate_version}"
            )
        return "already current"
    return f"upgrade from {current_version} to {candidate_version}"


def validate_bundle(directory: Path, version: str, require_formula: bool) -> None:
    """Require an exact, internally consistent release-asset set."""

    version = validate_version(version)
    expected = {archive_name(version, target) for target in TARGETS}
    expected.add("SHA256SUMS")
    if require_formula:
        expected.add("shue.rb")
    if not directory.is_dir():
        raise ReleaseError(f"release bundle directory does not exist: {directory}")
    children = list(directory.iterdir())
    if any(not path.is_file() or path.is_symlink() for path in children):
        raise ReleaseError("release bundle may contain only regular files")
    actual = {path.name for path in children}
    if actual != expected:
        raise ReleaseError(
            "release bundle contents mismatch; "
            f"missing={sorted(expected - actual)!r}, extra={sorted(actual - expected)!r}"
        )

    checksums = read_checksums(directory / "SHA256SUMS", version)
    for name, digest in checksums.items():
        archive = directory / name
        target = name.removeprefix(f"shue-v{version}-").removesuffix(".tar.gz")
        inspect_archive(archive, version, target)
        if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
            raise ReleaseError(f"checksum does not match release archive {name}")
    if require_formula:
        formula = directory / "shue.rb"
        if formula_version(formula) != version:
            raise ReleaseError("Homebrew formula version does not match release bundle")
        formula_source = formula.read_text(encoding="utf-8")
        for name, digest in checksums.items():
            if name not in formula_source or digest not in formula_source:
                raise ReleaseError(f"Homebrew formula is missing {name} or its checksum")


def formula_text(repository: str, version: str, checksums: dict[str, str]) -> str:
    if not REPOSITORY.fullmatch(repository):
        raise ReleaseError(
            f"invalid GitHub repository {repository!r}; expected an owner/name pair"
        )
    version = validate_version(version)

    def stanza(target: str, indent: str = "    ") -> str:
        name = archive_name(version, target)
        url = f"https://github.com/{repository}/releases/download/v{version}/{name}"
        return f'{indent}url "{url}"\n{indent}sha256 "{checksums[name]}"'

    # Homebrew's URL parser drops SemVer build metadata, so retain an explicit
    # version only when it is needed. Ordinary versions intentionally rely on
    # URL detection to satisfy `brew audit`'s redundant-version rule.
    version_stanza = f'  version "{version}"\n' if "+" in version else ""

    return f'''class Shue < Formula
  desc "Fast PCRE2 terminal highlighting with transparent SSH passthrough"
  homepage "https://github.com/{repository}"
{version_stanza}  license "MIT"

  on_macos do
    on_arm do
{stanza("aarch64-apple-darwin", "      ")}
    end

    on_intel do
{stanza("x86_64-apple-darwin", "      ")}
    end
  end

  on_linux do
    on_arm do
{stanza("aarch64-unknown-linux-musl", "      ")}
    end

    on_intel do
{stanza("x86_64-unknown-linux-musl", "      ")}
    end
  end

  def install
    bin.install "shue"
    doc.install "LICENSE-MIT", "THIRD-PARTY-LICENSES.txt"
  end

  test do
    assert_equal "homebrew\\n", pipe_output("#{{bin}}/shue --no-color --filter", "homebrew\\n", 0)
    assert_equal "shue #{{version}}", shell_output("#{{bin}}/shue --version").strip
  end
end
'''


def write_formula(
    *, repository: str, version: str, checksums_path: Path, output: Path
) -> Path:
    checksums = read_checksums(checksums_path, version)
    contents = formula_text(repository, version, checksums)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(contents, encoding="utf-8")
    return output


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    version = subparsers.add_parser("version", help="validate a tag and print its version")
    version.add_argument("--tag", required=True)

    verify = subparsers.add_parser("verify", help="verify a built release binary")
    verify.add_argument("--target", required=True, choices=TARGETS)
    verify.add_argument("--version", required=True)
    verify.add_argument("--binary", type=Path)

    package = subparsers.add_parser("package", help="create one release archive")
    package.add_argument("--target", required=True, choices=TARGETS)
    package.add_argument("--version", required=True)
    package.add_argument("--binary", type=Path)
    package.add_argument("--output", type=Path, default=ROOT / "dist")

    checksums = subparsers.add_parser(
        "checksums", help="validate all release archives and write SHA256SUMS"
    )
    checksums.add_argument("--version", required=True)
    checksums.add_argument("--directory", type=Path, default=ROOT / "dist")

    formula = subparsers.add_parser("formula", help="generate Formula/shue.rb")
    formula.add_argument("--repository", required=True)
    formula.add_argument("--version", required=True)
    formula.add_argument("--checksums", type=Path, required=True)
    formula.add_argument("--output", type=Path, required=True)

    bundle = subparsers.add_parser(
        "validate-bundle", help="validate the exact release-asset set"
    )
    bundle.add_argument("--version", required=True)
    bundle.add_argument("--directory", type=Path, required=True)
    bundle.add_argument("--require-formula", action="store_true")

    tap_check = subparsers.add_parser(
        "tap-check", help="reject Homebrew tap downgrades and replacements"
    )
    tap_check.add_argument("--candidate", type=Path, required=True)
    tap_check.add_argument("--current", type=Path, required=True)
    tap_check.add_argument("--version", required=True)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if args.command == "version":
            print(validate_tag(args.tag))
        elif args.command == "verify":
            binary = args.binary or ROOT / "target" / args.target / "release" / "shue"
            verify_binary(binary, args.target, args.version)
            print("release binary verification passed")
        elif args.command == "package":
            if args.version != workspace_version():
                raise ReleaseError(
                    f"package version {args.version!r} does not match workspace version"
                )
            binary = args.binary or ROOT / "target" / args.target / "release" / "shue"
            verify_binary(binary, args.target, args.version)
            print(
                create_archive(
                    root=ROOT,
                    binary=binary,
                    target=args.target,
                    version=args.version,
                    output_directory=args.output,
                )
            )
        elif args.command == "checksums":
            print(write_checksums(args.directory, args.version))
        elif args.command == "formula":
            print(
                write_formula(
                    repository=args.repository,
                    version=args.version,
                    checksums_path=args.checksums,
                    output=args.output,
                )
            )
        elif args.command == "validate-bundle":
            validate_bundle(args.directory, args.version, args.require_formula)
            print("release bundle verification passed")
        elif args.command == "tap-check":
            print(check_tap_update(args.candidate, args.current, args.version))
        else:  # pragma: no cover - argparse guarantees a known command
            raise ReleaseError(f"unknown command: {args.command}")
    except (OSError, ReleaseError, tarfile.TarError, tomllib.TOMLDecodeError) as error:
        print(f"release error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
