"""Copy a style, with its fonts and icons, from one account to another.

Two steps share this file:

  copy_style.py fetch   reads {"style_id", "from_profile", "to_profile", "name"}
                        on stdin, downloads the style's ZIP as the source
                        account, and writes the plan to stdout. Reads only.
  copy_style.py apply   reads that plan on stdin and carries it out as the
                        target account: fonts, then the style, then its icons,
                        then the style's sprite and glyph URLs.

Under MAPBOX_WORKFLOW_DRY_RUN=1, `apply` writes nothing and says what it
would run instead. Human-readable notes go to stderr; stdout is JSON only.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

CLI = os.environ.get("MAPBOX_CLI", "mapbox")
DRY_RUN = os.environ.get("MAPBOX_WORKFLOW_DRY_RUN") == "1"

# Set by the Styles API for the style it describes. A create either rejects
# them or ignores them, and the new style gets its own.
SERVER_FIELDS = ("id", "owner", "created", "modified", "visibility", "protected", "draft")

# The Styles API takes at most this many icons per batch upload.
ICONS_PER_BATCH = 25

FONT_EXTENSIONS = (".otf", ".ttf")


class Failed(Exception):
    pass


def note(message):
    print(message, file=sys.stderr, flush=True)


def mapbox(profile, *args, stdout=None):
    """Runs a mapbox command as `profile`'s login and returns its JSON output.

    `--use-login` because a MAPBOX_ACCESS_TOKEN in the environment would
    otherwise outrank the profile, and both accounts would be the same one.
    """
    command = [CLI, *args, "--profile", profile, "--use-login", "--quiet"]
    if stdout is None:
        command += ["--output", "json"]
    done = subprocess.run(
        command,
        stdout=stdout or subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=stdout is None,
    )
    if done.returncode != 0:
        detail = done.stderr if isinstance(done.stderr, str) else done.stderr.decode(errors="replace")
        raise Failed(f"`mapbox {' '.join(args[:2])}` failed: {detail.strip()}")
    if stdout is None:
        return json.loads(done.stdout) if done.stdout.strip() else None
    return None


def shown(profile, *args):
    """A command line to show the user, which they can run as it stands."""
    words = [*args, "--profile", profile, "--use-login"]
    quoted = [w if re.fullmatch(r"[A-Za-z0-9_./:@=-]+", w) else "'" + w.replace("'", "'\\''") + "'" for w in words]
    return " ".join(["mapbox", *quoted])


def counted(n, noun):
    return f"{n} {noun}" + ("" if n == 1 else "s")


def account_of(profile):
    status = mapbox(profile, "auth", "status") or {}
    account = status.get("stored_login")
    if not account:
        raise Failed(f"Profile `{profile}` is not logged in. Run `mapbox auth login --profile {profile}`.")
    return account


def owned_references(style, owner):
    """Tilesets and imports the source account owns, which are not copied."""
    found = []
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
        if url.startswith(f"mapbox://styles/{owner}/"):
            found.append(f"Import `{imported.get('id')}` is the style {url}; it is not copied.")
    return found


def fetch():
    request = json.load(sys.stdin)
    source, target = request["from_profile"], request["to_profile"]
    target_owner = account_of(target)

    workdir = tempfile.mkdtemp(prefix="mapbox-copy-style-")
    try:
        plan = plan_copy(request, workdir, target_owner)
    except BaseException:
        shutil.rmtree(workdir, ignore_errors=True)
        raise
    json.dump(plan, sys.stdout)


def plan_copy(request, workdir, target_owner):
    source, target = request["from_profile"], request["to_profile"]
    archive = os.path.join(workdir, "style.zip")
    with open(archive, "wb") as out:
        mapbox(source, "styles", "download", request["style_id"], stdout=out)
    unpacked = os.path.join(workdir, "style")
    with zipfile.ZipFile(archive) as bundle:
        bundle.extractall(unpacked)

    with open(os.path.join(unpacked, "style.json")) as f:
        style = json.load(f)
    source_owner = style.get("owner") or ""

    fonts_dir = os.path.join(unpacked, "fonts")
    fonts = sorted(
        f for f in (os.listdir(fonts_dir) if os.path.isdir(fonts_dir) else [])
        if f.lower().endswith(FONT_EXTENSIONS)
    )
    existing = set(mapbox(target, "fonts", "list") or [])
    upload = [f for f in fonts if os.path.splitext(f)[0] not in existing]
    skip = [os.path.splitext(f)[0] for f in fonts if f not in upload]

    icons_dir = os.path.join(unpacked, "sprite_images")
    icons = sorted(f for f in (os.listdir(icons_dir) if os.path.isdir(icons_dir) else []) if f.endswith(".svg"))

    plan = {
        "workdir": workdir,
        "to_profile": target,
        "source_owner": source_owner,
        "target_owner": target_owner,
        "name": request.get("name") or style.get("name"),
        "fonts": {"upload": upload, "skip": skip},
        "icons": icons,
        "warnings": owned_references(style, source_owner) if source_owner else [],
    }
    note(
        f"Downloaded {source_owner}/{request['style_id']}: {counted(len(icons), 'icon')}, "
        f"{counted(len(fonts), 'custom font')} ({len(skip)} already in {target_owner})."
    )
    return plan


def style_body(plan):
    with open(os.path.join(plan["workdir"], "style", "style.json")) as f:
        style = json.load(f)
    for field in SERVER_FIELDS:
        style.pop(field, None)
    style["name"] = plan["name"]
    return style


def pointed_at_target(style, plan, style_id):
    """The style with its sprite and glyphs moved to the target account."""
    style = {k: v for k, v in style.items() if k not in SERVER_FIELDS}
    # Without the version hash: the API appends the current one itself.
    style["sprite"] = f"mapbox://sprites/{plan['target_owner']}/{style_id}"
    glyphs = style.get("glyphs")
    prefix = f"mapbox://fonts/{plan['source_owner']}/"
    if isinstance(glyphs, str) and glyphs.startswith(prefix):
        style["glyphs"] = f"mapbox://fonts/{plan['target_owner']}/" + glyphs[len(prefix):]
    return style


def batches(icons):
    return [icons[i:i + ICONS_PER_BATCH] for i in range(0, len(icons), ICONS_PER_BATCH)]


def cleanup_commands(profile, created):
    commands = []
    if created["style"]:
        # Deleting the style removes its sprite, icons included.
        commands.append(shown(profile, "styles", "delete", created["style"]))
    commands += [shown(profile, "fonts", "delete", font) for font in created["fonts"]]
    return commands


def rehearse(plan):
    target = plan["to_profile"]
    fonts_dir = os.path.join(plan["workdir"], "style", "fonts")
    groups = batches(plan["icons"])
    would_run = [shown(target, "fonts", "upload", "--file", os.path.join(fonts_dir, f)) for f in plan["fonts"]["upload"]]
    would_run.append(shown(target, "styles", "create", "--data", "@-"))
    would_run += [shown(target, "sprites", "upload-batch", "<new style id>", "--file", "…") for _ in groups]
    would_run.append(shown(target, "styles", "update", "<new style id>", "--data", "@-"))

    lines = [f"Would copy the style into {plan['target_owner']} as \"{plan['name']}\":"]
    for f in plan["fonts"]["upload"]:
        lines.append(f"  upload font {os.path.splitext(f)[0]}")
    for font in plan["fonts"]["skip"]:
        lines.append(f"  skip font {font} (already in {plan['target_owner']})")
    lines.append("  create the style")
    lines.append(f"  upload {len(plan['icons'])} icons in {len(groups)} batches")
    lines.append(f"  point its sprite and glyphs at {plan['target_owner']}")
    note("\n".join(lines) + "\n")

    return {
        "dry_run": True,
        "fonts": plan["fonts"],
        "icons": len(plan["icons"]),
        "would_run": would_run,
        "warnings": plan["warnings"],
    }


def carry_out(plan):
    target = plan["to_profile"]
    unpacked = os.path.join(plan["workdir"], "style")
    created = {"style": None, "fonts": [], "icons": 0}
    try:
        for f in plan["fonts"]["upload"]:
            mapbox(target, "fonts", "upload", "--file", os.path.join(unpacked, "fonts", f))
            created["fonts"].append(os.path.splitext(f)[0])

        body = json.dumps(style_body(plan))
        result = subprocess.run(
            [CLI, "styles", "create", "--data", "@-", "--profile", target, "--use-login", "--quiet", "--output", "json"],
            input=body, capture_output=True, text=True,
        )
        if result.returncode != 0:
            raise Failed(f"`mapbox styles create` failed: {result.stderr.strip()}")
        style = json.loads(result.stdout)
        created["style"] = style["id"]

        for group in batches(plan["icons"]):
            files = []
            for icon in group:
                files += ["--file", os.path.join(unpacked, "sprite_images", icon)]
            mapbox(target, "sprites", "upload-batch", created["style"], *files)
            created["icons"] += len(group)

        updated = pointed_at_target(style, plan, created["style"])
        result = subprocess.run(
            [CLI, "styles", "update", created["style"], "--data", "@-", "--profile", target, "--use-login", "--quiet", "--output", "json"],
            input=json.dumps(updated), capture_output=True, text=True,
        )
        if result.returncode != 0:
            raise Failed(f"`mapbox styles update` failed: {result.stderr.strip()}")
    except Failed as failure:
        lines = [str(failure)]
        if created["style"] or created["fonts"]:
            lines.append("")
            lines.append(f"Created in {plan['target_owner']} before the failure:")
            if created["style"]:
                lines.append(f"  style {created['style']}, with {created['icons']} of {len(plan['icons'])} icons")
            for font in created["fonts"]:
                lines.append(f"  font {font}")
            lines.append("")
            lines.append("Nothing was removed. To remove what was created:")
            lines += [f"  {command}" for command in cleanup_commands(target, created)]
        note("\n".join(lines))
        sys.exit(1)

    return {
        "id": created["style"],
        "name": plan["name"],
        "owner": plan["target_owner"],
        "fonts": {"uploaded": created["fonts"], "skipped": plan["fonts"]["skip"]},
        "icons": created["icons"],
        "warnings": plan["warnings"],
    }


def apply():
    plan = json.load(sys.stdin)
    try:
        result = rehearse(plan) if DRY_RUN else carry_out(plan)
    finally:
        shutil.rmtree(plan["workdir"], ignore_errors=True)
    json.dump(result, sys.stdout)


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    try:
        if mode == "fetch":
            fetch()
        elif mode == "apply":
            apply()
        else:
            sys.exit(f"usage: {sys.argv[0]} fetch|apply")
    except Failed as failure:
        note(str(failure))
        sys.exit(1)


if __name__ == "__main__":
    main()
