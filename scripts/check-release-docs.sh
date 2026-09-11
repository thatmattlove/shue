#!/bin/sh
set -eu

python3 - <<'PY'
from pathlib import Path
import re
import tomllib

root = Path.cwd()
readme = (root / "README.md").read_text(encoding="utf-8")
contributing = (root / "CONTRIBUTING.md").read_text(encoding="utf-8")
workflow = (root / ".github/workflows/release.yml").read_text(encoding="utf-8")
with (root / "Cargo.toml").open("rb") as manifest:
    package = tomllib.load(manifest)["workspace"]["package"]
version = package["version"]
if package.get("license") != "MIT":
    raise SystemExit("workspace license is not exactly MIT")

readme_required = [
    "brew install thatmattlove/tap/shue",
    "set -eu",
    "SHA256SUMS",
    f"VERSION={version}",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
    "[CONTRIBUTING.md](CONTRIBUTING.md)",
    "[LICENSE](LICENSE)",
]
contributing_required = [
    "HOMEBREW_TAP_TOKEN",
    "HOMEBREW_TAP_REPOSITORY",
    "owner/homebrew-name",
    "fine-grained token",
    "immutable releases",
    "git tag -a",
    "prerelease",
    "Rust 1.85.0",
    "static PCRE2",
    "third-party license",
    "version downgrade",
]
for name, document, required in [
    ("README.md", readme, readme_required),
    ("CONTRIBUTING.md", contributing, contributing_required),
]:
    missing = [text for text in required if text not in document]
    if missing:
        raise SystemExit(f"{name} release documentation is missing: {missing!r}")
    if document.count("```") % 2:
        raise SystemExit(f"{name} has unbalanced code fences")

if "OWNER/homebrew-tap" not in contributing or "Formula/shue.rb" not in workflow:
    raise SystemExit("Homebrew tap naming/layout is not documented and implemented")
if not re.search(r'push:\s+tags:\s+- "v\*"', workflow):
    raise SystemExit("documented tag-triggered release workflow is missing")

print("release documentation verification passed")
PY
