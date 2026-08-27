# shue

`shue` is a Rust terminal highlighter inspired by
[ChromaTerm2](https://github.com/rgcr/ChromaTerm2). It runs the normal SSH
client by default, applies precompiled PCRE2 byte regexes to terminal output,
and does not require a shell alias:

```console
shue username@host
```

It also wraps an explicit program or acts as a stdin filter. Existing terminal
escape sequences and non-UTF-8 bytes are preserved; `shue` only adds styling
around matched text.

## Why Rust

Rust fits both halves of this program: byte-oriented PCRE2 with JIT when the
linked engine supports it provides lookarounds, captures, and backreferences,
while Rust makes it practical to build a bounded, low-overhead PTY streaming
loop with deterministic cleanup. The reader uses fixed-size reads and a bounded
queue, and rules are compiled once before streaming. These are architectural
choices, not an unmeasured speedup claim.

## Platforms and installation

The supported platforms are macOS and Linux. Stable releases provide native
Apple Silicon, Intel macOS, ARM64 Linux, and x86-64 Linux binaries. Linux
archives use musl so they do not inherit the GitHub runner's glibc version.

### Homebrew

The release pipeline publishes a formula to the repository owner's
`homebrew-tap` repository. Replace `OWNER` with the GitHub owner shown at the
top of this repository:

```console
brew install OWNER/tap/shue
```

Homebrew verifies the release archive's SHA-256 checksum, installs `shue`, and
runs the formula's version test in the release pipeline on all four supported
platform and architecture combinations. The formula also retains Shue's MIT
license and its statically linked dependencies' license notices in the
installed keg. For a non-default tap repository, replace `tap` with its
normalized tap name; for example, a repository named `homebrew-tools` uses
`OWNER/tools/shue`.

### Precompiled binary

Download `SHA256SUMS` and the archive for your machine from the repository's
[Releases](../../releases) page. The asset suffixes are:

| System | Target suffix |
| --- | --- |
| Apple Silicon macOS | `aarch64-apple-darwin` |
| Intel macOS | `x86_64-apple-darwin` |
| ARM64 Linux | `aarch64-unknown-linux-musl` |
| x86-64 Linux | `x86_64-unknown-linux-musl` |

For example, these commands download, verify, and install the ARM64 macOS
archive for version 0.1.0. Set `REPOSITORY` to this repository's GitHub
`owner/name`:

```console
set -eu
REPOSITORY=OWNER/shue
VERSION=0.1.0
TARGET=aarch64-apple-darwin
ARCHIVE="shue-v${VERSION}-${TARGET}.tar.gz"
curl -fLO "https://github.com/${REPOSITORY}/releases/download/v${VERSION}/${ARCHIVE}"
curl -fLO "https://github.com/${REPOSITORY}/releases/download/v${VERSION}/SHA256SUMS"
grep "  ${ARCHIVE}$" SHA256SUMS | shasum -a 256 -c -
tar -xzf "$ARCHIVE"
install -d "$HOME/.local/bin"
install -m 0755 "${ARCHIVE%.tar.gz}/shue" "$HOME/.local/bin/shue"
```

Ensure `$HOME/.local/bin` is on `PATH`, then run `shue --version`.

### Build from source

Building requires Rust 1.85 or newer and the native build tools required by the
`pcre2` crate.

From a checked-out source tree:

```console
cargo install --path crates/shue-cli --locked
```

For a local optimized build without installation:

```console
cargo build --release
./target/release/shue --help
```

## Maintainer release process

The tag-triggered workflow in `.github/workflows/release.yml` validates that a
tag such as `v0.2.0` exactly matches `workspace.package.version` in
`Cargo.toml`. It builds and smoke-tests all four native binaries, rejects a
macOS binary linked to a local Homebrew PCRE2 library, and enforces the locked
workspace tests on Linux and macOS with Rust 1.85.0 before publication. It
creates deterministic archives containing the applicable third-party license
notices and `SHA256SUMS`, validates the exact asset manifest, publishes the
GitHub release, tests the generated formula against those published assets,
and finally updates the tap. The tap update retries concurrent pushes but
refuses a version downgrade, same-version rewrite, or resurrection of a
deliberately removed formula. A version with a SemVer prerelease suffix, such
as `0.2.0-rc.1`, is marked as a GitHub prerelease and does not update the stable
Homebrew formula.

Before the first stable release:

1. Create and initialize a public `OWNER/homebrew-tap` repository. Its default
   branch must exist, and the release identity must be allowed to update it.
2. Add a fine-grained token as the source repository Actions secret
   `HOMEBREW_TAP_TOKEN`. Restrict it to the tap repository with only Contents
   read/write permission. The source repository's `GITHUB_TOKEN` cannot write
   to another repository.
3. If the tap is not `OWNER/homebrew-tap`, set the source repository Actions
   variable `HOMEBREW_TAP_REPOSITORY` to its `owner/homebrew-name`. The
   `homebrew-` repository prefix is required for Homebrew's short tap syntax.
4. Protect `v*` tags and workflow changes, and enable GitHub immutable releases.
   The publishing jobs use the `release` environment; configuring required
   reviewers on that environment adds a manual approval before publication.

For each release, update the workspace version, refresh `Cargo.lock`, run the
full verification suite, and merge that reviewed commit to the default branch.
Then create and push the matching annotated tag:

```console
git tag -a v0.2.0 -m "shue 0.2.0"
git push origin v0.2.0
```

The workflow uses Rust 1.85.0, locked dependencies, bundled static PCRE2, and
GitHub-hosted native runners. If a release job is retried, retry only the failed
jobs after correcting its external prerequisite; published release assets
must not be replaced after their checksums reach the Homebrew tap.

## Direct SSH usage

Everything after the first positional or unrecognized token is passed to SSH
as an OS-native argument, without UTF-8 conversion or shell evaluation:

```console
shue username@host
shue --color-depth truecolor -p 2222 username@host
shue -J bastion.example.net username@host show interfaces
```

Put shue's options before the first SSH argument. This boundary prevents a
remote command such as `shue host command --config remote-file` from having its
`--config` stolen by shue. `--` explicitly ends shue parsing when an SSH token
collides with a shue long option:

```console
shue -- --filter username@host
```

The SSH binary is resolved in this order:

1. `--ssh-path PATH`
2. `SHUE_SSH`
3. an executable named `ssh` found through `PATH`

For example:

```console
SHUE_SSH=/opt/openssh/bin/ssh shue username@host
shue --ssh-path /usr/bin/ssh username@host
```

No alias is involved, so SSH configuration, agents, host aliases, ProxyJump,
and ordinary SSH flags continue to be handled by the selected SSH client. The
child's exit status is shue's exit status. Runtime failures are written to
stderr with a `shue:` prefix.

## Other modes

Wrap any program with `--exec`; all arguments after `PROGRAM` pass through:

```console
shue --exec ping -c 4 192.0.2.1
shue --config ./rules.yaml --exec tail -f /var/log/system.log
```

Use `--filter` for pipelines. Highlighting remains enabled when stdout is a
pipe, matching ChromaTerm-style filter usage:

```console
journalctl -b | shue --filter | less -R
shue --config ./rules.yaml --filter < session.log
```

When both stdin and stdout are terminals, child programs run in a real PTY.
Local input is raw only while the child is active, terminal resizes propagate,
and a short idle flush makes prompts without newlines visible. Redirected use
runs with normal pipes and bounded 32 KiB reads. Shue forwards termination
signals and reaps the wrapped process; with non-terminal stdin it isolates the
child process group so helper processes are cleaned up as well, while terminal
stdin remains in the foreground process group for normal job control.

## Configuration discovery

The first available source wins. Explicit paths are required to exist, and a
selected file that cannot be read or decoded as UTF-8 is still an error because
there is no configuration to inspect. Content errors are non-fatal: shue emits
a concise `shue: warning:` diagnostic for each offending palette entry, rule,
or color group, ignores that part, and keeps every valid rule it can compile.
A wholly malformed YAML document produces a warning and an empty configuration,
so the wrapped command or filter continues with its output unchanged.

1. `--config PATH`
2. `SHUE_CONFIG`
3. `$XDG_CONFIG_HOME/shue/config.yaml`, then `config.yml`; when
   `XDG_CONFIG_HOME` is unset, `$HOME/.config/shue/config.yaml`, then
   `$HOME/.config/shue/config.yml`
4. for each absolute `XDG_CONFIG_DIRS` entry, `shue/config.yaml`, then
   `shue/config.yml`; the XDG default is `/etc/xdg`
5. `/etc/shue/config.yaml`, then `/etc/shue/config.yml`
6. embedded defaults

Automatic discovery only checks Shue-specific locations. Move an existing
configuration to `$XDG_CONFIG_HOME/shue/config.yaml`, or select another path
explicitly with `--config` or `SHUE_CONFIG`.

The embedded defaults are safe, theme-native rules for good/warning/error
states, IPv4 and IPv6 addresses, URLs, and standalone numbers. Shue never
creates a file or executes content from a config. A fully annotated config is
in [`examples/config.yaml`](examples/config.yaml).

The YAML shape is compatible with ChromaTerm2: top-level `palette` and `rules`,
and per-rule `description`, `regex`, scalar-or-group-map `color`, and
`exclusive` fields.

```yaml
palette:
  accent: '#5f87ff'

rules:
  - description: request result
    regex: 'status=(?<status>ok|failed)'
    color:
      status: 'f.accent bold'
    exclusive: true
```

## Regex support

Patterns use PCRE2 byte regexes, with JIT requested when available. This
supports positive and negative lookahead, lookbehind, numeric and named capture
groups, and backreference syntax such as `\1` or `\k<name>`. Group `0` means
the complete match; a color map can address numeric groups or a named capture.

```yaml
rules:
  - description: value bounded by markers
    regex: '(?<=BEGIN:)[A-F0-9]+(?=:END)'
    color: 'fg:cyan bold'

  - description: repeated word via named backreference
    regex: '\b(?<word>[A-Za-z]+)\s+\k<word>\b'
    color:
      word: underline
```

Lookbehind constraints depend on the linked PCRE2 version. Patterns rejected by
that engine produce a warning with the rule number; the rejected rule is
ignored while valid rules remain active. Matching is byte-oriented, so
arbitrary terminal output does not have to be valid UTF-8.

Configuration parsing and regex compilation fail open as described above. If a
compiled rule later reaches a PCRE2 runtime limit, shue also fails open: it
disables that rule for the rest of the process, prints one `shue: warning:`
diagnostic to stderr, and continues the wrapped session or filter output with
the remaining rules.

## Colors and terminal themes

Theme-native ANSI colors are the best choice when highlights should match the
terminal theme:

```yaml
color: 'fg:red bg:default bold'
```

Native names are `black`, `red`, `green`, `yellow`, `blue`, `magenta` (or
`purple`), `cyan`, `white`, their `bright-*` variants, and `default`. They use
the terminal's own ANSI palette at every color depth.

Custom colors accept ChromaTerm2 forms `f#rrggbb`, `b#rrggbb`, `f.name`, and
`b.name`, as well as `fg:#rrggbb`, `bg:#rrggbb`, `fg:rgb(r,g,b)`,
`bg:rgb(r,g,b)`, and `fg:<ansi-name>` / `bg:<ansi-name>`. Supported styles are
`bold`, `dim`, `italic`, `underline`, `blink`, `invert`, `hidden`, and `strike`.

`--color-depth` accepts:

- `auto` (default): inspect `COLORTERM` and `TERM`, conservatively falling back
  to `ansi16`
- `ansi16`: theme-native 16-color output
- `ansi256`: xterm 256-color output
- `truecolor`: 24-bit RGB output

RGB/hex colors degrade deterministically at lower depths. `SHUE_COLOR_DEPTH`
sets the default when the CLI option is absent. `--no-color` or the presence of
`NO_COLOR` disables added highlighting without changing the underlying bytes.

## Limitations

- Windows/ConPTY is not currently supported.
- In interactive mode, stdout and stderr share the PTY. In noninteractive mode,
  stdout is highlighted and stderr remains the child's ordinary stderr stream.
- An idle prompt flush commits the current streaming boundary; a regex cannot
  match across that particular boundary. Normal buffered streaming retains a
  bounded overlap for matches split across reads.
- PCRE2 features and lookbehind limits follow the linked PCRE2 version. Recovery
  is best-effort: a YAML syntax error that prevents parsing the document leaves
  no rules to recover, so shue warns and continues without added highlighting.
  Runtime-limit quarantine follows the same fail-open behavior.
- Terminal support for styles such as blink, italic, hidden, or strike varies.
- Shue forwards SSH arguments and exit codes but does not replace or interpret
  OpenSSH configuration.

## License and acknowledgements

Shue is licensed under the MIT License. See `LICENSE-MIT`.

The user experience and YAML compatibility are inspired by
[ChromaTerm2](https://github.com/rgcr/ChromaTerm2). Shue is an independent Rust
implementation; it does not claim code identity with ChromaTerm2.
