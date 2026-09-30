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

The first four steps are `mapbox` commands, the last two a script:

1. **Check the source login** and 2. **the target login**: `auth status` for each profile, which names the account its login belongs to.
3. **Download the style**: `styles download` as `from_profile`, kept as `style.zip` in the run's working directory with `save`.
4. **List the target account's fonts**: `fonts list` as `to_profile`.
5. **Plan the copy** (`scripts/copy_style.py plan`): unpacks the ZIP and lists the fonts to upload (fonts the target already has are skipped, by name), the icons, and anything the copy still depends on the source account for.
6. **Copy the fonts, the style and its icons** (`scripts/copy_style.py apply`), as `to_profile`: `fonts upload` for each new font, `styles create`, `sprites upload-batch` in batches of 25, then `styles update` to point the style's sprite and glyphs at the target account. A script, because it loops.

`--dry-run` runs steps 1 to 5 for real, since none of them writes anything, and step 6 without writing, so it shows exactly which fonts would be uploaded or skipped, how many icons, and every command step 6 would run.

The source account needs access to the style download API, which Mapbox grants on request. Tilesets and imported styles that belong to the source account are not copied; the copy can use them only if the target account can read them, and the `warnings` output lists each one.

If step 6 fails partway, nothing is removed. The error lists the style and fonts it created and the commands that remove them, for example:

```
Created in target-account before the failure:
  style ckcopy000000000000000001, with 250 of 561 icons
  font Yellow Banana Regular

Nothing was removed. To remove what was created:
  mapbox styles delete ckcopy000000000000000001 --profile target --use-login --username target-account
  mapbox fonts delete 'Yellow Banana Regular' --profile target --use-login --username target-account
```

Each command runs as its profile's login and names that login's account with `--username`, so a `MAPBOX_ACCESS_TOKEN` or `MAPBOX_USERNAME` in the environment cannot send a request as, or to, a different account.

Needs `python3` on `PATH`.
