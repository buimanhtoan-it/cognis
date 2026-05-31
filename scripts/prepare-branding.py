"""Regenerate shared branding assets from assets/logo.png."""

from __future__ import annotations

from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]

SOURCE = ROOT / "assets" / "logo.png"

TARGETS = [
    ROOT / "packages" / "core" / "cognis" / "assets" / "logo.png",
    ROOT / "apps" / "cognis-vscode" / "media" / "logo.png",
]

MEDIA = ROOT / "apps" / "cognis-vscode" / "media"

ICON_SIZES = {
    "icon.png": 128,
    "sidebar.png": 28,
    "command.png": 48,
}


def main() -> None:

    if not SOURCE.is_file():
        raise SystemExit(f"Missing source logo: {SOURCE}")

    img = Image.open(SOURCE).convert("RGBA")

    for target in TARGETS:
        target.parent.mkdir(parents=True, exist_ok=True)

        img.save(target)

    MEDIA.mkdir(parents=True, exist_ok=True)

    for name, size in ICON_SIZES.items():
        resized = img.copy()

        resized.thumbnail((size, size), Image.Resampling.LANCZOS)

        canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))

        offset = ((size - resized.width) // 2, (size - resized.height) // 2)

        canvas.paste(resized, offset, resized)

        canvas.save(MEDIA / name)

    print(f"Updated branding assets from {SOURCE}")


if __name__ == "__main__":
    main()
