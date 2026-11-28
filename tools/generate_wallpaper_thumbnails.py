"""Regenerate gallery-only previews of the bundled wallpaper assets (requires Pillow)."""
from pathlib import Path

from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parents[1]
WALLPAPERS = ROOT / "crates/arcnix-core/assets/wallpapers"


def main():
    output = WALLPAPERS / "thumbnails"
    output.mkdir(exist_ok=True)
    for source in sorted(WALLPAPERS.glob("*.webp")):
        with Image.open(source) as image:
            preview = ImageOps.fit(image.convert("RGB"), (320, 180), Image.Resampling.LANCZOS)
            preview.save(output / source.name, "WEBP", quality=80, method=6)
    print("Generated 320x180 wallpaper previews")


if __name__ == "__main__":
    main()
