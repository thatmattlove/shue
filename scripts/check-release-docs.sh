#!/bin/sh
set -eu

python3 - <<'PY'
from pathlib import Path
import re
import tomllib

root = Path.cwd()
readme = (root / "README.md").read_text(encoding="utf-8")
workflow = (root / ".github/workflows/release.yml").read_text(encoding="utf-8")
with (root / "Cargo.toml").open("rb") as manifest:
    package = tomllib.load(manifest)["workspace"]["package"]
version = package["version"]
if package.get("license") != "MIT":
    raise SystemExit("workspace license is not exactly MIT")

required = [
    "brew install OWNER/tap/shue",
    "set -eu",
    "SHA256SUMS",
    f"VERSION={version}",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
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
    "Shue is licensed under the MIT License. See `LICENSE-MIT`.",
]
missing = [text for text in required if text not in readme]
if missing:
    raise SystemExit(f"README release documentation is missing: {missing!r}")

if readme.count("```") % 2:
    raise SystemExit("README has unbalanced code fences")
if "OWNER/homebrew-tap" not in readme or "Formula/shue.rb" not in workflow:
    raise SystemExit("Homebrew tap naming/layout is not documented and implemented")
if not re.search(r'push:\s+tags:\s+- "v\*"', workflow):
    raise SystemExit("documented tag-triggered release workflow is missing")

print("release documentation verification passed")
PY
