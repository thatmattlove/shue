#!/usr/bin/env python3
"""Render README PNG/GIF assets from real shue output. Requires Pillow."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
SGR = re.compile(r"\x1b\[([0-9;]*)m")
WIDTH, HEIGHT = 1200, 640
BACKGROUND = "#10151d"
FOREGROUND = "#d4dbe5"
MUTED = "#8d9aae"
# A terminal palette maps shue's native ANSI colors to visible RGB values.
PALETTE = [
    "#151a24", "#ff727e", "#a1df9a", "#f5cb7f",
    "#83b8ff", "#c8a0ed", "#79d5e8", "#d4dbe5",
    "#68778d", "#ff8993", "#b5edac", "#ffdc97",
    "#9dc8ff", "#dab8fc", "#9be4f2", "#f2f5fa",
]
COMMAND = "shue --filter < examples/demo.log"


def find_font(requested: str | None) -> str:
    candidates = [requested] if requested else [
        "/Library/Fonts/SourceCodePro_Regular.ttf",
        "/System/Library/Fonts/Menlo.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation2/LiberationMono-Regular.ttf",
    ]
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            return candidate
    raise SystemExit("No monospace font found; pass --font /path/to/font.ttf")


def capture(binary: Path, sample: str) -> str:
    # Use the embedded rules verbatim so personal/system configs cannot alter
    # the demonstration. All highlight positions and styles come from shue.
    source = (ROOT / "crates/shue-cli/src/config.rs").read_text()
    match = re.search(r'EMBEDDED_DEFAULT_CONFIG: &str = r#"(.*?)"#;', source, re.S)
    if match is None:
        raise SystemExit("Cannot locate embedded default rules")
    env = dict(os.environ)
    env.pop("NO_COLOR", None)
    with tempfile.TemporaryDirectory(prefix="shue-demo-") as temporary:
        config = Path(temporary) / "config.yaml"
        config.write_text(match[1])
        result = subprocess.run(
            [str(binary), "--config", str(config), "--color-depth", "ansi16", "--filter"],
            input=sample, capture_output=True, text=True, env=env, check=True,
        )
    if result.stderr:
        raise SystemExit(f"shue reported diagnostics: {result.stderr}")
    if SGR.sub("", result.stdout) != sample or not SGR.search(result.stdout):
        raise SystemExit("Expected highlighted output with unchanged source text")
    return result.stdout


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/shue")
    parser.add_argument("--font", help="Path to a monospace TTF/OTF font")
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error("Build shue first: cargo build --locked -p shue")
    sample = (ROOT / "examples/demo.log").read_text()
    highlighted = capture(args.binary.resolve(), sample)
    font_path = find_font(args.font)
    font = ImageFont.truetype(font_path, 24)
    bold_path = Path(font_path.replace("_Regular", "_Bold").replace("-Regular", "-Bold"))
    if bold_path == Path(font_path):
        bold_path = Path(font_path).with_stem(Path(font_path).stem + "-Bold")
    bold_font = ImageFont.truetype(str(bold_path), 24) if bold_path.is_file() else None
    small = ImageFont.truetype(font_path, 17)
    cell = font.getlength("M")
    if max(map(len, sample.splitlines())) * cell > WIDTH - 88:
        raise SystemExit("Demo text exceeds the terminal width with this font")
    if 143 + len(sample.splitlines()) * 36 > HEIGHT - 54:
        raise SystemExit("Demo text exceeds the terminal height")

    def terminal(command: str, output: str, *, label: str, cursor: bool = False) -> Image.Image:
        frame = Image.new("RGB", (WIDTH, HEIGHT), BACKGROUND)
        draw = ImageDraw.Draw(frame)
        draw.rectangle((0, 0, WIDTH, 59), fill="#19212d")
        draw.line((0, 59, WIDTH, 59), fill="#2a3545")
        for x, color in [(27, "#ff6b6b"), (51, "#f5c06c"), (75, "#8bcf8b")]:
            draw.ellipse((x, 24, x + 11, 35), fill=color)
        draw.text((WIDTH / 2, 30), "shue", font=small, fill=FOREGROUND, anchor="mm")
        draw.text((WIDTH - 27, 30), label, font=small, fill=MUTED, anchor="rm")
        draw.text((42, 87), "$", font=font, fill=PALETTE[2])
        draw.text((42 + 2 * cell, 87), command, font=font, fill=FOREGROUND)
        if cursor:
            x = 42 + (len(command) + 2) * cell
            draw.rectangle((x, 91, x + cell - 2, 115), fill=MUTED)

        x, y = 42.0, 143
        color, bold, underline = FOREGROUND, False, False
        for chunk in re.split(r"(\x1b\[[0-9;]*m)", output):
            sgr = SGR.fullmatch(chunk)
            if sgr:
                for code in map(int, sgr[1].split(";") if sgr[1] else [0]):
                    if code == 0:
                        color, bold, underline = FOREGROUND, False, False
                    elif code == 1:
                        bold = True
                    elif code == 4:
                        underline = True
                    elif code == 22:
                        bold = False
                    elif code == 24:
                        underline = False
                    elif code == 39:
                        color = FOREGROUND
                    elif 30 <= code <= 37:
                        color = PALETTE[code - 30]
                    elif 90 <= code <= 97:
                        color = PALETTE[code - 90 + 8]
                    else:
                        raise SystemExit(f"Unsupported ANSI style in demo: {code}")
                continue
            for character in chunk:
                if character == "\n":
                    x, y = 42.0, y + 36
                    continue
                if ord(character) < 32:
                    raise SystemExit("Unsupported terminal control in demo output")
                draw.text((x, y), character, font=bold_font if bold and bold_font else font,
                          fill=color, stroke_width=1 if bold and not bold_font else 0,
                          stroke_fill=color)
                if underline:
                    draw.line((x, y + 29, x + cell, y + 29), fill=color)
                x += cell
        draw.line((42, HEIGHT - 54, WIDTH - 42, HEIGHT - 54), fill="#263141")
        draw.text((42, HEIGHT - 36), "Sample network log", font=small, fill=MUTED)
        draw.text((WIDTH - 42, HEIGHT - 36), "Embedded default rules", font=small,
                  fill=MUTED, anchor="ra")
        return frame

    still = terminal(COMMAND, highlighted, label="with shue")
    output_dir = ROOT / "docs/assets"
    output_dir.mkdir(parents=True, exist_ok=True)
    still.save(output_dir / "shue-demo.png", optimize=True)

    frames = [terminal("cat examples/demo.log", sample, label="plain output")]
    durations = [1700]
    for end in range(0, len(COMMAND) + 3, 3):
        frames.append(terminal(COMMAND[:end], "", label="with shue", cursor=True))
        durations.append(80)
    lines = highlighted.splitlines(keepends=True)
    for count in range(1, len(lines) + 1):
        frames.append(terminal(COMMAND, "".join(lines[:count]), label="with shue"))
        durations.append(160)
    durations[-1] = 3800
    # One palette for every frame avoids color shifts and keeps the GIF small.
    palette = still.quantize(colors=128)
    indexed = [frame.quantize(palette=palette, dither=Image.Dither.NONE) for frame in frames]
    indexed[0].save(output_dir / "shue-demo.gif", save_all=True, append_images=indexed[1:],
                    duration=durations, loop=0, optimize=True, disposal=1)
    for name in ["shue-demo.png", "shue-demo.gif"]:
        path = output_dir / name
        print(f"{path.relative_to(ROOT)}: {path.stat().st_size:,} bytes")


if __name__ == "__main__":
    main()
