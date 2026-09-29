"""Turn a style read from one account into the body that creates it in another.

Reads {"style": <style>, "name": <string or null>} on stdin and writes
{"style": <body>, "warnings": [<string>, ...]} to stdout.
"""

import json
import re
import sys

# Set by the Styles API for the style it describes. A create either rejects
# them or ignores them, and the new style gets its own.
SERVER_FIELDS = ("id", "owner", "created", "modified", "visibility", "protected", "draft")


def owned_references(style, owner):
    """Every URL in the style that points at something the source account owns."""
    prefix = re.escape(owner)
    patterns = [
        ("sprite", re.compile(rf"^mapbox://sprites/{prefix}/")),
        ("glyphs", re.compile(rf"^mapbox://fonts/{prefix}/")),
    ]
    found = []

    sprite = style.get("sprite")
    sprites = [s.get("url") for s in sprite] if isinstance(sprite, list) else [sprite]
    for url in filter(None, sprites):
        if patterns[0][1].match(url):
            found.append(f"The sprite is {url}; its icons are not copied.")

    glyphs = style.get("glyphs")
    if glyphs and patterns[1][1].match(glyphs):
        found.append(f"The fonts come from {glyphs}; they are not copied.")

    for source_id, source in (style.get("sources") or {}).items():
        url = source.get("url") or ""
        if not url.startswith("mapbox://"):
            continue
        # A composite source lists several tilesets in one URL.
        for tileset in url[len("mapbox://"):].split(","):
            if tileset.startswith(f"{owner}."):
                found.append(f"Source `{source_id}` uses the tileset {tileset}; it is not copied.")

    for imported in style.get("imports") or []:
        url = imported.get("url") or ""
        if re.match(rf"^mapbox://styles/{prefix}/", url):
            found.append(f"Import `{imported.get('id')}` is the style {url}; it is not copied.")

    return found


def main():
    payload = json.load(sys.stdin)
    style = dict(payload["style"])
    owner = style.get("owner") or ""

    warnings = owned_references(style, owner) if owner else []
    for field in SERVER_FIELDS:
        style.pop(field, None)
    if payload.get("name"):
        style["name"] = payload["name"]

    json.dump({"style": style, "warnings": warnings}, sys.stdout)


if __name__ == "__main__":
    main()
