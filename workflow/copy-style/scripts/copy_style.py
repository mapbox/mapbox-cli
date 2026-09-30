"""Copy a style, with its fonts and icons, from one account to another.

Two steps share this file:

  copy_style.py fetch   reads {"style_id", "from_profile", "to_profile", "name"}
                        on stdin, downloads the style's ZIP as the source
                        account, and writes the plan to stdout. Reads only.
  copy_style.py apply   reads that plan on stdin and carries it out as the
                        target account: fonts, then the style, then its icons,
                        then the style's sprite and glyph URLs.

Under MAPBOX_WORKFLOW_DRY_RUN=1, `apply` writes nothing and says what it
would run instead. stdout is JSON only. On stderr, a line starting with
`::progress ` is what the step is doing now, shown beside the runner's
spinner; one starting with `::warn ` is a warning; every other line is a
detail, kept under the step.
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


def progress(message):
    note(f"::progress {message}")


def warn(message):
    note(f"::warn {message}")


def mapbox(login, *args, stdout=None, input=None):
    """Runs a mapbox command as a profile's login and returns its JSON output.

    `login` is (profile, account). `--use-login` because a MAPBOX_ACCESS_TOKEN
    in the environment would otherwise outrank the profile; `--username`
    because a MAPBOX_USERNAME would otherwise name a different account than
    the profile's token belongs to, and every request would be refused.
    """
    profile, account = login
    command = [CLI, *args, "--profile", profile, "--use-login", "--quiet"]
    if account:
        command += ["--username", account]
    if stdout is None:
        command += ["--output", "json"]
    done = subprocess.run(
        command,
        input=input.encode() if input is not None else None,
        stdout=stdout or subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if done.returncode != 0:
        raise Failed(f"`mapbox {' '.join(args[:2])}` as `{profile}` failed: {readable(done.stderr)}")
    if stdout is None:
        return json.loads(done.stdout) if done.stdout.strip() else None
    return None


def readable(stderr):
    """A child's error as a person would want it: the message and the fix,
    rather than the JSON document `--output json` prints."""
    text = stderr.decode(errors="replace").strip()
    try:
        error = json.loads(text.splitlines()[-1])
    except (ValueError, IndexError):
        return text
    if not isinstance(error, dict) or "message" not in error:
        return text
    return "\n  ".join(filter(None, [error["message"], error.get("fix")]))


def shown(login, *args):
    """A command line to show the user, which they can run as it stands."""
    profile, account = login
    words = [*args, "--profile", profile, "--use-login", "--username", account]
    quoted = [w if re.fullmatch(r"[A-Za-z0-9_./:@=-]+", w) else "'" + w.replace("'", "'\\''") + "'" for w in words]
    return " ".join(["mapbox", *quoted])


def counted(n, noun, plural=None):
    return f"{n} {noun if n == 1 else plural or noun + 's'}"


def login_of(profile):
    """The profile and the account its stored login belongs to."""
    status = mapbox((profile, None), "auth", "status") or {}
    account = status.get("stored_login")
    if not account:
        raise Failed(f"Profile `{profile}` is not logged in. Run `mapbox auth login --profile {profile}`.")
    return (profile, account)


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
    source = login_of(request["from_profile"])
    target = login_of(request["to_profile"])
    # Before the download: a target that cannot be written to is the likelier
    # failure, and the cheaper one to find.
    progress(f"checking the fonts in {target[1]}")
    existing = set(mapbox(target, "fonts", "list") or [])

    workdir = tempfile.mkdtemp(prefix="mapbox-copy-style-")
    try:
        plan = plan_copy(request, workdir, source, target, existing)
    except BaseException:
        shutil.rmtree(workdir, ignore_errors=True)
        raise
    json.dump(plan, sys.stdout)


def plan_copy(request, workdir, source, target, existing):
    archive = os.path.join(workdir, "style.zip")
    with open(archive, "wb") as out:
        progress(f"downloading {source[1]}/{request['style_id']}")
        mapbox(source, "styles", "download", request["style_id"], stdout=out)
    note(f"Downloaded {source[1]}/{request['style_id']} ({os.path.getsize(archive) // 1024} KB)")
    unpacked = os.path.join(workdir, "style")
    with zipfile.ZipFile(archive) as bundle:
        bundle.extractall(unpacked)

    with open(os.path.join(unpacked, "style.json")) as f:
        style = json.load(f)
    source_owner = style.get("owner") or source[1]
    target_owner = target[1]

    fonts_dir = os.path.join(unpacked, "fonts")
    fonts = sorted(
        f for f in (os.listdir(fonts_dir) if os.path.isdir(fonts_dir) else [])
        if f.lower().endswith(FONT_EXTENSIONS)
    )
    upload = [f for f in fonts if os.path.splitext(f)[0] not in existing]
    skip = [os.path.splitext(f)[0] for f in fonts if f not in upload]

    icons_dir = os.path.join(unpacked, "sprite_images")
    icons = sorted(f for f in (os.listdir(icons_dir) if os.path.isdir(icons_dir) else []) if f.endswith(".svg"))

    plan = {
        "workdir": workdir,
        "to_profile": target[0],
        "source_owner": source_owner,
        "target_owner": target_owner,
        "name": request.get("name") or style.get("name"),
        "fonts": {"upload": upload, "skip": skip},
        "icons": icons,
        "warnings": owned_references(style, source_owner) if source_owner else [],
    }
    note(f"Found {counted(len(icons), 'icon')} and {counted(len(fonts), 'custom font')}")
    for font in skip:
        note(f"Font {font} is already in {target_owner}; it will be skipped")
    for warning in plan["warnings"]:
        warn(warning)
    return plan


def style_body(plan):
    """The body that creates the copy.

    The Styles API checks a new style's sprite and refuses one the target
    account cannot read, which a private sprite of the source account is.
    So the copy is created with none and gets its own once its icons are
    uploaded. Its fonts are uploaded first, so glyphs can move now.
    """
    with open(os.path.join(plan["workdir"], "style", "style.json")) as f:
        style = json.load(f)
    for field in SERVER_FIELDS:
        style.pop(field, None)
    style.pop("sprite", None)
    style["name"] = plan["name"]
    return with_target_glyphs(style, plan)


def with_target_glyphs(style, plan):
    glyphs = style.get("glyphs")
    prefix = f"mapbox://fonts/{plan['source_owner']}/"
    if isinstance(glyphs, str) and glyphs.startswith(prefix):
        style["glyphs"] = f"mapbox://fonts/{plan['target_owner']}/" + glyphs[len(prefix):]
    return style


def pointed_at_target(style, plan, style_id):
    """The created style with its sprite and glyphs on the target account."""
    style = {k: v for k, v in style.items() if k not in SERVER_FIELDS}
    # Without the version hash: the API appends the current one itself.
    style["sprite"] = f"mapbox://sprites/{plan['target_owner']}/{style_id}"
    return with_target_glyphs(style, plan)


def batches(icons):
    return [icons[i:i + ICONS_PER_BATCH] for i in range(0, len(icons), ICONS_PER_BATCH)]


def cleanup_commands(login, created):
    commands = []
    if created["style"]:
        # Deleting the style removes its sprite, icons included.
        commands.append(shown(login, "styles", "delete", created["style"]))
    commands += [shown(login, "fonts", "delete", font) for font in created["fonts"]]
    return commands


def rehearse(plan):
    target = (plan["to_profile"], plan["target_owner"])
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
    note("\n".join(lines))

    return {
        "dry_run": True,
        "fonts": plan["fonts"],
        "icons": len(plan["icons"]),
        "would_run": would_run,
        "warnings": plan["warnings"],
    }


def carry_out(plan):
    target = (plan["to_profile"], plan["target_owner"])
    unpacked = os.path.join(plan["workdir"], "style")
    created = {"style": None, "fonts": [], "icons": 0}
    try:
        for f in plan["fonts"]["upload"]:
            font = os.path.splitext(f)[0]
            progress(f"uploading font {font}")
            mapbox(target, "fonts", "upload", "--file", os.path.join(unpacked, "fonts", f))
            created["fonts"].append(font)
            note(f"Uploaded font {font}")

        progress("creating the style")
        style = mapbox(target, "styles", "create", "--data", "@-", input=json.dumps(style_body(plan)))
        created["style"] = style["id"]
        note(f"Created style {plan['target_owner']}/{created['style']}")

        for group in batches(plan["icons"]):
            files = []
            for icon in group:
                files += ["--file", os.path.join(unpacked, "sprite_images", icon)]
            progress(f"uploading icons {created['icons']}/{len(plan['icons'])}")
            mapbox(target, "sprites", "upload-batch", created["style"], *files)
            created["icons"] += len(group)
        if plan["icons"]:
            note(f"Uploaded {counted(created['icons'], 'icon')} in {counted(len(batches(plan['icons'])), 'batch', 'batches')}")

        updated = pointed_at_target(style, plan, created["style"])
        progress("pointing the sprite and glyphs at the copy")
        mapbox(target, "styles", "update", created["style"], "--data", "@-", input=json.dumps(updated))
        note(f"Pointed its sprite and glyphs at {plan['target_owner']}")
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
