# Contributing to shue

## Development

Build on macOS or Linux with Rust 1.85 or newer and native C build tools for
PCRE2. Install `rustfmt` and Clippy for code checks. The verification scripts
also require Python 3.11+ and Ruby; MSRV checks use Rust 1.85.0.

From a source checkout:

```console
cargo build --release
./target/release/shue --help
```

To install from the checkout:

```console
cargo install --path crates/shue-cli --locked
```

Before submitting a change, run the relevant checks:

```console
make test
make quality
```

Run `make verify` for the full suite: workspace tests, formatting, Clippy,
optimized acceptance and performance tests, MSRV, license policy, release
packaging and documentation, and interactive terminal behavior.
Use `make help` to list individual checks. Keep documentation and
[examples/config.yaml](examples/config.yaml) in sync with behavior changes.

## Demo assets

The README's GIF and PNG show actual shue output from
[examples/demo.log](examples/demo.log), rendered with a terminal palette and
scripted animation timing. To regenerate them, use Python 3 with Pillow and
a monospace font:

```console
cargo build --locked -p shue
python3 scripts/generate-demo.py
```

The renderer uses the embedded default rules so local configs cannot change the
capture. It looks for Source Code Pro, Menlo, DejaVu Sans Mono, or Liberation
Mono; use `--font /path/to/font.ttf` to select another font.

## Runtime maintenance

Rules compile once before streaming. Matching uses byte-oriented PCRE2 with
JIT when supported by the linked engine. Keep terminal escape sequences and
non-UTF-8 output intact.

Interactive children use a PTY with raw input, resize propagation, and an idle
flush for prompts without newlines. Redirected streams use bounded 32 KiB reads;
normal buffering retains overlap for matches split across reads. Preserve
terminal restoration, signal forwarding, and child reaping when changing this
code. Non-terminal stdin uses an isolated child process group for cleanup;
terminal stdin stays in the foreground process group for job control.

## Releases

The tag-triggered [release workflow](.github/workflows/release.yml) requires the
tag to match `workspace.package.version` in `Cargo.toml` exactly. A SemVer
prerelease such as `0.2.0-rc.1` is published as a GitHub prerelease and does not
update the stable Homebrew formula.

### One-time setup

1. Create and initialize a public `OWNER/homebrew-tap` repository with a default
   branch that the release identity can update.
2. Add `HOMEBREW_TAP_TOKEN` to the source repository's Actions secrets. Use a
   fine-grained token restricted to the tap repository with Contents read/write
   permission; the source repository's `GITHUB_TOKEN` cannot write to another repo.
3. For a non-default tap, set the Actions variable `HOMEBREW_TAP_REPOSITORY` to
   `owner/homebrew-name`. The `homebrew-` prefix is required for Homebrew's short
   tap syntax; for example, `OWNER/homebrew-tools` uses `OWNER/tools/shue`.
4. Protect `v*` tags and workflow changes, and enable GitHub immutable releases.
   Publishing uses the `release` environment; add required reviewers there if
   releases need manual approval.

### Publish a release

1. Update the workspace version in `Cargo.toml` and refresh `Cargo.lock`.
2. Run `make verify`, review the change, and merge it to the default branch.
3. Create and push the matching annotated tag from that commit:

   ```console
   git tag -a v0.2.0 -m "shue 0.2.0"
   git push origin v0.2.0
   ```

The workflow uses Rust 1.85.0, locked dependencies, bundled static PCRE2, and
GitHub-hosted native runners. It:

- Builds and smoke-tests Apple Silicon and Intel macOS, plus ARM64 and x86-64
  Linux binaries. Linux archives use musl; macOS binaries linked to a local
  Homebrew PCRE2 library are rejected.
- Runs locked workspace tests on Linux and macOS with Rust 1.85.0.
- Creates deterministic archives with third-party license notices and
  `SHA256SUMS`, validates the asset manifest, and publishes the GitHub release.
- Tests the generated Homebrew formula against published assets on all four
  targets, including checksum verification and the version test. The formula
  installs Shue's MIT license and statically linked dependencies' notices.
- Updates the tap, retrying concurrent pushes while refusing a version downgrade,
  same-version rewrite, or restoration of a deliberately removed formula.

After correcting an external prerequisite, retry only the failed jobs.
Do not replace published release assets after their checksums reach the tap.
