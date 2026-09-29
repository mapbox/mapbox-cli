# copy-style

Copies a style from one Mapbox account to another.

```sh
mapbox auth login --profile source
mapbox auth login --profile target
mapbox workflow install copy-style
mapbox workflow run copy-style \
  --style-id <style id> \
  --from-profile source \
  --to-profile target
```

1. `styles get` reads the style as `from_profile`.
2. `scripts/prepare.py` removes the fields the Styles API sets itself (`id`, `owner`, `created`, `modified`, `visibility`, `protected`, `draft`), applies `name` if given, and lists every tileset, font, sprite and import that belongs to the source account.
3. `styles create` creates the copy as `to_profile`.

Only the style document is copied. Tilesets, fonts, sprite icons and imported styles that belong to the source account stay there, and the copy can use them only if the target account can read them. The `warnings` output lists each one, so you can tell what to upload again.

Needs `python3` on `PATH`.
