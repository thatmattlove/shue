# Pinned binary-distribution notices

The release archives aggregate these files into `THIRD-PARTY-LICENSES.txt`.
They cover statically linked components that Cargo package metadata does not
fully describe.

- `PCRE2-10.46-LICENCE.md` is the complete `LICENCE.md` from PCRE2's signed
  10.46 tag at commit `b2bd4254b379b9d7dc9a3dda060a7e27009ccdff`:
  <https://github.com/PCRE2Project/pcre2/blob/b2bd4254b379b9d7dc9a3dda060a7e27009ccdff/LICENCE.md>.
  Its reviewed SHA-256 is
  `9cf7ac6976099a1d856826d3ef1b093bd6b84489dc6100628ac79e740cf9885a`.
- `MUSL-1.2.3-COPYRIGHT` is `COPYRIGHT` from the official musl 1.2.3 source
  archive at <https://musl.libc.org/releases/musl-1.2.3.tar.gz>. Its reviewed
  SHA-256 is
  `f9bc4423732350eb0b3f7ed7e91d530298476f8fec0c6c427a1c04ade22655af`.

The release helper also collects normal Cargo dependency notices, the exact
vendored SLJIT notice, and the Rust 1.85.0 toolchain `COPYRIGHT` file. Hash and
version checks fail closed when one of these reviewed inputs changes, prompting
the notices to be audited alongside dependency or toolchain updates.
