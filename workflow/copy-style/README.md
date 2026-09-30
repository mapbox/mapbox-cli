# copy-style

Copies a style from one Mapbox account to another, with its custom fonts and icons.

```sh
mapbox auth login --profile source
mapbox auth login --profile target
mapbox workflow install copy-style
mapbox workflow run copy-style \
  --style-id <style id> \
  --from-profile source \
  --to-profile target
```

1. **Download the style and plan the copy.** `styles download` fetches the style's ZIP as `from_profile`. The plan lists the fonts to upload (fonts `to_profile` already has are skipped, by name), the icons, and anything the copy still depends on the source account for.
2. **Copy the fonts, the style and its icons**, as `to_profile`: `fonts upload` for each new font, `styles create`, `sprites upload-batch` in batches of 25, then `styles update` to point the style's sprite and glyphs at the target account.

`--dry-run` runs step 1 for real and step 2 without writing anything, so it shows exactly which fonts would be uploaded or skipped, how many icons, and every command step 2 would run.

The source account needs access to the style download API, which Mapbox grants on request. Tilesets and imported styles that belong to the source account are not copied; the copy can use them only if the target account can read them, and the `warnings` output lists each one.

If step 2 fails partway, nothing is removed. The error lists the style and fonts it created and the commands that remove them, for example:

```
Created in target-account before the failure:
  style ckcopy000000000000000001, with 250 of 561 icons
  font Yellow Banana Regular

Nothing was removed. To remove what was created:
  mapbox styles delete ckcopy000000000000000001 --profile target --use-login
  mapbox fonts delete 'Yellow Banana Regular' --profile target --use-login
```

Needs `python3` on `PATH`.
