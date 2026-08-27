#!/usr/bin/env python3
"""Structural and fixture-based verification for the release pipeline."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_PATH = ROOT / ".github/workflows/release.yml"
RELEASE_TOOL_PATH = ROOT / "scripts/release.py"
ACTION_PINS = {
    "actions/checkout": "3d3c42e5aac5ba805825da76410c181273ba90b1",
    "actions/upload-artifact": "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
    "actions/download-artifact": "3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
}
EXPECTED_MATRIX = {
    ("aarch64-apple-darwin", "macos-15"),
    ("x86_64-apple-darwin", "macos-15-intel"),
    ("aarch64-unknown-linux-musl", "ubuntu-24.04-arm"),
    ("x86_64-unknown-linux-musl", "ubuntu-24.04"),
}


def fail(message: str) -> "NoReturn":
    raise AssertionError(message)


def expect_release_error(release, action, label: str) -> None:
    try:
        action()
    except release.ReleaseError:
        return
    fail(f"{label} positive control unexpectedly passed")


def load_workflow() -> dict:
    ruby = """
require "json"
require "yaml"
source = File.read(ARGV.fetch(0), encoding: "UTF-8")
puts JSON.generate(YAML.safe_load(source, aliases: true))
"""
    try:
        result = subprocess.run(
            ["ruby", "-e", ruby, str(WORKFLOW_PATH)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        stderr = getattr(error, "stderr", "")
        fail(f"release workflow is not valid YAML: {stderr.strip()}")
    workflow = json.loads(result.stdout)
    if not isinstance(workflow, dict):
        fail("release workflow must be a YAML mapping")
    return workflow


def load_release_tool():
    spec = importlib.util.spec_from_file_location("shue_release", RELEASE_TOOL_PATH)
    if spec is None or spec.loader is None:
        fail("unable to import scripts/release.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def all_uses(value) -> list[str]:
    found: list[str] = []
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "uses" and isinstance(child, str):
                found.append(child)
            found.extend(all_uses(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(all_uses(child))
    return found


def action_step(job: dict, action: str) -> dict:
    matches = [
        step
        for step in job.get("steps", [])
        if isinstance(step, dict) and step.get("uses", "").startswith(f"{action}@")
    ]
    if len(matches) != 1:
        fail(f"job must use {action} exactly once; found {len(matches)}")
    return matches[0]


def step_script(job: dict) -> str:
    return "\n".join(
        step.get("run", "")
        for step in job.get("steps", [])
        if isinstance(step, dict) and isinstance(step.get("run", ""), str)
    )


def check_shell_syntax(workflow: dict) -> None:
    for job_name, job in workflow.get("jobs", {}).items():
        for index, step in enumerate(job.get("steps", []), 1):
            script = step.get("run") if isinstance(step, dict) else None
            if not isinstance(script, str):
                continue
            result = subprocess.run(
                ["bash", "-n"],
                input=script,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )
            if result.returncode != 0:
                fail(
                    f"shell syntax error in {job_name} step {index}: "
                    f"{result.stdout.strip()}"
                )


def check_workflow(workflow: dict, source: str) -> None:
    trigger = workflow.get("on")
    if trigger != {"push": {"tags": ["v*"]}}:
        fail(f"release trigger changed unexpectedly: {trigger!r}")
    if workflow.get("permissions") != {}:
        fail("workflow-level permissions must remain empty")
    if workflow.get("env", {}).get("RUST_VERSION") != "1.85.0":
        fail("release workflow must use the documented, tested Rust 1.85.0 toolchain")
    if workflow.get("env", {}).get("RUSTUP_TOOLCHAIN") != "1.85.0":
        fail("release helper subprocesses must use the exact Rust 1.85.0 toolchain")

    jobs = workflow.get("jobs")
    expected_jobs = {
        "prepare",
        "release_test",
        "build",
        "bundle",
        "publish",
        "homebrew_test",
        "homebrew_publish",
    }
    if not isinstance(jobs, dict) or set(jobs) != expected_jobs:
        fail(f"release jobs mismatch: {sorted(jobs or {})!r}")

    for name, job in jobs.items():
        if "${{ runner." in json.dumps(job.get("env", {})):
            fail(f"job {name!r} uses the unavailable runner context in job-level env")

    release_test = jobs["release_test"]
    if release_test.get("needs") != "prepare":
        fail("release source tests must wait for tag validation")
    if release_test.get("permissions") != {"contents": "read"}:
        fail("release source tests need read-only repository access")
    release_test_matrix = release_test["strategy"]["matrix"]["include"]
    if {(entry["os"], entry["quality"]) for entry in release_test_matrix} != {
        ("ubuntu-24.04", True),
        ("macos-15", False),
    }:
        fail("release source tests must cover Linux/macOS with one quality runner")
    if release_test.get("env", {}).get("PCRE2_SYS_STATIC") != "1":
        fail("release source tests must exercise bundled static PCRE2")
    release_test_script = step_script(release_test)
    for required in [
        'rustup toolchain install "$RUST_VERSION" --profile minimal',
        'cargo +"$RUST_VERSION" test --workspace --all-targets --locked',
        'cargo +"$RUST_VERSION" fmt --all -- --check',
        'cargo +"$RUST_VERSION" clippy --locked --workspace --all-targets',
        "scripts/check-license-policy.sh",
    ]:
        if required not in release_test_script:
            fail(f"release source test job is missing {required!r}")

    includes = jobs["build"]["strategy"]["matrix"]["include"]
    matrix = {(entry["target"], entry["os"]) for entry in includes}
    if matrix != EXPECTED_MATRIX:
        fail(f"release target matrix mismatch: {sorted(matrix)!r}")
    if jobs["build"].get("env", {}).get("PCRE2_SYS_STATIC") != "1":
        fail("release builds must force static PCRE2")
    if "version_without_build" not in step_script(jobs["prepare"]):
        fail("prerelease detection must ignore SemVer build metadata")
    prepare_outputs = jobs["prepare"].get("outputs", {})
    if "source_sha" not in prepare_outputs or "HEAD^{commit}" not in step_script(jobs["prepare"]):
        fail("tag validation must expose the peeled source commit for publication")

    build_script = step_script(jobs["build"])
    for required in [
        'echo "CC_${target_key}=musl-gcc"',
        'echo "CARGO_TARGET_${cargo_key}_LINKER=musl-gcc"',
        'cargo +"$RUST_VERSION" build --release --locked -p shue --bin shue',
        'scripts/release.py verify --target "$TARGET" --version "$VERSION"',
        'scripts/release.py package --target "$TARGET" --version "$VERSION"',
    ]:
        if required not in build_script:
            fail(f"build job is missing {required!r}")
    build_upload = action_step(jobs["build"], "actions/upload-artifact")
    if build_upload.get("with", {}).get("archive") is not False:
        fail("binary archives must use direct artifact upload without a wrapping ZIP")
    if "SOURCE_DATE_EPOCH" not in build_script:
        fail("release archives must use the tagged commit time deterministically")
    if 'git show -s --format=%ct "${SOURCE_SHA}^{commit}"' not in build_script:
        fail("archive timestamps must peel annotated tags to their source commit")

    publish = jobs["publish"]
    if publish.get("environment") != "release":
        fail("GitHub publication must use the protectable release environment")
    if publish.get("permissions") != {"contents": "write"}:
        fail("only the publisher should request contents: write")
    publish_uses = all_uses(publish)
    if any(value.startswith("actions/checkout@") for value in publish_uses):
        fail("the write-capable publish job must not check out repository code")
    publish_script = step_script(publish)
    for required in [
        "gh api",
        "SOURCE_SHA",
        "gh release create",
        "--verify-tag",
        "--generate-notes",
    ]:
        if required not in publish_script:
            fail(f"publish job is missing {required!r}")
    if publish.get("env", {}).get("SOURCE_SHA") != "${{ needs.prepare.outputs.source_sha }}":
        fail("publisher must compare the tag against prepare's peeled source commit")
    if "GITHUB_SHA" in publish_script:
        fail("publisher must not compare a peeled tag commit to the raw tag-object SHA")

    for name, job in jobs.items():
        if name != "publish" and job.get("permissions", {}).get("contents") == "write":
            fail(f"job {name!r} unexpectedly has contents: write")

    if jobs["bundle"].get("needs") != ["prepare", "release_test", "build"]:
        fail("bundle must wait for source tests and every binary build")
    if jobs["publish"].get("needs") != ["prepare", "bundle", "release_test"]:
        fail("GitHub publication must wait for tests and the complete bundle")
    bundle_script = step_script(jobs["bundle"])
    for required in [
        "$RUNNER_TEMP/shue-release-bundle",
        "validate-bundle",
        "--require-formula",
    ]:
        if required not in bundle_script:
            fail(f"bundle job is missing isolated-manifest control {required!r}")
    bundle_download = action_step(jobs["bundle"], "actions/download-artifact")
    download_inputs = bundle_download.get("with", {})
    if download_inputs.get("skip-decompress") is not True:
        fail("direct binary artifacts must be downloaded without decompression")
    if "runner.temp" not in str(download_inputs.get("path", "")):
        fail("release bundles must use the runner's isolated temporary directory")
    bundle_upload = action_step(jobs["bundle"], "actions/upload-artifact")
    if bundle_upload.get("with", {}).get("name") != "release-bundle":
        fail("validated release assets must be uploaded as one named bundle")
    if jobs["homebrew_test"].get("needs") != ["prepare", "publish"]:
        fail("Homebrew testing must wait for published release assets")
    homebrew_matrix = jobs["homebrew_test"]["strategy"]["matrix"]["include"]
    tested_homebrew_systems = {entry["os"] for entry in homebrew_matrix}
    if tested_homebrew_systems != {os_name for _, os_name in EXPECTED_MATRIX}:
        fail("Homebrew must install-test every released platform/architecture")
    if sum(entry.get("audit") is True for entry in homebrew_matrix) != 1:
        fail("exactly one Homebrew runner must perform the all-platform audit")
    homebrew_test_script = step_script(jobs["homebrew_test"])
    for required in [
        "brew tap-new",
        "brew style --formula",
        "brew audit --strict --online",
        "brew install --formula",
        "brew test",
        'former_license="LICENSE-A""PACHE"',
        'test ! -e "$documentation/$former_license"',
        "THIRD-PARTY-LICENSES.txt",
        "brew untap --force",
    ]:
        if required not in homebrew_test_script:
            fail(f"Homebrew test job is missing {required!r}")
    if jobs["homebrew_test"].get("env", {}).get("FORMULA_REF") != "shue/release-test/shue":
        fail("Homebrew tests must address a formula staged in a temporary tap")
    if 'brew install --formula "$RUNNER_TEMP' in homebrew_test_script:
        fail("Homebrew 6 forbids installing formulae directly from arbitrary paths")
    if jobs["homebrew_publish"].get("needs") != [
        "prepare",
        "publish",
        "homebrew_test",
    ]:
        fail("tap publication must wait for release and Homebrew tests")
    if "prerelease == 'false'" not in jobs["homebrew_publish"].get("if", ""):
        fail("prereleases must not update the stable Homebrew formula")

    if source.count("HOMEBREW_TAP_TOKEN") != 1:
        fail("the tap credential must appear exactly once")
    homebrew_publish_text = json.dumps(jobs["homebrew_publish"], sort_keys=True)
    if jobs["homebrew_publish"].get("environment") != "release":
        fail("tap publication must use the protectable release environment")
    if "HOMEBREW_TAP_TOKEN" not in homebrew_publish_text:
        fail("the tap credential escaped the tap-publishing job")
    if "homebrew-tap" not in homebrew_publish_text or "Formula/shue.rb" not in homebrew_publish_text:
        fail("tap repository layout is incomplete")
    tap_script = step_script(jobs["homebrew_publish"])
    for required in [
        "owner/homebrew-name",
        "tap-check",
        "for attempt in 1 2 3 4",
        "fetch --no-tags origin",
        "--diff-filter=AM",
        "git -C homebrew-tap add Formula/shue.rb",
        "git -C homebrew-tap diff --cached --check",
        "git -C homebrew-tap diff --cached --quiet",
        'push origin "HEAD:refs/heads/$tap_branch"',
    ]:
        if required not in tap_script:
            fail(f"tap update is missing {required!r}")
    if tap_script.index("tap-check") > tap_script.index("cp \""):
        fail("tap version guard must run before the candidate formula is copied")
    if "push --force" in tap_script or "push -f" in tap_script:
        fail("tap publication must never force-push")

    uses = all_uses(workflow)
    if not uses:
        fail("release workflow has no actions")
    for value in uses:
        match = re.fullmatch(r"([^@]+)@([0-9a-f]{40})", value)
        if match is None:
            fail(f"action is not pinned to a full commit SHA: {value}")
        action, revision = match.groups()
        expected = ACTION_PINS.get(action)
        if expected is None:
            fail(f"unreviewed action in release workflow: {action}")
        if revision != expected:
            fail(f"unexpected pin for {action}: {revision}")


def check_release_helpers(release) -> None:
    release_source = RELEASE_TOOL_PATH.read_text(encoding="utf-8")
    if "static-pie linked" not in release_source or 'r"^\\s*INTERP\\s"' not in release_source:
        fail("Linux linkage verification must accept static PIE and reject ELF interpreters")
    if '["file", "-b", str(binary)]' not in release_source:
        fail("architecture verification must suppress file's path prefix")

    version = release.workspace_version(ROOT)
    if release.workspace_rust_version(ROOT) != "1.85":
        fail("workspace MSRV must match the release workflow's Rust 1.85.0 toolchain")
    gitignore = (ROOT / ".gitignore").read_text(encoding="utf-8").splitlines()
    if "/dist/" not in gitignore or "__pycache__/" not in gitignore:
        fail("generated release/Python outputs must remain outside version control")
    if release.validate_tag(f"v{version}", ROOT) != version:
        fail("valid release tag did not resolve to the workspace version")
    for valid in ["0.0.0", "1.2.3-rc.1", "1.2.3+build-7", "1.2.3-0.alpha"]:
        release.validate_version(valid)
    for invalid in ["01.2.3", "1.02.3", "1.2.03", "1.2.3-01", "1.2"]:
        expect_release_error(
            release,
            lambda value=invalid: release.validate_version(value),
            f"invalid SemVer {invalid}",
        )
    expect_release_error(
        release,
        lambda: release.validate_tag("v999.0.0", ROOT),
        "tag/version mismatch",
    )
    comparisons = [
        ("1.10.0", "1.9.999999999999999999999999999999", 1),
        ("1.0.0-rc.1", "1.0.0", -1),
        ("1.0.0-alpha.10", "1.0.0-alpha.2", 1),
        ("1.0.0+one", "1.0.0+two", 0),
    ]
    for left, right, expected in comparisons:
        actual = release.compare_versions(left, right)
        if actual != expected:
            fail(f"SemVer comparison {left} vs {right} returned {actual}, expected {expected}")

    with tempfile.TemporaryDirectory(prefix="shue-architecture-check-") as temporary:
        binary = Path(temporary) / "aarch64-apple-darwin/shue"
        binary.parent.mkdir()
        binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        binary.chmod(0o755)
        calls: list[list[str]] = []
        original_run_output = release.run_output

        def wrong_architecture(command: list[str]) -> str:
            calls.append(command)
            if command[0] == "file":
                return "Mach-O 64-bit executable x86_64"
            fail(f"architecture guard continued after mismatch: {command!r}")

        release.run_output = wrong_architecture
        try:
            expect_release_error(
                release,
                lambda: release.verify_binary(binary, "aarch64-apple-darwin", version),
                "wrong-architecture binary",
            )
        finally:
            release.run_output = original_run_output
        if calls != [["file", "-b", str(binary)]]:
            fail(f"architecture guard invoked file unsafely: {calls!r}")

    former_name = "Apa" + "che"
    dependencies = release.normal_dependencies(release.cargo_metadata(ROOT))
    delegated_package = next(
        package
        for package in dependencies
        if package["name"] == "winapi-i686-pc-windows-gnu"
    )
    delegate, delegated_files = release.delegated_license_files(
        delegated_package, dependencies
    )
    expected_delegate_files = {"LICENSE-MIT", "LICENSE-A" + "PACHE"}
    if delegate["name"] != "winapi" or {
        path.name for path in delegated_files
    } != expected_delegate_files:
        fail("reviewed winapi license delegation is incomplete")
    for field in ["repository", "license"]:
        mismatched = dict(delegated_package)
        mismatched[field] = "mismatched fixture"
        expect_release_error(
            release,
            lambda mismatched=mismatched: release.delegated_license_files(
                mismatched, dependencies
            ),
            f"winapi delegate {field} mismatch",
        )
    original_delegation = release.LICENSE_DELEGATES[delegated_package["name"]]
    try:
        delegate_name, required_files = original_delegation
        release.LICENSE_DELEGATES[delegated_package["name"]] = (
            delegate_name,
            required_files | {"MISSING-LICENSE-FIXTURE"},
        )
        expect_release_error(
            release,
            lambda: release.delegated_license_files(delegated_package, dependencies),
            "winapi delegate missing required file",
        )
    finally:
        release.LICENSE_DELEGATES[delegated_package["name"]] = original_delegation

    notices = release.third_party_licenses(ROOT)
    dependency_declaration = f"Declared license: MIT OR {former_name}-2.0".encode()
    dependency_full_text = (
        f"{former_name} License\n                           Version 2.0, January 2004"
    ).encode()
    for marker in [
        b"Package: linux-raw-sys",
        b"--- COPYRIGHT ---",
        dependency_declaration,
        dependency_full_text,
        b"PCRE2 License",
        b"Zoltan Herczeg",
        b"musl as a whole",
        b"The Rust Project",
    ]:
        if marker not in notices:
            fail(f"third-party notices are missing {marker.decode()!r}")

    with tempfile.TemporaryDirectory(prefix="shue-release-check-") as temporary:
        root = Path(temporary)
        directory = root / "bundle"
        directory.mkdir()
        binary = root / "shue"
        binary.write_bytes(b"fixture release executable\n")
        binary.chmod(0o755)
        for target in release.TARGETS:
            archive = release.create_archive(
                root=ROOT,
                binary=binary,
                target=target,
                version=version,
                output_directory=directory,
            )
            release.inspect_archive(archive, version, target)

        first_archive = directory / release.archive_name(version, release.TARGETS[0])
        top = first_archive.name.removesuffix(".tar.gz")
        with release.tarfile.open(first_archive, "r:gz") as archive:
            notice_member = archive.extractfile(f"{top}/THIRD-PARTY-LICENSES.txt")
            if notice_member is None:
                fail("release archive omitted the generated third-party notices")
            archived_notices = notice_member.read()
            for marker in [b"PCRE2 License", dependency_declaration, dependency_full_text]:
                if marker not in archived_notices:
                    fail(
                        "release archive did not retain third-party marker "
                        f"{marker.decode()!r}"
                    )

        repeat_directory = root / "repeat"
        original = directory / release.archive_name(version, release.TARGETS[0])
        repeated = release.create_archive(
            root=ROOT,
            binary=binary,
            target=release.TARGETS[0],
            version=version,
            output_directory=repeat_directory,
        )
        if original.read_bytes() != repeated.read_bytes():
            fail("identical packaging inputs did not produce a deterministic archive")

        checksums = release.write_checksums(directory, version)
        parsed = release.read_checksums(checksums, version)
        if len(parsed) != 4 or any(len(value) != 64 for value in parsed.values()):
            fail("checksum manifest did not cover all four release archives")

        missing = directory / release.archive_name(version, release.TARGETS[0])
        saved = missing.read_bytes()
        missing.unlink()
        expect_release_error(
            release,
            lambda: release.write_checksums(directory, version),
            "missing archive",
        )
        missing.write_bytes(saved)

        formula = directory / "shue.rb"
        release.write_formula(
            repository="owner/shue",
            version=version,
            checksums_path=checksums,
            output=formula,
        )
        formula_source = formula.read_text(encoding="utf-8")
        has_explicit_version = f'  version "{version}"' in formula_source
        if has_explicit_version != ("+" in version):
            fail(
                "formula must use an explicit version exactly when SemVer build "
                "metadata cannot be inferred safely from its URLs"
            )
        for target in release.TARGETS:
            name = release.archive_name(version, target)
            if name not in formula_source or parsed[name] not in formula_source:
                fail(f"formula is missing URL/checksum mapping for {target}")
        for required in [
            "on_macos do",
            "on_linux do",
            "on_arm do",
            "on_intel do",
            'bin.install "shue"',
            'doc.install "LICENSE-MIT", "THIRD-PARTY-LICENSES.txt"',
            'pipe_output("#{bin}/shue --no-color --filter", "homebrew\\n", 0)',
            'license "MIT"',
        ]:
            if required not in formula_source:
                fail(f"formula is missing {required!r}")
        syntax = subprocess.run(
            ["ruby", "-c", str(formula)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        if syntax.returncode != 0 or "Syntax OK" not in syntax.stdout:
            fail(f"generated formula is not valid Ruby: {syntax.stdout.strip()}")

        release.validate_bundle(directory, version, True)
        stray = directory / "unexpected.txt"
        stray.write_text("must not ship\n", encoding="utf-8")
        expect_release_error(
            release,
            lambda: release.validate_bundle(directory, version, True),
            "stray release asset",
        )
        stray.unlink()

        metadata_version = "1.2.3+build-7"
        metadata_checksums = {
            release.archive_name(metadata_version, target): "a" * 64
            for target in release.TARGETS
        }
        metadata_formula = root / "metadata.rb"
        metadata_formula.write_text(
            release.formula_text("owner/shue", metadata_version, metadata_checksums),
            encoding="utf-8",
        )
        metadata_source = metadata_formula.read_text(encoding="utf-8")
        if f'  version "{metadata_version}"' not in metadata_source:
            fail("formula must preserve SemVer build metadata explicitly")
        if release.formula_version(metadata_formula) != metadata_version:
            fail("formula parser did not preserve SemVer build metadata")
        metadata_syntax = subprocess.run(
            ["ruby", "-c", str(metadata_formula)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        if metadata_syntax.returncode != 0:
            fail(f"build-metadata formula is not valid Ruby: {metadata_syntax.stdout}")

        def formula_fixture(path: Path, fixture_version: str, digit: str = "b") -> Path:
            fixture_checksums = {
                release.archive_name(fixture_version, target): digit * 64
                for target in release.TARGETS
            }
            path.write_text(
                release.formula_text("owner/shue", fixture_version, fixture_checksums),
                encoding="utf-8",
            )
            return path

        candidate = formula_fixture(root / "candidate.rb", "2.0.0")
        absent = root / "absent.rb"
        if release.check_tap_update(candidate, absent, "2.0.0") != "first release":
            fail("first tap publication was not accepted")
        current = formula_fixture(root / "current.rb", "1.9.9")
        if not release.check_tap_update(candidate, current, "2.0.0").startswith("upgrade"):
            fail("higher tap version was not accepted")
        current.write_bytes(candidate.read_bytes())
        if release.check_tap_update(candidate, current, "2.0.0") != "already current":
            fail("byte-identical tap retry was not idempotent")
        formula_fixture(current, "2.0.0", "c")
        expect_release_error(
            release,
            lambda: release.check_tap_update(candidate, current, "2.0.0"),
            "same-version tap replacement",
        )
        formula_fixture(current, "2.1.0")
        expect_release_error(
            release,
            lambda: release.check_tap_update(candidate, current, "2.0.0"),
            "tap downgrade",
        )
        build_candidate = formula_fixture(root / "build-candidate.rb", "3.0.0+two")
        build_current = formula_fixture(root / "build-current.rb", "3.0.0+one")
        expect_release_error(
            release,
            lambda: release.check_tap_update(
                build_candidate, build_current, "3.0.0+two"
            ),
            "equal-precedence build-metadata update",
        )
        malformed = root / "malformed.rb"
        malformed.write_text("class Shue < Formula\nend\n", encoding="utf-8")
        expect_release_error(
            release,
            lambda: release.check_tap_update(candidate, malformed, "2.0.0"),
            "malformed current formula",
        )
        symlink_formula = root / "symlink.rb"
        symlink_formula.symlink_to(candidate)
        expect_release_error(
            release,
            lambda: release.check_tap_update(candidate, symlink_formula, "2.0.0"),
            "symlinked current formula",
        )
        prerelease = formula_fixture(root / "prerelease.rb", "4.0.0-rc.1")
        expect_release_error(
            release,
            lambda: release.check_tap_update(prerelease, current, "4.0.0-rc.1"),
            "stable-tap prerelease",
        )


def check_first_tap_update() -> None:
    with tempfile.TemporaryDirectory(prefix="shue-tap-check-") as temporary:
        tap = Path(temporary)

        def git(*arguments: str, check: bool = True) -> subprocess.CompletedProcess:
            return subprocess.run(
                ["git", *arguments],
                cwd=tap,
                check=check,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )

        git("init", "--quiet")
        git("config", "user.name", "release test")
        git("config", "user.email", "release-test@invalid")
        git("config", "commit.gpgsign", "false")
        git("config", "tag.gpgsign", "false")
        (tap / "README.md").write_text("# tap\n", encoding="utf-8")
        git("add", "README.md")
        git("commit", "--quiet", "-m", "initialize tap")

        formula = tap / "Formula/shue.rb"
        formula.parent.mkdir()
        formula.write_text("class Shue < Formula\nend\n", encoding="utf-8")
        if git("diff", "--quiet", "--", "Formula/shue.rb", check=False).returncode != 0:
            fail("untracked-formula positive control no longer demonstrates the git diff hazard")
        git("add", "Formula/shue.rb")
        if git("diff", "--cached", "--quiet", check=False).returncode == 0:
            fail("first tap formula was not visible after staging")
        git("diff", "--cached", "--check")
        git("commit", "--quiet", "-m", "shue fixture")
        formula.write_text(formula.read_text(encoding="utf-8"), encoding="utf-8")
        git("add", "Formula/shue.rb")
        if git("diff", "--cached", "--quiet", check=False).returncode != 0:
            fail("an unchanged formula would create a redundant tap commit")


def check_annotated_tag_peeling() -> None:
    with tempfile.TemporaryDirectory(prefix="shue-tag-check-") as temporary:
        repository = Path(temporary)

        def git(*arguments: str) -> str:
            return subprocess.run(
                ["git", *arguments],
                cwd=repository,
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            ).stdout.strip()

        git("init", "--quiet")
        git("config", "user.name", "release test")
        git("config", "user.email", "release-test@invalid")
        git("config", "commit.gpgsign", "false")
        git("config", "tag.gpgsign", "false")
        (repository / "source").write_text("release\n", encoding="utf-8")
        git("add", "source")
        git("commit", "--quiet", "-m", "release source")
        git("tag", "-a", "v1.0.0", "-m", "release 1.0.0")
        tag_object = git("rev-parse", "v1.0.0")
        source_commit = git("rev-parse", "v1.0.0^{commit}")
        if tag_object == source_commit:
            fail("annotated-tag fixture did not create a distinct tag object")
        epoch = git("show", "-s", "--format=%ct", f"{source_commit}^{{commit}}")
        if not epoch.isdigit() or "\n" in epoch:
            fail(f"peeled release epoch is not one integer: {epoch!r}")


def main() -> int:
    try:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        workflow = load_workflow()
        release = load_release_tool()
        check_workflow(workflow, source)
        check_shell_syntax(workflow)
        check_release_helpers(release)
        check_first_tap_update()
        check_annotated_tag_peeling()
    except (
        AssertionError,
        OSError,
        KeyError,
        RuntimeError,
        subprocess.CalledProcessError,
        TypeError,
        ValueError,
    ) as error:
        print(f"release pipeline verification failed: {error}", file=sys.stderr)
        return 1
    print("release pipeline verification passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
