# Implemented commands

Every command the CLI ships: four auth commands, 37 API operations across 13
command groups, the tilesets-cli proxy, `completion` and `generate-skills`. Each is
shown in both of its renderings. Which one you get is decided by `--output`, whose default
(`auto`) reads stdout: a terminal gets the left column, a pipe or redirect
gets the right one. See
[README's output section](../README.md#output-format) for the rules.

Account names, style ids and tokens in the examples are replaced; everything
else is as the API sent it.

**33 of the 37 were run against the live API and show what came back:**
`directions`, `isochrone`, `map-matching`, `matrix`, `feedback list` and
`feedback get` on 2026-09-24, once those
command groups existed at all, and the rest earlier — `fonts list`,
`fonts upload` and `fonts delete` on 2026-09-08 once
`fonts:list`/`fonts:write` became registrable, the remainder before that.
The write operations were exercised as round trips on throwaway objects —
a style created, updated, drafted and deleted; icons uploaded to a sprite
and taken out again; a font uploaded and deleted — leaving the account as
it was found.

Every API command's **Outputs** block below is that snapshot rather than a
live reading, and is re-taken by hand — nothing schedules it and nothing
enforces it. The auth, `completion`, `generate-skills` and tilesets-cli blocks
are not captures: those are the CLI's own rendering, which the test suite does
cover.
Nothing re-checks the captures in between, because what
the Mapbox APIs return is not this repo's to monitor. What *is* ours — the
commands and the flags they take — is held to `mapbox --schema` on every
`cargo test` run by `tests/docs_contract.rs`, so the half of this page that
can be checked cannot fall behind the binary.

The remaining 4 give the response shape from the spec or the docs instead
of a live capture: they're all `search`'s — read-only and safe to run, but
the credentials used to write this page have no Search Box API access, so
every call answers 401 rather than a result.

Each **Parameters** section lists only what is specific to its command. The
globals every API command takes are
[in one table](#what-every-api-command-takes).

## Contents

Every command, as `group.command` — `styles.draft.get` is the one that
nests, and is typed `mapbox styles draft get`.

**[Auth](#auth)** — [auth.login](#mapbox-auth-login) ·
[auth.logout](#mapbox-auth-logout) · [auth.refresh](#mapbox-auth-refresh) ·
[auth.whoami](#mapbox-auth-whoami)

**[Agent skills](#agent-skills)** —
[agent-skills.list](#mapbox-agent-skills-list) ·
[agent-skills.install](#mapbox-agent-skills-install) ·
[agent-skills.update](#mapbox-agent-skills-update) ·
[agent-skills.uninstall](#mapbox-agent-skills-uninstall)

**[Completion](#completion)** — [completion](#mapbox-completion)

**[Generate skills](#generate-skills)** —
[generate-skills](#mapbox-generate-skills)

**[Uninstall](#uninstall)** — [uninstall](#mapbox-uninstall)

**[Config](#config)** — [config.get](#mapbox-config-get) ·
[config.set](#mapbox-config-set) · [config.list](#mapbox-config-list) ·
[config.unset](#mapbox-config-unset)

**[Usage](#usage)** — [usage](#mapbox-usage)

**[Accounts](#accounts)** —
[accounts.list-tokens](#mapbox-accounts-list-tokens) ·
[accounts.retrieve-token](#mapbox-accounts-retrieve-token) ·
[accounts.list-scopes](#mapbox-accounts-list-scopes)

**[Directions](#directions)** — [directions](#mapbox-directions)

**[Feedback](#feedback)** — [feedback.list](#mapbox-feedback-list) ·
[feedback.get](#mapbox-feedback-get)

**[Fonts](#fonts)** — [fonts.list](#mapbox-fonts-list) ·
[fonts.upload](#mapbox-fonts-upload) · [fonts.delete](#mapbox-fonts-delete)

**[Geocoder](#geocoder)** —
[geocoder.forward](#mapbox-geocoder-forward) ·
[geocoder.reverse](#mapbox-geocoder-reverse) ·
[geocoder.batch](#mapbox-geocoder-batch)

**[Isochrone](#isochrone)** — [isochrone](#mapbox-isochrone)

**[Map Matching](#map-matching)** — [map-matching](#mapbox-map-matching)

**[Matrix](#matrix)** — [matrix](#mapbox-matrix)

**[Search](#search)** — [search.forward](#mapbox-search-forward) ·
[search.reverse](#mapbox-search-reverse) ·
[search.category](#mapbox-search-category) ·
[search.list-category](#mapbox-search-list-category)

**[Sprites](#sprites)** — [sprites.get-json](#mapbox-sprites-get-json) ·
[sprites.upload](#mapbox-sprites-upload) ·
[sprites.upload-batch](#mapbox-sprites-upload-batch) ·
[sprites.delete](#mapbox-sprites-delete) ·
[sprites.delete-batch](#mapbox-sprites-delete-batch)

**[Static](#static)** — [static.get-image](#mapbox-static-get-image) ·
[static.get-tile](#mapbox-static-get-tile)

**[Styles](#styles)** — [styles.list](#mapbox-styles-list) ·
[styles.get](#mapbox-styles-get) · [styles.create](#mapbox-styles-create) ·
[styles.update](#mapbox-styles-update) ·
[styles.delete](#mapbox-styles-delete) ·
[styles.draft.get](#mapbox-styles-draft-get) ·
[styles.draft.update](#mapbox-styles-draft-update) ·
[styles.draft.delete](#mapbox-styles-draft-delete)

**[Tilesets](#tilesets)** —
[tilesets.get-tile](#mapbox-tilesets-get-tile) ·
[tilesets.get-mvt](#mapbox-tilesets-get-mvt) ·
[tilesets.query](#mapbox-tilesets-query)

**[Tilesets CLI](#tilesets-cli)** — [tilesets-cli](#mapbox-tilesets-cli-args)

Then [Errors](#errors) — the shape a failure takes in each mode.

---

## Auth

Credentials live in `~/.mapbox`, one file per profile — or in whatever
directory `MAPBOX_CONFIG_DIR` names, when it is set.

All four commands take:

| Parameter | Effect |
| --- | --- |
| `--profile <name>` | Which credential file to act on. Default `default`. |
| `--output`, `-o` | `auto` \| `text` \| `json`. |

`login`, `logout` and `refresh` take `--dry-run` as well. `whoami` does not,
for the reason its own section gives.

### `mapbox auth login`

Registers an OAuth client, opens the browser for the authorization code, and
exchanges it for a token.

#### Examples

```sh
mapbox auth login                    # default profile
mapbox auth login --profile work     # a second account, side by side
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Logged in as user.
Tip: export MAPBOX_USERNAME=user to skip
--username on each command.
```

</td><td>

```json
{"logged_in":true,"username":"user","profile":"default"}
```

</td></tr>
</table>

Progress, including the URL to visit if the browser does not open, goes to
stderr in both modes.

### `mapbox auth logout`

Deletes the stored credentials file for the profile.

#### Examples

```sh
mapbox auth logout
mapbox auth logout --profile work -o json
```

#### Outputs

Succeeds either way; the boolean says whether there was anything to delete.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Logged out successfully.
```

</td><td>

```json
{"logged_out":true,"profile":"default"}
```

</td></tr>
<tr><td>

```
Not currently logged in.
```

</td><td>

```json
{"logged_out":false,"profile":"scratch"}
```

</td></tr>
</table>

### `mapbox auth refresh`

Forces a token refresh regardless of expiry, under the per-profile credential
lock.

#### Examples

```sh
mapbox auth refresh
mapbox auth refresh --profile work --debug
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Token refreshed. The new token expires in 59 minutes.
```

</td><td>

```json
{"expires_at":1788276540,"profile":"default","refreshed":true}
```

</td></tr>
</table>

### `mapbox auth whoami`

Reports the token the *next* command will use and whose it is, which is not
always the login you remember making.

It reads the stored credentials without refreshing them, so asking who you
are cannot spend the single-use refresh token or wait on another
invocation's lock. A stored token already inside the five-minute refresh
window is therefore reported as expiring — the next real command is what
refreshes it.

#### Parameters

Everything reported below is read out of the token locally, and reading a
token cannot tell a revoked one from a live one: a revoked token still
carries a readable account and a future expiry.

| Parameter | Effect |
| --- | --- |
| `--verify` | Ask Mapbox about the token instead of only reading it. |

`--verify` adds a `Verified: TokenValid` line, and turns every other verdict
into a failure with a code of its own — `token_expired`, `token_revoked`,
`token_invalid`, `token_malformed` — so a script can tell a token that needs
re-issuing from one that was never a token at all. It is the only part of
this command that makes a request.

There is no `--dry-run`: the command reads the store and reports, and
`--verify` asks about a token rather than changing one.

#### Examples

```sh
mapbox auth whoami
mapbox auth whoami --profile work
mapbox auth whoami --verify
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Account:  user
Source:   mapbox auth login (profile `default`)
Token:    temporary (tk), expires in 59 minutes
```

</td><td>

```json
{"account":"user","env_var":null,"expires_at":1788276540,"profile":"default","source":"login","stored_login":"user","usage":"tk","verified":null}
```

</td></tr>
</table>

`MAPBOX_ACCESS_TOKEN` outranks a login, and the report names the source it
resolved rather than whichever it happened to read first. A `Login:` line
appears when there is a stored login that is not the one in use, and the
mismatch is warned about on stderr in both modes:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Account:  someone-else
Source:   MAPBOX_ACCESS_TOKEN (environment)
Token:    public (pk), no expiry
Login:    `user` stored under profile `default`, not in use
```

</td><td>

```json
{"account":"someone-else","env_var":"MAPBOX_ACCESS_TOKEN","expires_at":null,"profile":"default","source":"environment","stored_login":"user","usage":"pk","verified":null}
```

</td></tr>
</table>

With nothing to report it exits non-zero under the code
`not_authenticated`, so `mapbox auth whoami && …` means what it looks like:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Error: No Mapbox token available.
Fix: Run `mapbox auth login`, export MAPBOX_ACCESS_TOKEN, or pass `--token`.
Next: mapbox auth login
Docs: https://docs.mapbox.com/api/accounts/tokens/
```

</td><td>

```json
{"code":"not_authenticated","docs":["https://docs.mapbox.com/api/accounts/tokens/"],"fix":"Run `mapbox auth login`, export MAPBOX_ACCESS_TOKEN, or pass `--token`.","message":"No Mapbox token available.","next_actions":["mapbox auth login"]}
```

</td></tr>
</table>

---

## API command groups

33 operations across 10 command groups. Nine are generated from the OpenAPI specs
vendored in `openapi/`; `search` is the one exception — a hand-authored
spec versioned in this repo's own `custom-openapi/`, see
`custom-openapi/README.md`.

**A command group is not a spec file.** Which command group an operation belongs to is
decided per operation, by an `x-mapbox-cli-command` extension the sync
writes onto it, not by which file it was parsed from — so `sprites` is
five operations out of the styles spec, and `tilesets` is one operation
each out of the raster-tiles and vector-tiles specs. Two names that used to
be command groups, `maps` and `vectortiles`, are gone because their last
operation moved to `tilesets`.

| Command group | Operations |
| --- | --- |
| [`accounts`](#accounts) | **3** |
| [`fonts`](#fonts) | **3** |
| [`geocoder`](#geocoder) | **3** |
| [`search`](#search) | **4** |
| [`sprites`](#sprites) | **5** |
| [`static-images`](#static-images) | **3** |
| [`static-tiles`](#static-tiles) | **1** |
| [`styles`](#styles) | **8** |
| [`tilequery`](#tilequery) | **1** |
| [`tilesets`](#tilesets) | **2** |

The [Contents](#contents) list above names every one of the 33.

**Everything else the Mapbox specs describe is not here at all.** Not
hidden, not shipped as a command that refuses: absent from the spec content
this binary compiles against.
A maintainer-only decision record tracks which operations are enabled, and
the vendoring step that derives `openapi/` strips everything it does not
mark `enabled`. So an operation left out answers exactly as a mistyped
name does, and there is nothing in `--help`, `--schema` or this page to
suggest otherwise.

Three kinds of reason sit behind those decisions, and they are worth
telling apart:

- **No token can carry the scope.** `POST /oauth/register` refuses
  `fonts:metadata`, `tokens:write` and `styles:download`, so a login can
  never obtain them. `src/spec.rs`'s `UNSUPPORTED_OPERATIONS` records
  which operations need one, and an entry comes off that list once the
  scope becomes registrable — nothing here can force that.
  `fonts:list` and `fonts:write` used to be on that list —
  both became registrable on 2026-09-08, which is what shipped
  `fonts list`, `fonts upload` and `fonts delete`. `styles
  download-style-zip`'s real blocker turned out to be one level deeper:
  production answers it 403 "This is a prerelease API. Please contact
  support," regardless of scope — access is granted per-account by Mapbox
  support, not by OAuth, so adding `styles:download` to the allowlist would
  not unblock it by itself.
- **Withheld deliberately.** `styles set-style-protected` unlocks a style
  for deletion — a live token holding `styles:protect` (which *is*
  registrable) can call it successfully, this CLI just declines to offer a
  one-line command with no confirmation for something that dangerous;
  a couple of `styles` operations are admin-only and gated by role rather
  than by scope — a normal login gets a bare 403 with no scope named, even
  for an account that belongs to Mapbox; `fonts get-model-asset` only needs `fonts:read`, already
  granted, but reaches a product surface nothing here can exercise — no
  reachable account has a model to fetch; the raster-tiles spec's
  `get-legacy-grid` and `get-legacy-tile` are the retired v1–v3 API, which
  cannot succeed with any token this CLI can hold. `WITHHELD_OPERATIONS` in
  `src/spec.rs` keeps the reasons next to the names.
- **Curated out of the spec itself.** `search`'s `suggest` and
  `retrieve/{id}` are in neither list — they were never written into
  `custom-openapi/search/openapi/search.yaml` to begin with, since both need
  a caller-managed `session_token` for a client-side autocomplete flow this
  CLI has no one-shot equivalent of. A reader following
  `UNSUPPORTED_OPERATIONS` or `WITHHELD_OPERATIONS` into `src/spec.rs` to
  ask why finds nothing about either name; the answer is in the spec file
  itself.

Both `src/spec.rs` lists still filter what reaches the command tree, and
both currently remove nothing: the sync that derives `openapi/` already
took every operation they name out of it. They stay because a later sync
can re-enable one, and because the reason an endpoint is unusable belongs
beside the endpoint.

Several command groups also define a liveness probe, at the group's root or a
conventional health-check path. Those are for whatever monitors it
rather than for a caller, so none is a command — and none is in `openapi/`
either.

### What every API command takes

| Parameter | Effect |
| --- | --- |
| `--token`, `-t` | Access token. Falls back to `MAPBOX_ACCESS_TOKEN`, then stored credentials. |
| `--username`, `-u` | Fills `{username}`/`{owner}`/`{account}` path placeholders. Falls back to `MAPBOX_USERNAME`, then to the logged-in user — so it can be left off entirely once you are signed in. Examples below pass it anyway, to show where it lands. |
| `--use-login` | Ignore `MAPBOX_ACCESS_TOKEN`; use stored credentials. |
| `--profile <name>` | Which stored credentials to use. |
| `--output`, `-o` | `auto` \| `text` \| `json`. |
| `--id <value>` | On a command that returns a list, print just the row with that `id` or `name`. |
| `--timeout <seconds>` | How long one request may take, connection included. Defaults to 60 seconds, or 900 for a body read from `--file` or from a `--data @<path>`/`@-`. Also `MAPBOX_TIMEOUT`. |

An operation with a request body takes `--data`/`-d` when that body is text
the caller types — JSON for most, a bare `true`/`false` for `star-file` —
and `--file <PATH>` when it is bytes: raw for `application/octet-stream` and
`image/svg+xml`, one repeatable part for `multipart/form-data`. Two
operations declare both and reject having both passed. Each command's own
**Parameters** below lists only what is specific to it.

`--data` does not have to carry the body itself. Following curl,
**`@<path>` reads a file and `@-` reads stdin**:

```sh
mapbox styles create --data @style.json
jq '.name = "Renamed"' style.json | mapbox styles update STYLE_ID --data @-
```

Only the first character decides, so `--data '{"contact":"a@b.example"}'` is
still the body it looks like. A body whose *first* character is a literal `@`
cannot be passed this way — curl has the same limitation, and it costs
nothing here because every operation that takes `--data` sends JSON, and `@`
is not valid JSON.

Three things worth knowing about the read forms:

- **The body is sent byte for byte.** The trailing newline a text editor
  leaves is insignificant to a JSON parser and is not stripped, because
  trimming a body the caller supplied would be the CLI editing what it was
  asked to send.
- **The timeout changes with it.** A typed `--data` is capped by the command
  line at a megabyte or so and gets the 60-second budget; `@<path>` and `@-`
  are unbounded and get the same 900 seconds `--file` does.
- **`@-` suppresses the confirmation on a delete.** Of the five operations
  that take `--data`, only `mapbox sprites delete-batch` is a `DELETE`, and
  a question needs stdin to be a terminal — which a pipe is not. So piping a
  body into it sends it unasked, exactly as `< file` always did. Use
  `@<path>` rather than `@-` to keep the prompt, or pass `--yes` to say the
  answer deliberately.

A command that changes something takes `--dry-run`, which prints the request
it would send, on stdout, and sends nothing. Which commands those are is not
a list anyone keeps: it is every `POST`, `PUT`, `PATCH` and `DELETE` — 12 of
the 33 operations — plus `auth login`, `auth logout`, `auth refresh` and
`generate-skills`. A read-only `GET` does not take it, so `mapbox styles
list --dry-run` is a usage error rather than a no-op. It rehearses
rather than describes: `--data` is parsed and every `--file` is read, so a
body that will not parse or a path that cannot be read fails under it too.
Like `--data`, it goes after the operation name.

Two shapes, one with almost nothing and one with most of it:

```sh
mapbox geocoder forward --q Helsinki

mapbox --use-login --profile work styles create --username user \
  --data '{"name":"My Style","version":8,"sources":{},"layers":[]}'
```

### How a response is rendered

Under `json` a response goes out as one compact line, untouched — indented
instead when `-o json` is asked for at a terminal, since then a person is
reading it.

`-o json` promises that everything on stdout is JSON. It does not promise how
many documents: a command with one result emits one, and a command that
*streams* emits one per line — JSON Lines. Which one you get is a property of
the command, not of the flag, the same way a command that answers with a PNG
answers with a PNG in every mode. There is no `-o jsonl`. No command streams
today; when one exists it will say so in its own section here, and every
command that does not stream will keep emitting exactly one document, which
the test suite checks on every run.

Under `text` it is rendered by shape, decided from the response itself
rather than from the spec:

| Response | Rendered as |
| --- | --- |
| An array of like objects | A table, one row each |
| One key holding an array of like objects | The same table — `{"icons":[…]}` is still a listing |
| A single object | A field list, one level of nesting flattened onto dotted keys |
| No body at all | A confirmation naming what happened — `Deleted <id>.`, or `{"ok":true,…}` |
| A `search`, `geocoder` or `tilequery` `FeatureCollection` | A numbered list, one entry per feature — see the paragraph below |
| Anything else | Pretty-printed JSON — every other command group's GeoJSON, style documents and bare values lose their meaning in a table |

A table shows the columns most rows have, that vary, and that do not repeat
another column; identifiers keep their full width and everything else narrows
to fit. Underneath it says what it clipped and how to see one row whole.

`search`'s GeoJSON `FeatureCollection` is a deliberate exception to
"anything else": `forward`, `reverse` and `category` each return a list of
POIs meant to be scanned, so it renders — rather than staying JSON the way
most other command groups' GeoJSON does. Not as a table, though: a table's
column is one fixed width for every row, and a street address is exactly
the field that width can't be chosen for without clipping almost every one
of them to a few characters and an ellipsis. So it's a numbered list
instead — name, its POI category (or `feature_type` for a result with
none, e.g. an address) and distance on one line, the whole address on the
next, `longitude,latitude` on the one after that (the next thing a caller
usually wants a result for), never clipped:

```
1. Golden Gate Bridge (bridge, landmark) — 8710.3 km
   Golden Gate Bridge, Sausalito, California, United States
   -122.4783,37.8199

2. Kalve Coffee Golden Gate (café, coffee, coffee shop) — 91.3 km
   Ahtri tn 6, 10151 Tallinn, Estonia
   24.7454,59.437
```

`geocoder`'s and `tilequery`'s `FeatureCollection`s render as a numbered list
for the same reason — see their own sections for the shape. The match is on
those three command-group names exactly, so another command group that answers with
GeoJSON keeps falling to pretty-printed JSON; its nesting is the information
a list or table would throw away. A feature whose properties give it nothing
to show falls the whole collection back to JSON rather than print a blank
numbered entry — a feature that also carries a geometry still keeps its
coordinate line, since the guard checks what the row ended up with, not the
properties directly. A conforming response never reaches that case:
`geocoder` requires `name`/`feature_type` on every feature, `tilequery`
requires `tilequery.layer`.

Three of the ten command groups can answer with bytes — `static-images`,
`static-tiles` and `tilesets`. Those bypass `--output` in
both modes:

<table>
<tr><th width="50%">Terminal — refuses</th><th width="50%">Redirected — raw bytes</th></tr>
<tr><td>

```
Error: Response is image/png (48213 bytes).
Refusing to write it to the terminal — redirect
it to a file, e.g. `... > out.png`.
```

</td><td>

```
$ mapbox static-images get-static-image … > map.png
$ file map.png
map.png: PNG image data, 600 x 400
```

</td></tr>
</table>

#### One page at a time

Several listings are paginated by the API, which returns one page and a
`Link` header naming the next. **The CLI says so rather than leaving the
result looking complete**, and names the flags that fetch the next page:

```
$ mapbox accounts list-tokens --username user --limit 2
ID                         NOTE            CREATED     USAGE
cmtoken00000000000000001a  CI deploy key   2026-09-04  sk
cmtoken00000000000000002b  Local dev       2026-09-03  pk

Tips:
  `-o json` for the response as the API sent it.
  To see one row: add `--id cmtoken00000000000000001a`
  More results: add `--limit 2 --start cmtoken00000000000000002b` for the next page.
```

Under `-o json` the same note is the only thing printed to stderr on a
success, and as the lone tip it takes the singular form:

```
$ mapbox accounts list-tokens --username user --limit 2 -o json > page1.json
Tip: More results: add `--limit 2 --start cmtoken00000000000000002b` for the next page.
```

The flags are derived from the response, not hardcoded: whatever the spec
calls an operation's paging parameters is what the line names. The access
token is never among them, even though the API echoes it back in that header.

Two details worth knowing:

- **The note goes to stderr in both modes**, including `-o json`. The result
  is just as partial there, and the API's own document cannot carry the fact
  without an envelope this CLI has promised not to add — so a `-o json`
  consumer reading stdout alone is unaffected, and one watching stderr is
  told. There is no `--all` yet; following the pages is the caller's job.
- **`--id` searches the page it was given.** On a paginated listing a miss
  means "not on this page", which is not the same as "does not exist", so
  the error says which and how to look further:

  ```
  Error: No row has the id `cmtoken00000000000000009z`.
  Fix: This is one page of results, so the id may be on a later one. Add
  `--start cmtoken00000000000000002b --limit 2` to search the next page.
  ```

---

## Accounts

Mapbox Tokens API. Three operations; the three that write tokens need
`tokens:write`, which is not registrable.

### `mapbox accounts list-tokens`

Lists the access tokens for an account. Secret (`sk`) entries omit the
`token` string. Needs `tokens:read`.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--limit <n>` | How many to return. |
| `--start <id>` | Continue after this token id — the paging cursor. |
| `--sortby <created\|modified>` | Sort order. |
| `--usage <pk\|sk\|tk>` | Only tokens of that kind. |
| `--default` | Only the account's default token. |

Results are paginated. When more exist the CLI prints the `--start` value to
continue from — see [One page at a time](#one-page-at-a-time).

#### Examples

```sh
mapbox accounts list-tokens --username user
mapbox accounts list-tokens --username user --limit 2
mapbox accounts list-tokens --username user --usage sk --sortby created
mapbox accounts list-tokens --username user --id cmtoken00000000000000001a
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
ID                         CLIENT  CREATED     NOTE
cmtoken00000000000000001a  api     2026-06-10  CI token
cmtoken00000000000000002b  api     2026-06-05  Studio-crea…

Tips:
  Values are shortened to fit; `-o json` prints each row whole.
  To see one row: add --id cmtoken00000000000000001a
```

</td><td>

```json
[{"client":"api","created":"2026-06-10T09:10:53.850Z","default":false,"id":"cmtoken00000000000000001a","note":"CI token","scopes":["styles:read","fonts:read"],"usage":"pk"}]
```

</td></tr>
</table>

The API omits the `token` string from `sk` rows, so a secret token's string
never comes back at all. `pk` and `tk` rows do carry it, and a listing where
most rows have one — `--usage pk`, say — gets a `TOKEN` column, clipped to
the column width like any other value.

`--usage` and `--limit` together return an empty list from the API,
whatever the limit is:

```sh
mapbox accounts list-tokens --username user --usage pk            # 3 rows
mapbox accounts list-tokens --username user --usage pk --limit 10 # 0 rows
```

Same from `curl`, so it is the Tokens API rather than the CLI. Use one or
the other.

### `mapbox accounts retrieve-token`

Checks whether a token is valid and reports what it carries. The token being
checked is the one the command authenticates with, so this is "what am I
holding" rather than a lookup by id.

#### Examples

```sh
mapbox accounts retrieve-token
mapbox accounts retrieve-token --token pk.eyJ1Ijoi…
mapbox accounts retrieve-token --profile work -o json
```

#### Outputs

Always HTTP 200 — `code` carries the verdict: `TokenValid`,
`TokenMalformed`, `TokenInvalid`, `TokenExpired` or `TokenRevoked`.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
code           TokenValid
token.client   9f3c1ab2…7d40e6c8
token.created  2026-09-01T15:50:16.000Z
token.expires  2026-09-01T23:50:16.000Z
token.scopes   styles:tiles, styles:read, styles:write,
               styles:list, fonts:read, datasets:read…
               (+7 more)
token.usage    tk
token.user     user

Tip: `-o json` for the response as the API sent it.
```

</td><td>

```json
{"code":"TokenValid","token":{"client":"9f3c1ab2…7d40e6c8","created":"2026-09-01T15:50:16.000Z","expires":"2026-09-01T23:50:16.000Z","scopes":["styles:tiles","styles:read","styles:write"],"usage":"tk","user":"user"}}
```

</td></tr>
</table>

The scope list is the thing worth reading here: a 403 from any other command
is usually a scope missing from this list.

### `mapbox accounts list-scopes`

Lists the token scopes the account may request. Needs `scopes:list`.

#### Examples

```sh
mapbox accounts list-scopes --username user
mapbox accounts list-scopes --username user --id styles:download
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
ID             DESCRIPTION
scopes:list    List all available scopes.
map:read       Read tilesets, tilestats, legac…
user:read      Read user profile information.
styles:read    Read styles.
styles:list    List styles.
```

</td><td>

```json
[{"description":"List all available scopes.","id":"scopes:list"},{"description":"Read styles.","id":"styles:read"}]
```

</td></tr>
</table>

What this lists is what the account is *allowed* to hold, which is not the
same as what the current token holds — `retrieve-token` answers that.

---
## Directions

Turn-by-turn routes between 2-25 waypoints, for driving (with or without
live traffic), walking, or cycling. Curated by hand down to the parameters
documented at docs.mapbox.com/api/navigation/directions — see
`custom-openapi/README.md` for why this command group doesn't come from the
vendored specs the way most others do. The ~30 electric-vehicle-routing
parameters (`engine=electric` and everything under it) are deliberately not
here: those describe a vehicle's charging curve down to the watt, which is
data an integration passes in from a vehicle profile, not something to
hand-type as CLI flags.

### `mapbox directions`

A route between 2-25 waypoints, in the order given — a route through fixed
stops, not a traveling-salesman solve. No subcommand: this API has one
operation, so there's nothing a second word would disambiguate — the same
reason `mapbox usage` has none either.

#### Parameters

`<routing-profile>` and `<coordinates>` (both positional) are required.
`<routing-profile>` is sent exactly as typed, not checked against a fixed
list: `mapbox/driving-traffic`, `mapbox/driving`, `mapbox/walking` and
`mapbox/cycling` are documented, but some accounts (OEM agreements, mainly)
have additional profiles of their own that were never published — the API
is the authority on whether a value is valid, not this page. `<coordinates>`
is 2-25 `{longitude},{latitude}` pairs, semicolon-separated.

| Parameter | Effect |
| --- | --- |
| `--alternatives` | Return up to 2 alternative routes alongside the primary one. |
| `--annotations <fields>` | Segment-level metadata per leg, comma-separated (`distance`, `duration`, `speed`, `congestion`, `congestion_numeric`, `maxspeed`, `closure`, `state_of_charge`). Requires `--overview full`. |
| `--avoid-maneuver-radius <1-1000>` | Meters around the start to avoid a significant maneuver within. |
| `--bearings <angle,degrees;...>` | Filter road segments by direction of travel, one entry per coordinate. |
| `--layers <int;...>` | A road layer (Z-order) per waypoint, for multi-level roads. |
| `--continue-straight` | Keep going straight at an intermediate waypoint rather than u-turning back to it. |
| `--exclude <types>` | Road types or `point(lon lat)` values to route around, comma-separated (`motorway`, `toll`, `ferry`, `unpaved`, `cash_only_tolls`, `country_border`, `state_border`, `tunnel`). |
| `--geometries <geojson\|polyline\|polyline6>` | Route geometry format. Defaults to `polyline`. |
| `--include <types>` | Special road types to allow, comma-separated (`hov2`, `hov3`, `hot`). |
| `--overview <full\|simplified\|false>` | Geometry detail level. Defaults to `simplified`. |
| `--radiuses <meters\|unlimited;...>` | Max snap distance to the road network, one per coordinate. |
| `--approaches <unrestricted\|curb;...>` | Which side of the road to approach each waypoint from. |
| `--steps` | Return turn-by-turn instructions. Several flags below only take effect with this set. |
| `--banner-instructions` | Return banner objects for display. Requires `--steps`. |
| `--language <tag>` | Instruction language. Defaults to `en`. Requires `--steps`. |
| `--roundabout-exits` | Separate entry/exit instructions for a roundabout. Requires `--steps`. |
| `--voice-instructions` | Return SSML-marked voice guidance. Requires `--steps`. |
| `--voice-units <imperial\|british_imperial\|metric>` | Requires `--steps` and `--voice-instructions`. |
| `--waypoints <indices>` | Which coordinates get their own arrival instruction — must include `0` and the last index. Requires `--steps`. |
| `--waypoints-per-route` | Nest each route's waypoints under that route object instead of once at the top level. |
| `--waypoint-names <names;...>` | A name per waypoint for its arrival instruction. Requires `--steps`. |
| `--waypoint-targets <lon,lat;...>` | A drop-off point per waypoint, when it differs from the routed-to coordinate. Requires `--steps`. |
| `--notifications <all\|none>` | Whether to return route notification/warning metadata. |
| `--alley-bias <-1..1>` | `mapbox/driving` only. |
| `--arrive-by <ISO 8601>` | `mapbox/driving` only. |
| `--depart-at <ISO 8601>` | `mapbox/driving` and `mapbox/driving-traffic`. |
| `--max-height <0-10>` / `--max-width <0-10>` / `--max-weight <0-100>` | `mapbox/driving` and `mapbox/driving-traffic`. Meters, meters, metric tons. |
| `--snapping-include-closures` / `--snapping-include-static-closures` | `mapbox/driving-traffic` only. |
| `--walking-speed <0.14-6.94>` / `--walkway-bias <-1..1>` | `mapbox/walking` only. |

#### Examples

```sh
mapbox directions mapbox/driving "-122.42,37.78;-122.45,37.91"
mapbox directions mapbox/walking "-122.42,37.78;-122.43,37.79" \
  --steps --geometries geojson --overview full --annotations distance,duration
```

The negative longitude is not treated as a flag here, even without `--`
before it — `coordinates` is one of the parameters this CLI recognizes a
leading `-` on and lets through, the same way `search`'s `--proximity` and
`--bbox` already do (see `HYPHEN_LEADING_VALUE_PARAMS` in `src/main.rs`).

#### Outputs

Captured live against `mapbox/driving` between two San Francisco points.
`routes`/`legs`/`waypoints` are nested arrays of objects, which
`output::emit`'s text mode has no bespoke summary for (unlike the GeoJSON
`FeatureCollection` responses `search` and `geocoder` return) — both modes
print the same JSON, `-o text` pretty-printed and `-o json` on one line:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```json
{
  "code": "Ok",
  "routes": [
    {
      "distance": 26966.74,
      "duration": 2552.258,
      "geometry": "{{qeFvdejVqdChZ…",
      "legs": [
        {
          "distance": 26966.74,
          "duration": 2552.258,
          "steps": [],
          "summary": "US 101 North, Paradise Drive",
          "weight": 3068.861
        }
      ],
      "weight": 3068.861,
      "weight_name": "auto"
    }
  ],
  "uuid": "…",
  "waypoints": [
    { "distance": 11.034, "location": [-122.420122, 37.779978], "name": "US 101 North" },
    { "distance": 1820.457, "location": [-122.453429, 37.893872], "name": "" }
  ]
}
```

</td><td>

```json
{"code":"Ok","routes":[{"distance":26966.74,"duration":2552.258,"geometry":"{{qeFvdejVqdChZ…","legs":[{"distance":26966.74,"duration":2552.258,"steps":[],"summary":"US 101 North, Paradise Drive","weight":3068.861}],"weight":3068.861,"weight_name":"auto"}],"uuid":"…","waypoints":[{"distance":11.034,"location":[-122.420122,37.779978],"name":"US 101 North"},{"distance":1820.457,"location":[-122.453429,37.893872],"name":""}]}
```

</td></tr>
</table>

Both trimmed to one leg for length — the real response also carries
`admins` (administrative boundaries traversed) and `notifications` (three
tunnel alerts, on this particular route) per leg.

---
## Feedback

Feedback submitted against Mapbox API responses — geocoding, search,
directions and the rest — filterable, sortable, and paginated. Curated by
hand down to the parameters documented at docs.mapbox.com/api/feedback —
see `custom-openapi/README.md` for why this command group doesn't come from
the vendored specs the way most others do.

**`feedback create`, the write side of this API, is not a command.** It
needs a `user-feedback:write` scope that `POST /oauth/register` silently
drops from the granted set — confirmed directly against production, the
same shape `accounts create-token` and `styles download-style-zip` already
document. No `mapbox auth login` token can ever carry it.

### `mapbox feedback list`

Every feedback item on the account, newest received first by default.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--feedback-id <ids>` | One or more feedback ids, comma-separated. |
| `--after <cursor>` | Page forward from a previous response's `end_cursor`. |
| `--limit <n>` | Maximum items to return, up to 1000. |
| `--sort-by <received_at\|created_at\|updated_at>` | Which timestamp to sort by. Defaults to `received_at`. |
| `--order <asc\|desc>` | Sort direction. Defaults to `asc`. |
| `--status <statuses>` | One or more of `received`, `fixed`, `reviewed`, `out_of_scope`, comma-separated. |
| `--category <cats>` | One or more feedback categories, comma-separated — account-specific, no fixed list. |
| `--search <text>` | A phrase to match against feedback text. |
| `--trace-id <ids>` | One or more caller-provided trace ids, comma-separated. |
| `--created-before` / `--created-after <ISO 8601>` | Window on when the caller created the item. |
| `--received-before` / `--received-after <ISO 8601>` | Window on when Mapbox received it. |
| `--updated-before` / `--updated-after <ISO 8601>` | Window on when it was last updated. |

#### Examples

```sh
mapbox feedback list --limit 5
mapbox feedback list --status received --category positioning_issue
```

#### Outputs

Captured live, two items:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```json
{
  "items": [
    {
      "id": "01a06d61-17e4-74aa-b824-13baaf272670",
      "status": "received",
      "category": "positioning_issue",
      "feedback": "This is a test feedback. …",
      "location": { "lat": 0, "lon": 0 },
      "received_at": "2026-09-04T17:04:34.818Z"
    },
    {
      "id": "01a06d61-77cc-7649-8db6-5beb2de0278d",
      "status": "received",
      "category": "application_issue",
      "feedback": "This is a test feedback. …",
      "location": {
        "lat": 37.779238,
        "lon": -122.419359,
        "place_name": "400 Van Ness Avenue, San Francisco, California 94103, United States"
      },
      "received_at": "2026-09-04T17:04:59.466Z"
    }
  ],
  "has_after": true,
  "has_before": false,
  "start_cursor": "…",
  "end_cursor": "…"
}
```

</td><td>

```json
{"items":[{"id":"01a06d61-17e4-74aa-b824-13baaf272670","status":"received","category":"positioning_issue","feedback":"This is a test feedback. …","location":{"lat":0,"lon":0},"received_at":"2026-09-04T17:04:34.818Z"},{"id":"01a06d61-77cc-7649-8db6-5beb2de0278d","status":"received","category":"application_issue","feedback":"This is a test feedback. …","location":{"lat":37.779238,"lon":-122.419359,"place_name":"400 Van Ness Avenue, San Francisco, California 94103, United States"},"received_at":"2026-09-04T17:04:59.466Z"}],"has_after":true,"has_before":false,"start_cursor":"…","end_cursor":"…"}
```

</td></tr>
</table>

Neither output mode has a bespoke rendering for this response — it isn't
GeoJSON — so both print the same JSON, `-o text` pretty-printed and `-o
json` on one line. Feedback text trimmed and `created_at`/`updated_at`/
`has_screenshot` dropped per item, for length; the real response carries
them too.

### `mapbox feedback get`

One feedback item by id.

#### Parameters

`<feedback-id>` (positional) is required.

#### Examples

```sh
mapbox feedback get 01a06d61-17e4-74aa-b824-13baaf272670
```

#### Outputs

Captured live, the same item `list` returned above — a single object this
time, not wrapped in `items`:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```json
{
  "id": "01a06d61-17e4-74aa-b824-13baaf272670",
  "status": "received",
  "category": "positioning_issue",
  "feedback": "This is a test feedback. …",
  "location": { "lat": 0, "lon": 0 },
  "received_at": "2026-09-04T17:04:34.818Z"
}
```

</td><td>

```json
{"id":"01a06d61-17e4-74aa-b824-13baaf272670","status":"received","category":"positioning_issue","feedback":"This is a test feedback. …","location":{"lat":0,"lon":0},"received_at":"2026-09-04T17:04:34.818Z"}
```

</td></tr>
</table>

Same trimming as `list` above.

---
## Fonts

The fonts an account owns. Three operations.

All three need `fonts:list` or `fonts:write`, which became registrable
on 2026-09-08. All three were run live on 2026-09-08 with a token
from `mapbox auth login` — a round trip: uploaded, listed, deleted — and
this page's captures below are that run. Anyone who ran `mapbox auth login`
before this release needs to run it again: the scope set is fixed when the
login client registers, so a refreshed token cannot pick up a scope added
afterward.

### `mapbox fonts list`

The font faces an account owns. Cached: a font just uploaded may not appear
for some window afterward, per the API's own `Cache-Control`.

#### Examples

```sh
mapbox fonts list --username user
mapbox fonts list --username user -o json
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
[
  "Open Sans Regular"
]
```

</td><td>

```json
["Open Sans Regular"]
```

</td></tr>
</table>

An account with nothing uploaded:

```
(none)

Tip: `-o json` for the response as the API sent it.
```

### `mapbox fonts upload`

Upload a font face. Requires `fonts:write`.

#### Parameters

`--file` is the font's own bytes (`.ttf`, `.otf`, or similar), sent raw —
not JSON, not multipart. Roughly 30MB is the API's own limit.

`--dry-run` reads the file to confirm it exists and is readable, and prints
the request it would send, without uploading anything.

#### Examples

```sh
mapbox fonts upload --file "./Open Sans Regular.ttf" --username user
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
family_name  Open Sans
hash         0000000000000000000000000000000000000a
owner        user
style_name   Regular
visibility   private
```

</td><td>

```json
{"family_name":"Open Sans","hash":"0000000000000000000000000000000000000a","owner":"user","style_name":"Regular","visibility":"private"}
```

</td></tr>
</table>

Uploading again — same account, same face — succeeds the same way rather
than conflicting; the second upload replaces the first.

### `mapbox fonts delete`

Delete a font face. Requires `fonts:write`. Asks for confirmation at a
terminal, like every `DELETE`; `--yes` skips it.

#### Parameters

`<face>` is the full face name, family and style together — the same
spelling `fonts list` reports.

`--dry-run` prints the request without sending it and without asking.

#### Examples

```sh
mapbox fonts delete "Open Sans Regular" --username user
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Deleted Open Sans Regular.
```

</td><td>

```json
{"command":"fonts delete","ok":true,"status":204}
```

</td></tr>
</table>

Returns 204 whether or not the font existed, so a second delete of the same
face answers exactly the same way — there is nothing in the response body
to tell the two apart.

## Geocoder

Places to coordinates and back. Geocoding v6.

All three return GeoJSON, rendered as a numbered list under `-o text` —
name and feature type on one line, the full address on the next,
`longitude,latitude` on the one after that (the next thing a caller
usually wants a result for), never clipped. The response's `attribution`,
the terms the results come under, follows the list — once under `batch`'s
whole result rather than once under each of up to fifty identical copies.
`batch` gets one such list per query, under a `Query N:` header — omitted
when the batch held a single query, since there is nothing to tell it apart
from.

### `mapbox geocoder forward`

Looks up a location from search text, and returns its standardized address,
geographic context and coordinates.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--q <text>` | The search string. |
| `--limit <n>` | How many results. Default 5, max 10. |
| `--country <codes>` | ISO 3166-1 alpha-2, comma-separated. |
| `--types <types>` | `address`, `place`, `postcode`, `poi`, … |
| `--proximity <lon,lat>` | Bias results towards a point. |
| `--bbox <minlon,minlat,maxlon,maxlat>` | Restrict to a box. |
| `--language <tag>` | IETF language tag. |
| `--autocomplete` | Partial-input matching. |
| `--worldview <code>` | Which country's view of disputed borders. |
| `--permanent` | The result may be stored. Billed differently. |
| `--format <format>` | `geojson` (default), or `v5` for the older response shape. |
| `--entrances` | Include the building entrances of address features. Public Preview. |

Structured input is an alternative to `--q`: `--address-number`,
`--street`, `--place`, `--region`, `--postcode`, `--locality`,
`--neighborhood`, `--address-line1`, `--block`. Do not combine them with
`--q`.

#### Examples

```sh
mapbox geocoder forward --q Helsinki --limit 1
mapbox geocoder forward --q "1600 Pennsylvania Ave" --country us --types address
mapbox geocoder forward --street "Kaivokatu" --place Helsinki --country fi
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
1. Helsinki (place)
   Helsinki, Uusimaa, Finland
   24.941822,60.167507

NOTICE: © 2026 Mapbox and its suppliers. All rights reserved. This response and the information it contains may not be retained.

Tip: `-o json` for the response as the API sent it.
```

</td><td>

```json
{"type":"FeatureCollection","attribution":"NOTICE: © 2026 Mapbox and its suppliers. All rights reserved. This response and the information it contains may not be retained.","features":[{"geometry":{"coordinates":[24.941822,60.167507],"type":"Point"},"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}}]}
```

</td></tr>
</table>

### `mapbox geocoder reverse`

Looks up the features at a pair of coordinates.

#### Parameters

`--longitude` and `--latitude` are required. `--limit`, `--types`,
`--country`, `--language`, `--worldview` and `--permanent` narrow the result
the same way they do for forward geocoding.

#### Examples

```sh
mapbox geocoder reverse --longitude 24.94 --latitude 60.16
mapbox geocoder reverse --longitude -74.0 --latitude 40.7 --types address
```

A negative coordinate is a value, not a flag. That took a fix — clap read
`-74.0` as a cluster of short options and rejected the command, which made
every coordinate west of Greenwich unusable.

#### Outputs

The same numbered list as forward geocoding.

### `mapbox geocoder batch`

Up to 50 forward or reverse queries in one request. Each query is an object
in a JSON array, with what would have been query parameters as its fields.

#### Parameters

`--data`/`-d` carries the array. `--permanent` applies to the whole batch.

#### Examples

```sh
mapbox geocoder batch -d '[
  {"types":["place"],"q":"Helsinki"},
  {"types":["place"],"q":"Tampere"},
  {"longitude":24.94,"latitude":60.16}
]'
```

#### Outputs

A `batch` array, one entry per query, in the order sent. Under `-o text`
each query's results render as their own numbered list, same as
`forward`/`reverse`, under a `Query N:` header — dropped
when there is only one query, where the header names the only thing on
screen:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Query 1:
1. Helsinki (place)
   Helsinki, Uusimaa, Finland
   24.941822,60.167507

Query 2:
1. Tampere (place)
   Tampere, Pirkanmaa, Finland
   23.757025,61.4981

NOTICE: © 2026 Mapbox and its suppliers. All rights reserved. This response and the information it contains may not be retained.

Tip: `-o json` for the response as the API sent it.
```

</td><td>

```json
{"batch":[{"type":"FeatureCollection","features":[…],"attribution":"NOTICE: …"},{"type":"FeatureCollection","features":[…],"attribution":"NOTICE: …"}]}
```

</td></tr>
</table>

Every entry carries its own `attribution` and it is always the same notice,
so it is printed once under the whole batch. Two that genuinely differed
would both be shown.

A query malformed enough that its own list can't be built falls the whole
batch back to pretty-printed JSON, same as one broken feature does for a
single query.

---
## Isochrone

How far you can get from a point in a given time or distance, for driving
(with or without live traffic), walking, or cycling. Curated by hand down to
the parameters documented at docs.mapbox.com/api/navigation/isochrone — see
`custom-openapi/README.md` for why this command group doesn't come from the
vendored specs the way most others do.

### `mapbox isochrone`

One contour per value in `--contours-minutes` or `--contours-meters`, as
GeoJSON around the given center point. No subcommand: this API has one
operation, so there's nothing a second word would disambiguate, the same
reason `mapbox directions` has none either.

#### Parameters

`<routing-profile>` and `<coordinates>` (both positional) are required.
`<routing-profile>` is sent exactly as typed, not checked against a fixed
list: `mapbox/driving-traffic`, `mapbox/driving`, `mapbox/walking` and
`mapbox/cycling` are documented, but some accounts (OEM agreements, mainly)
have additional profiles of their own that were never published, the API
is the authority on whether a value is valid, not this page. `<coordinates>`
is one `{longitude},{latitude}` pair, unlike `mapbox directions`, this
command takes a single center point, not a list of waypoints.

Exactly one of `--contours-minutes` or `--contours-meters` is required by
the API, though nothing here enforces it before the request goes out.

| Parameter | Effect |
| --- | --- |
| `--contours-minutes <mins>` | Up to 4 times in minutes, 1-60, comma-separated and increasing. One contour per value. |
| `--contours-meters <meters>` | Up to 4 distances in meters, 1-100000, comma-separated and increasing. One contour per value. |
| `--contours-colors <hex,...>` | A hex color per contour (no `#`), comma-separated — must match the contour count. |
| `--polygons` | Return each contour as a GeoJSON polygon instead of a linestring. |
| `--denoise <0.0-1.0>` | A smaller value removes more of the smaller contours. Defaults to 1.0. |
| `--generalize <meters>` | Douglas-Peucker simplification tolerance — a higher value is a coarser, smaller contour. |
| `--exclude <types>` | Road types to route around, comma-separated (`motorway`, `toll`, `ferry`, `unpaved`, `cash_only_tolls`). |
| `--depart-at <ISO 8601>` | For `mapbox/driving-traffic`, which live traffic conditions to route against. |

#### Examples

```sh
mapbox isochrone mapbox/driving "-122.42,37.78" --contours-minutes 5,10,15
mapbox isochrone mapbox/walking "-122.42,37.78" --contours-minutes 5,10 --polygons
```

#### Outputs

Captured live against `mapbox/walking`, two 5- and 10-minute contours as
polygons. This response is a real GeoJSON `FeatureCollection`, unlike
`mapbox directions`'s response, but isochrone isn't one of the three
services (`search`, `geocoder`, `tilequery`) this CLI has a bespoke
list-per-feature rendering for yet (`output.rs`'s `list_rendering` is an
exact service allow-list, not a "looks like GeoJSON" test), so both output
modes print the same JSON, `-o text` pretty-printed and `-o json` on one
line, same shape as `mapbox directions`'s Outputs section above:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```json
{
  "type": "FeatureCollection",
  "features": [
    {
      "type": "Feature",
      "properties": {
        "color": "#bf4040",
        "contour": 10,
        "fill": "#bf4040",
        "fill-opacity": 0.33,
        "fillColor": "#bf4040",
        "fillOpacity": 0.33,
        "metric": "time",
        "opacity": 0.33
      },
      "geometry": { "type": "Polygon", "coordinates": "…" }
    }
  ]
}
```

</td><td>

```json
{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"color":"#bf4040","contour":10,"fill":"#bf4040","fill-opacity":0.33,"fillColor":"#bf4040","fillOpacity":0.33,"metric":"time","opacity":0.33},"geometry":{"type":"Polygon","coordinates":"…"}}]}
```

</td></tr>
</table>

Trimmed to one of the two features (the response has one per
`--contours-minutes` value) and the polygon's coordinates, for length.

---
## Map Matching

Snaps a noisy GPS trace to the road network and returns the route it most
likely followed, for driving (with or without live traffic), walking, or
cycling. Curated by hand down to the parameters documented at
docs.mapbox.com/api/navigation/map-matching — see `custom-openapi/README.md`
for why this command group doesn't come from the vendored specs the way
most others do. Excludes POST, which this CLI's spec format has no way to
express alongside GET for the same operation — the API's own POST is for a
trace too long for a URL (~8100 bytes), a real gap rather than a design
choice.

### `mapbox map-matching`

One or more matched routes — more than one where the trace is ambiguous
enough to split — each carrying a `confidence` the API assigns itself, plus
one tracepoint per input coordinate (`null` for one too far from any
candidate to match at all). No subcommand: this API has one operation, so
there's nothing a second word would disambiguate, the same reason `mapbox
directions` has none either.

#### Parameters

`<routing-profile>` and `<coordinates>` (both positional) are required.
`<routing-profile>` is sent exactly as typed, not checked against a fixed
list: `mapbox/driving-traffic`, `mapbox/driving`, `mapbox/walking` and
`mapbox/cycling` are documented, but some accounts (OEM agreements, mainly)
have additional profiles of their own that were never published, the API
is the authority on whether a value is valid, not this page. `<coordinates>`
is 2-100 `{longitude},{latitude}` trace points, semicolon-separated, or an
OpenLR-encoded string of up to 50 points (pair with `--openlr-spec`/
`--openlr-format`).

| Parameter | Effect |
| --- | --- |
| `--annotations <fields>` | Segment-level metadata per leg, comma-separated (`distance`, `duration`, `speed`, `congestion`, `congestion_numeric`, `maxspeed`). Requires `--overview full`. |
| `--approaches <unrestricted\|curb;...>` | Which side of the road to approach each waypoint from. Requires `--steps`. |
| `--geometries <geojson\|polyline\|polyline6>` | Route geometry format. Defaults to `polyline`. |
| `--overview <full\|simplified\|false>` | Geometry detail level. Defaults to `simplified`. |
| `--radiuses <meters;...>` | Max snap distance, 0-50, one per coordinate. Defaults to 5. |
| `--steps` | Return turn-by-turn instructions. Several flags below only take effect with this set. |
| `--banner-instructions` | Return banner objects for display. Requires `--steps`. |
| `--language <tag>` | Instruction language. Defaults to `en`. Requires `--steps`. |
| `--roundabout-exits` | Separate entry/exit instructions for a roundabout. Requires `--steps`. |
| `--voice-instructions` | Return SSML-marked voice guidance. Requires `--steps`. |
| `--voice-units <imperial\|british_imperial\|metric>` | Requires `--steps` and `--voice-instructions`. |
| `--tidy` | Remove clusters and resample the trace before matching — for a trace recorded at an inconsistent sample rate. |
| `--timestamps <unix;...>` | When the trace was recorded, per coordinate, ascending — rather than assumed from even spacing. |
| `--waypoint-names <names;...>` | A name per waypoint for its arrival instruction. Requires `--steps`. |
| `--waypoints <indices>` | Which coordinates get their own arrival instruction — must include `0` and the last index. Requires `--steps`. |
| `--ignore <types>` | Restrictions to ignore, comma-separated (`access`, `oneways`, `restrictions`). `mapbox/driving` only. |
| `--linear-references` | Return an OpenLR reference (base64) per matched leg, alongside the ordinary geometry. |
| `--openlr-spec <tomtom\|here>` | Which OpenLR spec `coordinates` is encoded with, if it's an OpenLR string. Defaults to `tomtom`. |
| `--openlr-format tomtom` | The OpenLR binary format `coordinates` is encoded in, if it's an OpenLR string. |
| `--depart-at <ISO 8601>` | For `mapbox/driving-traffic`, which live traffic conditions to route against. |

#### Examples

```sh
mapbox map-matching mapbox/driving "-122.42,37.78;-122.421,37.781;-122.422,37.782"
mapbox map-matching mapbox/driving "-122.42,37.78;-122.421,37.781;-122.422,37.782" \
  --steps --geometries geojson
```

#### Outputs

Captured live: three trace points in San Francisco, one deliberately far
enough off the road network to leave its tracepoint `null`.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```json
{
  "code": "Ok",
  "matchings": [
    {
      "confidence": 0,
      "distance": 353.157,
      "duration": 92.702,
      "geometry": "q|qeFndejVf@lJyDd@Y_E",
      "legs": [
        {
          "distance": 353.157,
          "duration": 92.702,
          "steps": [],
          "summary": "McAllister Street, Franklin Street",
          "weight": 126.614
        }
      ],
      "weight": 126.614,
      "weight_name": "auto"
    }
  ],
  "tracepoints": [
    { "name": "McAllister Street", "location": [-122.420084, 37.780093], "waypoint_index": 0 },
    { "name": "Golden Gate Avenue", "location": [-122.421141, 37.780946], "waypoint_index": 1 },
    null
  ]
}
```

</td><td>

```json
{"code":"Ok","matchings":[{"confidence":0,"distance":353.157,"duration":92.702,"geometry":"q|qeFndejVf@lJyDd@Y_E","legs":[{"distance":353.157,"duration":92.702,"steps":[],"summary":"McAllister Street, Franklin Street","weight":126.614}],"weight":126.614,"weight_name":"auto"}],"tracepoints":[{"name":"McAllister Street","location":[-122.420084,37.780093],"waypoint_index":0},{"name":"Golden Gate Avenue","location":[-122.421141,37.780946],"waypoint_index":1},null]}
```

</td></tr>
</table>

Like `mapbox directions`, neither output mode has a bespoke rendering for
this response — it isn't GeoJSON at the top level — so both print the same
JSON, `-o text` pretty-printed and `-o json` on one line. Trimmed to one
matching and dropped `admins`/`via_waypoints`/`alternatives_count`/`uuid`
for length; the real response carries them too.

---
## Matrix

Travel time and distance between every pair in a set of up to 25
coordinates, in one call, for driving (with or without live traffic),
walking, or cycling. Curated by hand down to the parameters documented at
docs.mapbox.com/api/navigation/matrix — see `custom-openapi/README.md` for
why this command group doesn't come from the vendored specs the way most
others do.

**vs. `mapbox directions`**: this answers "how far/long between every pair",
not a route through all of them in order — `mapbox directions` is a route
through fixed stops; this is an N×N table, useful for ranking or filtering
many candidates by reachability before committing to a route through any of
them.

### `mapbox matrix`

A `durations` and/or `distances` matrix in row-major order —
`durations[i][j]` is the time from the ith source to the jth destination —
across every source/destination pair, or a subset of either side. No
subcommand: this API has one operation, so there's nothing a second word
would disambiguate, the same reason `mapbox directions` has none either.

#### Parameters

`<routing-profile>` and `<coordinates>` (both positional) are required.
`<routing-profile>` is sent exactly as typed, not checked against a fixed
list: `mapbox/driving-traffic`, `mapbox/driving`, `mapbox/walking` and
`mapbox/cycling` are documented, but some accounts (OEM agreements, mainly)
have additional profiles of their own that were never published, the API
is the authority on whether a value is valid, not this page.
`<coordinates>` is 2-25 `{longitude},{latitude}` pairs, semicolon-separated,
10 max for `mapbox/driving-traffic`.

| Parameter | Effect |
| --- | --- |
| `--annotations <duration\|distance>` | Which matrix or matrices to return, comma-separated. `duration` alone is the default; both together returns both. |
| `--approaches <unrestricted\|curb;...>` | Which side of the road to approach each coordinate from. |
| `--bearings <angle,degrees;...>` | Filter road segments by direction of travel, one entry per coordinate. |
| `--sources <indices>` | Which coordinates are matrix rows — `all` (the default) or zero-based indices, **semicolon**-separated. Verified against production: comma-separated is a 422 here, unlike most other index lists on these commands. |
| `--destinations <indices>` | Which coordinates are matrix columns — same rules as `--sources`. |
| `--fallback-speed <km/h>` | Replaces a `null` (unreachable) cell with a straight-line estimate at this speed, rather than leaving it `null`. Legacy. |
| `--depart-at <ISO 8601>` | For future traffic conditions and time-dependent road restrictions. |

#### Examples

```sh
mapbox matrix mapbox/driving "-122.42,37.78;-122.45,37.91;-122.41,37.80"
mapbox matrix mapbox/driving "-122.42,37.78;-122.45,37.91;-122.41,37.80" \
  --sources 0 --destinations "1;2"
```

#### Outputs

Captured live: a full 3×3 matrix between three San Francisco points, both
`durations` (seconds) and `distances` (meters).

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```json
{
  "code": "Ok",
  "durations": [
    [0, 2381.5, 790.1],
    [2593.8, 0, 2272.5],
    [994.8, 2269.7, 0]
  ],
  "distances": [
    [0, 25766, 3348.3],
    [26960.4, 0, 25382],
    [3781.3, 25174.1, 0]
  ],
  "sources": [
    { "name": "Van Ness Avenue", "location": [-122.420122, 37.779978] },
    { "name": "Playa Verde", "location": [-122.461997, 37.89621] },
    { "name": "Columbus Avenue", "location": [-122.409926, 37.800067] }
  ],
  "destinations": [
    { "name": "Van Ness Avenue", "location": [-122.420122, 37.779978] },
    { "name": "Playa Verde", "location": [-122.461997, 37.89621] },
    { "name": "Columbus Avenue", "location": [-122.409926, 37.800067] }
  ]
}
```

</td><td>

```json
{"code":"Ok","durations":[[0,2381.5,790.1],[2593.8,0,2272.5],[994.8,2269.7,0]],"distances":[[0,25766,3348.3],[26960.4,0,25382],[3781.3,25174.1,0]],"sources":[{"name":"Van Ness Avenue","location":[-122.420122,37.779978]},{"name":"Playa Verde","location":[-122.461997,37.89621]},{"name":"Columbus Avenue","location":[-122.409926,37.800067]}],"destinations":[{"name":"Van Ness Avenue","location":[-122.420122,37.779978]},{"name":"Playa Verde","location":[-122.461997,37.89621]},{"name":"Columbus Avenue","location":[-122.409926,37.800067]}]}
```

</td></tr>
</table>

Neither output mode has a bespoke rendering for this response, same as
`mapbox directions` and `mapbox map-matching` — both print the same JSON,
`-o text` pretty-printed and `-o json` on one line. Dropped each waypoint's
own snap `distance` for length; the real response carries it too.

---

## Search

The public, non-interactive surface of the Search Box API: text search,
reverse lookup, category search, and the category list. Curated by hand
down to the parameters documented at docs.mapbox.com/api/search/search-box
— see `custom-openapi/README.md` for why this command group doesn't come from
the vendored specs the way the others do. `suggest` and
`retrieve/{id}` are deliberately not here: both need a caller-managed
`session_token` to group a client-side autocomplete flow — a UX built
around a person typing into a search box, not a one-shot CLI invocation —
so there is no non-interactive way to use them.

**vs. `geocoder`**: `geocoder reverse` and `search reverse` take
nearly identical coordinates and answer different questions. `geocoder`
returns canonical addresses and administrative hierarchy (country, region,
postcode, place); `search` returns POIs and businesses with the metadata a
geocoder has no field for — rating, price level, hours of operation, brand.
Reaching for the wrong one for a query the other answers is the most likely
mistake here: "what's the address at this point" is `geocoder`, "what's
near this point" is `search`.

### `mapbox search forward`

Text search for an address or POI — the one-off equivalent of typing into a
search box and taking the first screen of results, with no autocomplete
session behind it.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--q <text>` | The search string. Required. |
| `--limit <n>` | How many results, up to 10. |
| `--proximity <lon,lat>` \| `ip` | Bias results towards a point, or the caller's IP location. |
| `--near <text>` | Bias results towards a place described in free text, e.g. `"paris france"`. |
| `--bbox <minlon,minlat,maxlon,maxlat>` | Restrict to a box. |
| `--radius <degrees>` | Restrict to a radius around `--proximity`. |
| `--country <codes>` | ISO 3166-1 alpha-2, comma-separated. |
| `--types <types>` | `poi`, `address`, `place`, … |
| `--poi-category <cats>` / `--poi-category-exclusions <cats>` | Include or exclude POI categories. |
| `--show-closed-pois` / `--open-now` | Include closed POIs, or only currently-open ones. |
| `--minimum-rating <0.0-5.0>` / `--price-levels <$..$$$$>` | Filter POIs by rating or price. |
| `--exclude-fields <fields>` | Omit metadata fields from the response, e.g. `photos,reviews`. |
| `--rank-strategy <distance\|relevance>` | Change how results are ordered. |
| `--language <tag>` | ISO language code. |
| `--auto-complete` | Include partial and fuzzy matches, for autocomplete-style input. |
| `--sar-type isochrone` + `--route <polyline>` + `--route-geometry <polyline\|polyline6>` | Search-along-route: results near a route rather than a point. |
| `--time-deviation <minutes>` | With SAR, maximum detour allowed from the route. |
| `--eta-type navigation` + `--navigation-profile <driving\|walking\|cycling>` + `--origin <lon,lat>` | Include an ETA in each result, from `--origin` (or `--proximity`) to it. |

#### Examples

```sh
mapbox search forward --q "34170 Gannon Terrace" --limit 1
mapbox search forward --q coffee --proximity -121.90662,37.42827 --poi-category coffee
```

#### Outputs

Not captured live: the credentials used to write this doc have no Search
Box API access (401). `properties` below is
[the documented example](https://docs.mapbox.com/api/search/search-box/#text-search);
the text column is the actual list this response renders as (see "How a
response is rendered" — `search` is one of the three command groups whose GeoJSON
becomes a list rather than staying pretty-printed):

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
1. 34170 Gannon Terrace (address) — 20.0 km
   34170 Gannon Terrace, Fremont, California 94555, United States
   -122.059627,37.56153
```

</td><td>

```json
{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-122.059627,37.56153],"type":"Point"},"properties":{"name":"34170 Gannon Terrace","mapbox_id":"{mapbox_id}","feature_type":"address","full_address":"34170 Gannon Terrace, Fremont, California 94555, United States","distance":20045}}]}
```

</td></tr>
</table>

### `mapbox search reverse`

The POIs and addresses at a coordinate — `search`'s counterpart to
`geocoder reverse`, answering with business metadata instead of
administrative hierarchy.

#### Parameters

`--longitude` and `--latitude` are required.

| Parameter | Effect |
| --- | --- |
| `--limit <n>` | How many results, up to 10. |
| `--country <codes>` | ISO 3166-1 alpha-2, comma-separated. |
| `--types <types>` | `poi`, `address`, `place`, … |
| `--show-closed-pois` | Include permanently closed POIs. |
| `--language <tag>` | ISO language code. |

#### Examples

```sh
mapbox search reverse --longitude -118.471383 --latitude 34.023653 --limit 1
```

#### Outputs

Not captured live, for the same reason as `forward` above. Listed the same
way; `properties` is per
[the docs](https://docs.mapbox.com/api/search/search-box/#reverse-lookup).
No `distance` here — the docs' own example doesn't return one for `reverse`:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
1. 1827 21st Street (address)
   1827 21st Street, Santa Monica, California 90404, United States
   -118.471584,34.023345
```

</td><td>

```json
{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-118.471584,34.023345],"type":"Point"},"properties":{"name":"1827 21st Street","feature_type":"address","full_address":"1827 21st Street, Santa Monica, California 90404, United States"}}]}
```

</td></tr>
</table>

### `mapbox search category`

POIs in a canonical category, near a location or along a route — quick
"buttons" like a coffee search, rather than a text query.

#### Parameters

`<category>` (positional) is the canonical category ID, e.g. `coffee` —
see `list-category` below for the full list. One of `--proximity`,
`--near`, `--bbox` or `--route` is required by the API, though nothing
here enforces it before the request goes out.

| Parameter | Effect |
| --- | --- |
| `--proximity <lon,lat>` \| `ip` | Search near a point, or the caller's IP location. |
| `--near <text>` | Search near a place described in free text. |
| `--bbox <minlon,minlat,maxlon,maxlat>` | Restrict to a box. |
| `--radius <degrees>` | Restrict to a radius around `--proximity`. |
| `--limit <n>` | How many results, up to 25. |
| `--country <codes>` | ISO 3166-1 alpha-2, comma-separated. |
| `--types <types>` | `poi`, `address`, `place`, … |
| `--poi-category-exclusions <cats>` | Exclude POI categories. |
| `--show-closed-pois` | Include permanently closed POIs. |
| `--exclude-fields <fields>` | Omit metadata fields from the response, e.g. `photos,reviews`. |
| `--language <tag>` | ISO language code. |
| `--sar-type isochrone` + `--route <polyline>` + `--route-geometry <polyline\|polyline6>` | Search-along-route: results near a route rather than a point. |
| `--time-deviation <minutes>` | With SAR, maximum detour allowed from the route. |
| `--eta-type navigation` + `--navigation-profile <driving\|walking\|cycling>` + `--origin <lon,lat>` | Include an ETA in each result, from `--origin` (or `--proximity`) to it. |

#### Examples

```sh
mapbox search category coffee --proximity -121.90662,37.42827 --limit 1

# --route takes an encoded polyline — the same format the Directions API's
# `routes[].geometry` returns (geometries=polyline by default; pass
# --route-geometry polyline6 if you fetched one with geometries=polyline6):
mapbox search category gas_station \
  --route '_fmcFn{`gV`AdGCT~AbJBf@C`@i@dBK~@iBn@{@c@' \
  --sar-type isochrone --limit 2
```

#### Outputs

Not captured live, for the same reason as `forward` above. Listed the same
way; `properties` is per
[the docs](https://docs.mapbox.com/api/search/search-box/#retrieve-pois-by-category).
`poi_category` is what shows in place of `feature_type`, joined —
`category` search results almost always have one; `brand` is not shown at
all (nothing here is a good enough summary of it to put on one line), and
`-o json` is where it is:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
1. Starbucks (café, coffee, coffee shop) — 19.6 km
   15885 Dam Road, Clearlake, California 95422, United States
   -122.6180785,38.9307594
```

</td><td>

```json
{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-122.6180785,38.9307594],"type":"Point"},"properties":{"name":"Starbucks","feature_type":"poi","brand":["Starbucks"],"poi_category":["café","coffee","coffee shop"],"full_address":"15885 Dam Road, Clearlake, California 95422, United States","distance":19568}}]}
```

</td></tr>
</table>

### `mapbox search list-category`

The canonical category IDs usable with `search category`, with a display
name in the requested language. Does not describe parent/child
relationships between categories.

#### Parameters

None beyond `--language`.

#### Examples

```sh
mapbox search list-category
mapbox search list-category --language fr
```

#### Outputs

Not captured live, for the same reason as `forward` above. Renders as a
table, not the list `forward`/`reverse`/`category` get — every category is
three short strings, none of them long enough to need a line of its own —
built from `canonical_id`, `name` and `icon`; `uuid` and `version` are left
out, both generated fresh each request and no use as identifiers. Shape
per [the docs](https://docs.mapbox.com/api/search/search-box/#list-categories):

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
NAME            CANONICAL_ID    ICON
Food and Drink  food_and_drink  fast-food
Lodging         lodging         lodging
```

</td><td>

```json
{"listItems":[{"canonical_id":"food_and_drink","icon":"fast-food","name":"Food and Drink","uuid":"71fed985-…","version":"25:6bd9…"},{"canonical_id":"lodging","icon":"lodging","name":"Lodging","uuid":"8de7b125-…","version":"25:6bd9…"}],"attribution":"…"}
```

</td></tr>
</table>

---

## Sprites

The sprite sheet a style draws its icons from, and the individual icons in
it. Five operations.

These are Styles API endpoints — every URL here sits under
`styles/v1/{username}/{style_id}/sprite`, and a `<style-id>` positional is
the first argument of all five. They are a command group of their own
rather than five more `styles` commands because a sprite is a different
thing from a style, and because `mapbox styles --help` read as two
command groups stacked on top of each other while it held both.

### `mapbox sprites get-json`

The sprite index: where each icon sits in the sheet, and how big it is.

#### Examples

```sh
mapbox sprites get-json ckstyle00000000000000001a --username user
mapbox sprites get-json ckstyle00000000000000001a --username user -o json > sprite.json
```

#### Outputs

⚠️ **This read is cached and cannot confirm a write.** It is a GET behind
CloudFront with `max-age=900`, so for up to fifteen minutes after an upload
or delete it keeps answering with the sprite as it was. The mutation
commands all return the index themselves, from the origin; use that.

The response is one object keyed by icon name, not an array, so it becomes a
field list — one line per icon *property*, with the icon name as the prefix.
For a style with a few hundred icons that is several thousand lines, and the
key column is padded to the longest icon name. `-o json` is the usable form.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
ae-d-route-3.height       24
ae-d-route-3.pixelRatio   1
ae-d-route-3.placeholder  0, 9, 24, 15
ae-d-route-3.visible      yes
ae-d-route-3.width        24
ae-d-route-3.x            78
ae-d-route-3.y            124
ae-d-route-4.height       24
…
```

</td><td>

```json
{"ae-d-route-3":{"height":24,"pixelRatio":1,"placeholder":[0,9,24,15],"visible":true,"width":24,"x":78,"y":124}}
```

</td></tr>
</table>

### `mapbox sprites upload`

Adds one SVG to a style's sprite, under the icon name given.

#### Parameters

`--file <PATH>` is the SVG. The body is a raw `image/svg+xml`, so the file's
bytes go out untouched — `--data` does not apply here.

Limits: 512 px per side, under 400 KB, 1,000 images per sprite, icon names
up to 255 characters.

#### Examples

```sh
mapbox sprites upload ckstyle00000000000000001a zz-clitest-1 \
  --username user --file icon.svg
```

#### Outputs

**Every one of the four sprite commands answers with the whole sprite
index**, not with the icon you touched — 440 entries for the style above,
one line of 45 KB under `json` and some 2,900 lines under `text`. Rows for
the new icon, from a real upload:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
…
zz-clitest-1.height      16
zz-clitest-1.pixelRatio  1
zz-clitest-1.visible     yes
zz-clitest-1.width       16
zz-clitest-1.x           480
zz-clitest-1.y           409
```

</td><td>

```json
{…,"zz-clitest-1":{"height":16,"pixelRatio":1,"visible":true,"width":16,"x":480,"y":409}}
```

</td></tr>
</table>

`x` and `y` are where the icon landed in the sheet, which is the point of
getting the whole index back: the sprite has been repacked, so every other
icon's coordinates may have moved too.

**The index a mutation returns is the only fresh view of the sprite.**
`sprites get-json` is a GET behind CloudFront with `max-age=900`, so for up to
fifteen minutes after an upload it keeps answering with the sprite as it was
— same entry count, none of the new icons. Verified: a `sprites upload-batch`
answered with 444 entries including all four new icons while
`sprites get-json` still said 440 with none, and the response carried
`x-cache: Hit from cloudfront`. Read the mutation's own response; do not
confirm a write by reading it back.

The `-o json` hint under the field list goes to **stderr**, so
`... -o text > sprite.txt` still gets a clean file.

Re-uploading an existing name **overwrites it and answers 200** — there is
no "already exists" error, and nothing warns you.

A file that is not an SVG is refused by the API, not locally:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Error: Image is invalid. Must be a valid SVG.
(HTTP 422)
```

</td><td>

```json
{"code":"http_422","message":"Image is invalid. Must be a valid SVG.","status":422}
```

</td></tr>
</table>

A path that does not exist is caught before any request goes out:

```json
{"code":"invalid_file","message":"Cannot read `/tmp/nope.svg` given to --file: No such file or directory (os error 2)"}
```

### `mapbox sprites upload-batch`

Adds up to 25 SVGs in one request, 100 KB each.

#### Parameters

`--file <PATH>`, repeated once per image. Each becomes a part in a
`multipart/form-data` body under the field the spec names, `images`.

**The icon name comes from the filename**, not from an argument:
`zz-clitest-3.svg` becomes the icon `zz-clitest-3`. There is no way to
upload a batch under names that differ from the files.

#### Examples

```sh
mapbox sprites upload-batch ckstyle00000000000000001a --username user \
  --file zz-clitest-3.svg --file zz-clitest-4.svg
```

#### Outputs

The whole sprite index again, with the uploaded icons in it. From a real run
of the two files above:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
…
zz-clitest-3.height      16
zz-clitest-3.pixelRatio  1
zz-clitest-3.visible     yes
zz-clitest-3.width       16
zz-clitest-3.x           0
zz-clitest-3.y           425
zz-clitest-4.x           16
zz-clitest-4.y           425
```

</td><td>

```json
{…,"zz-clitest-3":{"height":16,"pixelRatio":1,"visible":true,"width":16,"x":0,"y":425},"zz-clitest-4":{…,"x":16,"y":425}}
```

</td></tr>
</table>

A batch lands on a fresh row — `y: 425`, where the single upload above went
to `y: 409` — because the sheet was repacked to fit them.

### `mapbox sprites delete`

Removes one icon from the sprite.

#### Examples

```sh
mapbox sprites delete ckstyle00000000000000001a zz-clitest-1 --username user
```

#### Outputs

The sprite index **after** the delete — the named icon is gone from it, and
everything else is still there. A 200 with the full layout, not a 204: the
absence of the icon is the only acknowledgment there is.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
…
zz-clitest-2.height      16
zz-clitest-2.x           480
…
```

</td><td>

```json
{…,"zz-clitest-2":{…},"zz-clitest-3":{…},"zz-clitest-4":{…}}
```

</td></tr>
</table>

`zz-clitest-1` is absent; `-2` onwards remain. Note that `-2` has moved to
`x: 480`, the slot `-1` occupied — a delete repacks the sheet too.

An icon name that is not in the sprite is a 404:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Error: Sprite not found (HTTP 404)
```

</td><td>

```json
{"code":"http_404","message":"Sprite not found","status":404}
```

</td></tr>
</table>

### `mapbox sprites delete-batch`

Removes up to 150 icons in one request.

#### Parameters

`--data`/`-d` is a JSON **array of icon names** — not an object.

#### Examples

```sh
mapbox sprites delete-batch ckstyle00000000000000001a --username user \
  -d '["zz-clitest-5","zz-clitest-6"]'
```

#### Outputs

The sprite index after the deletions, as with the single delete. Captured by
uploading two icons and removing them again: the response went from 442
entries with both present to 440 with neither.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
ae-d-route-3.height      24
ae-d-route-3.pixelRatio  1
…
(2,877 rows, no zz-clitest-* among them)
```

</td><td>

```json
{"ae-d-route-3":{…},…}   // 44,859 bytes, 440 entries, no zz-clitest-*
```

</td></tr>
</table>

Every name must exist. One that does not takes the whole request down with a
404 that names it — the deletions are not partial-applied:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Error: Image "zz-clitest-nonexistent" not found
(HTTP 404)
```

</td><td>

```json
{"code":"http_404","message":"Image \"zz-clitest-nonexistent\" not found","status":404}
```

</td></tr>
</table>

An empty array is refused, so there is no no-op form:

```json
{"code":"http_422","message":"Remove at least 1 image","status":422}
```

And an object where an array belongs:

```json
{"code":"http_400","message":"Body must be an array of image names.","status":400}
```

---

## Static

A rendered map image or raster tile from a style. Both return image bytes,
so `--output` does not apply — redirect to a file. Static Images and Static
Tiles merged into this one command group (#116); `get-image` and `get-tile`
are what were `static-images get-static-image` and `static-tiles
get-static-tile`.

`<format>` takes a **leading dot** — `.png`, `.jpeg`, `.webp`, or `""` for
the style default. `<highRes>` is `@2x` or `""`. `<overlay>` (`get-image`
only) is a marker/path/GeoJSON expression, or `""` for none.

`""` for the overlay drops the segment entirely rather than sending an empty
one, so a plain map image needs no overlay expression.

### `mapbox static get-image`

A map image centered on a point.

#### Parameters

Eleven positionals, in order: `<style-id> <overlay> <lon> <lat> <zoom>
<bearing> <pitch> <width> <height> <highRes> <format>`.

`<bearing>` and `<pitch>` default to `0` at the API, but the CLI requires
both as integers — pass `0 0` rather than `"" ""`.

| Parameter | Effect |
| --- | --- |
| `--attribution` / `--logo` | Keep or drop the Mapbox attribution and logo. |
| `--addlayer <json>` | Add one layer on top of the style. |
| `--before-layer <id>` | Where to insert it. |
| `--setfilter <json>` / `--layer-id <id>` | Filter an existing layer. |

#### Examples

```sh
mapbox static get-image streets-v12 "" 24.94 60.16 12 0 0 600 400 "" .png \
  --username mapbox > map.png

mapbox static get-image streets-v12 "pin-s+555555(24.94,60.16)" \
  24.94 60.16 12 0 0 600 400 "@2x" .png --username mapbox > pin.png
```

#### Outputs

<table>
<tr><th width="50%">Terminal — refuses</th><th width="50%">Redirected — raw bytes</th></tr>
<tr><td>

```
Error: Response is image/png (54621 bytes).
Refusing to write it to the terminal — redirect
it to a file, e.g. `... > out.png`.
```

</td><td>

```
$ mapbox static get-image … > map.png
$ file map.png
map.png: PNG image data, 600 x 400
```

</td></tr>
</table>

### `mapbox static get-tile`

One raster tile rendered from a style, rather than from a tileset.

#### Parameters

Seven positionals: `<style-id> <tilesize> <z> <x> <y> <highRes> <format>`.

`<tilesize>` is `512` (the default) or `256`. 512 px tiles at zoom *z* cover
the same ground as 256 px tiles at *z+1*, so 256 needs four times as many
requests for the same area.

`<highRes>` is `@2x` or `""`; `<format>` takes a leading dot, as in
`get-image`.

#### Examples

```sh
mapbox static get-tile streets-v12 512 2 1 1 "" .png \
  --username mapbox > tile.png
mapbox static get-tile streets-v12 256 12 2048 1361 "@2x" .png \
  --username mapbox > tile@2x.png
```

#### Outputs

<table>
<tr><th width="50%">Terminal — refuses</th><th width="50%">Redirected — raw bytes</th></tr>
<tr><td>

```
Error: Response is image/png (85567 bytes).
Refusing to write it to the terminal — redirect
it to a file, e.g. `... > out.png`.
```

</td><td>

```
$ mapbox static get-tile … > tile.png
$ file tile.png
tile.png: PNG image data, 512 x 512
```

</td></tr>
</table>

---
## Styles

Styles and their drafts. Eight operations.

The three draft commands sit under a `draft` group of their own —
`mapbox styles draft get`, not `mapbox styles get-style-draft` — because
the draft is a second version of the same style rather than a second kind
of thing, and the three read as a set. `mapbox styles draft --help` lists
them.

The sprite endpoints share these URLs and are [a command group of their
own](#sprites).

### `mapbox styles list`

Metadata for every style in an account.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--limit <n>` | How many to return. |
| `--start <style-id>` | Continue after this style — the paging cursor. |
| `--draft` | Return draft versions instead of published. |
| `--deleted` | Return recently deleted styles instead of active ones. |

Paginated the same way as tokens: the `Link` header carries the next
`start`.

#### Examples

```sh
mapbox styles list --username user
mapbox styles list --username user --limit 5
mapbox styles list --username user --id ckstyle00000000000000001a
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
ID                         NAME           CREATED     VISIBILITY
ckstyle00000000000000001a  Parks & Rail…  2025-09-04  private
ckstyle00000000000000002b  Streets        2023-05-23  private

Tips:
  Values are shortened to fit; `-o json` prints each row whole.
  To see one row: add --id ckstyle00000000000000001a
```

</td><td>

```json
[{"created":"2025-09-04T12:06:35.359Z","id":"ckstyle00000000000000001a","modified":"2025-09-04T12:08:57.099Z","name":"Parks & Railways Highlight","owner":"user","protected":false,"visibility":"private"}]
```

</td></tr>
</table>

The id copies straight into the next command, which is the reason to look at
a listing at all:

```sh
mapbox styles get ckstyle00000000000000001a --username user
```

### `mapbox styles get`

One style document, conforming to the Mapbox Style Specification.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--download` | Ask for it as a file attachment. |
| `--optimize` | Rewrite vector source URLs for cache optimization. |

#### Examples

```sh
mapbox styles get ckstyle00000000000000001a --username user
mapbox styles get ckstyle00000000000000001a --username user -o json > style.json
```

#### Outputs

⚠️ **This read is cached and can be up to fifteen minutes stale.** Every
`styles/v1` read — this, `styles draft get`, `styles list`, `get-sprite-json`
— is served through CloudFront with `max-age=900`, so a `styles get` right
after an `styles update` may still show the old document. The mutation
commands return the new state themselves, from the origin; trust that rather
than reading back.

A style document is a nested tree of layers and sources, not rows, so it
stays JSON in both modes — indented under `text`, one line under `json`.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
{
  "created": "2025-09-04T12:06:35.359Z",
  "id": "ckstyle00000000000000001a",
  "layers": [
    {
      "id": "background",
      "paint": {
        "background-color": "#f8f9fa"
      },
      "type": "background"
    },
    …
```

</td><td>

```json
{"created":"2025-09-04T12:06:35.359Z","id":"ckstyle00000000000000001a","layers":[{"id":"background","paint":{"background-color":"#f8f9fa"},"type":"background"}],"version":8}
```

</td></tr>
</table>

### `mapbox styles create`

Adds a new style. The server fills in `created`, `id`, `modified`, `owner`
and the sprite URL; `name` defaults to the style id if omitted.

#### Parameters

`--data`/`-d` carries the style document. It must be valid against the
Mapbox Style Specification — anything else is a 422. The minimum the API
accepts is a name, a version, and empty `sources` and `layers`.

#### Examples

```sh
mapbox styles create --username user \
  -d '{"name":"My Style","version":8,"sources":{},"layers":[]}'

mapbox styles create --username user -d "$(cat style.json)"
```

#### Outputs

The created style, as a single object, so a field list. Empty `layers`
renders as `(none)`.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
created     2026-09-01T19:59:12.754Z
draft       no
glyphs      mapbox://fonts/mapbox/{fontstack}/{range}.pbf
id          ckstyle00000000000000004d
layers      (none)
modified    2026-09-01T19:59:12.754Z
name        My Style
owner       user
protected   no
sprite      mapbox://sprites/user/ckstyle00000000000000004d/…
version     8
visibility  private
```

</td><td>

```json
{"created":"2026-09-01T19:59:13.073Z","draft":false,"glyphs":"mapbox://fonts/mapbox/{fontstack}/{range}.pbf","id":"ckstyle00000000000000004d","layers":[],"modified":"2026-09-01T19:59:13.073Z","name":"My Style","owner":"user","protected":false,"sources":{},"sprite":"mapbox://sprites/user/ckstyle00000000000000004d/…","version":8,"visibility":"private"}
```

</td></tr>
</table>

The `id` is what every other styles command wants. A body the API cannot
read is a 400:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Error: Invalid JSON received (HTTP 400)
```

</td><td>

```json
{"code":"http_400","message":"Invalid JSON received","status":400}
```

</td></tr>
</table>

### `mapbox styles update`

Modifies an existing style. `name` is required in the body. Strip `created`
and `modified` before sending — including them is a 422. Cross-version
updates (v7 to v8) are rejected.

#### Examples

```sh
mapbox styles update ckstyle00000000000000004d --username user \
  -d '{"name":"Renamed","version":8,"sources":{},"layers":[]}'
```

#### Outputs

The updated style, in the same shape `styles create` returns. `modified`
moves; `created` does not.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
created     2026-09-01T19:59:12.754Z
draft       no
id          ckstyle00000000000000004d
layers      (none)
modified    2026-09-01T19:59:28.467Z
name        Renamed
…
```

</td><td>

```json
{"created":"2026-09-01T19:59:13.073Z","draft":false,"id":"ckstyle00000000000000004d","layers":[],"modified":"2026-09-01T19:59:28.868Z","name":"Renamed",…}
```

</td></tr>
</table>

Note that the body replaces the style: the `layers` and `sources` you send
are the ones it will have, so read it with `styles get` first unless you mean
to empty it.

### `mapbox styles delete`

Removes a style and its sprites. Mapbox keeps a deleted style recoverable
for 30 days, but this CLI does not ship the operation that restores one —
see the `Breaking` entry in the changelog.

#### Examples

```sh
mapbox styles delete ckstyle00000000000000003c --username user
```

#### Outputs

A 204 carries no body. Rather than print nothing, the CLI confirms what
happened — nothing is asked beforehand, so the line after the fact is the
only acknowledgment there is.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Deleted ckstyle00000000000000003c.
```

</td><td>

```json
{"command":"styles delete","ok":true,"status":204}
```

</td></tr>
</table>

The style leaves the account immediately, and stays reachable for 30 days
through `--deleted`:

```sh
$ mapbox styles get ckstyle00000000000000003c --username user
Error: Style not found (HTTP 404)

$ mapbox styles list --username user --deleted
ID                         NAME      CREATED   DELETED   MODIFIED  OWNER     PROTECTED  VERSION  VISIBILITY
ckstyle00000000000000003c  Old ske…  2018-03…  2026-09…  2018-03…  user      no         8        public
```

Two things that are easy to get wrong:

- **Deleting an already-deleted style succeeds again.** It answers 204 and
  says `Deleted …` a second time. Only an id that never existed is a 404.
- The 404 below therefore means "no such style, ever" — not "already gone".

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Error: Style not found (HTTP 404)
```

</td><td>

```json
{"code":"http_404","message":"Style not found","status":404}
```

</td></tr>
</table>

Unlike `styles draft delete`, which cannot tell a real id from a typo, this
one does.

### `mapbox styles draft get`

The draft version of a style. Every style carries a published version and a
draft; Mapbox Studio always edits the draft. With no draft, the published
style comes back instead — which is also how you can tell a draft was
discarded.

#### Examples

```sh
mapbox styles draft get ckstyle00000000000000004d --username user
```

#### Outputs

The same shape as `styles get`, with `"draft": true` while a draft exists.

⚠️ **This read is cached.** See the note under
[`styles get`](#mapbox-styles-get).

### `mapbox styles draft update`

Updates the draft. Creates one from the published version if none exists.
The published style is untouched until it is published in Studio.

#### Examples

```sh
mapbox styles draft update ckstyle00000000000000004d --username user \
  -d '{"name":"Work in progress","version":8,"sources":{},"layers":[]}'
```

#### Outputs

The updated draft. Two fields separate it from `styles update`'s answer:
`draft` is `yes`/`true`, and the sprite URL ends in `/draft`.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
created     2026-09-01T19:59:12.754Z
draft       yes
id          ckstyle00000000000000004d
layers      (none)
modified    2026-09-01T19:59:29.072Z
name        Work in progress
protected   no
sprite      mapbox://sprites/user/ckstyle00000000000000004d/draft
version     8
visibility  private
```

</td><td>

```json
{"created":"2026-09-01T19:59:13.073Z","draft":true,"id":"ckstyle00000000000000004d","modified":"2026-09-01T19:59:29.344Z","name":"Work in progress","sprite":"mapbox://sprites/user/ckstyle00000000000000004d/draft",…}
```

</td></tr>
</table>

### `mapbox styles draft delete`

Discards the draft, reverting the style to its published version. The
published style is untouched.

#### Examples

```sh
mapbox styles draft delete ckstyle00000000000000004d --username user
```

#### Outputs

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Deleted ckstyle00000000000000004d.
```

</td><td>

```json
{"command":"styles draft delete","ok":true,"status":204}
```

</td></tr>
</table>

It does what it says: after the delete, `styles draft get` returns the
published style with `"draft": false`. Confirmed on a style whose draft URL
had never been fetched, so no cached copy could stand in for the answer.

⚠️ It also answers 204 for a style id that does not exist — verified against
`zzznosuchstylexyz000000000`, which says `Deleted zzznosuchstylexyz000000000.`
and exits 0, while `styles delete` on the same id returns 404. **The
confirmation is not evidence the style existed**, and there is no prompt
beforehand.

---

## Tilesets

Tiles by tileset id, plus the vector-tile lookup that keys off one: raster
from the Raster Tiles API, vector from the Vector Tiles API, and `query`
from the Tilequery API. Three operations, out of three different specs,
under one command group (#116) — a caller asking about a tileset is asking
the same kind of question regardless of which API answers it.

Not to be confused with [`mapbox tilesets-cli`](#tilesets-cli), which
forwards to the separately installed Python Tilesets CLI and shares
nothing with this but the word.

The two tile commands return bytes, so `--output` does not apply on
them — redirect to a file. `query` returns GeoJSON.

### `mapbox tilesets get-tile`

One raster tile at standard resolution, 256×256.

#### Parameters

`<tilesets>` may be several ids separated by commas, composited into one
tile. `<z> <x> <y>` are the tile coordinates.

`<format>` is `png`, `pngraw`, `jpg`, `jpeg` or `webp`. `<quality>` is
appended to it rather than being a separate URL segment, so `jpg` with `70`
fetches `.jpg70`; pass `""` for the format's default. Both are positional,
in that order.

#### Examples

```sh
mapbox tilesets get-tile mapbox.satellite 2 1 1 png "" > tile.png
mapbox tilesets get-tile mapbox.satellite 12 2048 1361 jpg 70 > tile.jpg70
```

#### Outputs

Image bytes; redirect to a file. The refusal to print them names an
extension matching what the API sent, which is not always what was asked
for — `mapbox.satellite` is stored as JPEG, so a `png` request still
suggests `out.jpg`.

<table>
<tr><th width="50%">Terminal — refuses</th><th width="50%">Redirected — raw bytes</th></tr>
<tr><td>

```
Error: Response is image/jpeg (9774 bytes).
Refusing to write it to the terminal — redirect
it to a file, e.g. `... > out.jpg`.
```

</td><td>

```
$ mapbox tilesets get-tile mapbox.satellite 2 1 1 png "" > tile.png
$ file tile.png
tile.png: JPEG image data
```

</td></tr>
</table>

### `mapbox tilesets get-mvt`

One vector tile from one or more Mapbox-hosted tilesets. Up to 15 ids,
comma-separated, composited into a single tile.

#### Parameters

`<format>` is `mvt` or `vector.pbf` — the same bytes under two names.

`--style <owner>/<style-id>[@<timestamp>]` asks the API to filter the tile
to what that style actually draws.

#### Examples

```sh
mapbox tilesets get-mvt mapbox.mapbox-streets-v8 12 2048 1361 mvt > tile.mvt
mapbox tilesets get-mvt mapbox.mapbox-streets-v8,mapbox.mapbox-terrain-v2 \
  12 2048 1361 mvt > composite.mvt
```

#### Outputs

Protobuf bytes; redirect to a file.

<table>
<tr><th width="50%">Terminal — refuses</th><th width="50%">Redirected — raw bytes</th></tr>
<tr><td>

```
Error: Response is application/vnd.mapbox-vector-tile
(80741 bytes). Refusing to write it to the terminal —
redirect it to a file, e.g. `... > out.mvt`.
```

</td><td>

```
$ mapbox tilesets get-mvt … > tile.mvt
$ ls -l tile.mvt
-rw-r--r--  80741  tile.mvt
```

</td></tr>
</table>

### `mapbox tilesets query`

What features a vector tileset has at or near a point.

`query` is a hand-picked name, not the one this command would otherwise
carry: tilequery's own spec spells its one operation's `operationId` after
its own URL path (`getV4TilesetsTilequeryLonLatJson`) rather than after
what it does, and that spec is not this repo's to rename. The name comes
from the maintainer-only decision record instead.

Neither older spelling still runs. `get-v4tilesets-tilequery-lon-lat-json`,
`tilequery get-tilequery` and `tilequery get` were each replaced in turn,
the last one folding the whole command into this group (#116) — see the
`Breaking` entry in the changelog.

#### Parameters

`<tilesets>` may be several ids separated by commas. `<lon>` and `<lat>` are
the point.

| Parameter | Effect |
| --- | --- |
| `--radius <m>` | How far around the point to look. `0` means exactly there. |
| `--limit <n>` | How many features. Default 5, max 50. |
| `--layers <names>` | Only these source layers. |
| `--geometry <type>` | Only `point`, `linestring` or `polygon`. |
| `--dedupe` | Drop duplicate features. |
| `--bands <names>` | Raster-array bands to sample. |
| `--language` / `--worldview` | As in geocoding. |

#### Examples

```sh
mapbox tilesets query \
  mapbox.mapbox-streets-v8 24.94 60.16 --limit 2

mapbox tilesets query \
  mapbox.mapbox-streets-v8 -74.0 40.7 --radius 100 --layers building
```

#### Outputs

GeoJSON — a `FeatureCollection` of what was found. Each feature carries a
`tilequery` property saying how far away it was and which layer it came
from; under `-o text` those render as a numbered list, same shape as
`geocoder`'s. `properties.name` is the label where a layer has one (a POI,
a place, a named road); where it doesn't, a `properties.type` the tileset
sent *as a string* stands in. Only some tilesets provide one — a
raster-array result has neither — so an entry with no name is a normal
answer, not a broken one.

What else a tileset puts in `properties` is its own business, and what the
lines above did not use is shown rather than dropped: every other property,
and everything in the `tilequery` object past `layer` and `distance`, gets
its own `key: value` line under the coordinates, in alphabetical order.
Used is the test, not the name — a POI carrying both `name` and `type` shows
`type` as an attribute, since `name` is what became its label.

The `tilequery` object's own values come in under dotted keys —
`tilequery.band`, `tilequery.zoom` — so that a tileset free to carry a
top-level `zoom` or `geometry` of its own keeps both.

A list of scalars goes on one line, comma-separated. What has structure
inside it does not fit a line at all and is left out: an object, or a list
with objects or lists in it. `-o json` is a flag away for those.

A vector tileset — `height` is one of the `building` layer's attributes, and
reaches the text column as its own line:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
1. building (building) — 0.0 m
   24.94,60.16
   height: 6.2

Tip: `-o json` for the response as the API sent it.
```

</td><td>

```json
{"features":[{"geometry":{"coordinates":[24.94,60.16],"type":"Point"},"properties":{"height":6.2,"tilequery":{"distance":0,"layer":"building"},"type":"building"},"type":"Feature"}],"type":"FeatureCollection"}
```

</td></tr>
</table>

A raster-array tileset queried with `--bands` answers with a different
shape: the sampled value under `val` — one number per band, so a list — and
a `tilequery` object naming the band, the zoom it was read at and the units,
with no `type` to stand in for a name and no `distance`. This one is the
spec's own `rasterarray` response example rather than a capture; this
account has no raster-array tileset to query.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
1. (unnamed) (data)
   -122.459,37.7754
   tilequery.band: frame-0
   tilequery.units: m
   tilequery.zoom: 6
   val: 1.23

Tip: `-o json` for the response as the API sent it.
```

</td><td>

```json
{"features":[{"geometry":{"coordinates":[-122.459,37.7754],"type":"Point"},"id":null,"properties":{"tilequery":{"band":"frame-0","layer":"data","units":"m","zoom":6},"val":[1.23]},"type":"Feature"}],"type":"FeatureCollection"}
```

</td></tr>
</table>

---

## Agent skills

Installs the [Mapbox Agent Skills](https://github.com/mapbox/mapbox-agent-skills)
— hand-written domain guidance for coding agents, covering cartography, token
security, style quality, geospatial operations and the mobile and web SDKs.
Twenty skills, each a directory of Markdown.

**Not the same as [`generate-skills`](#generate-skills)**, which is its
neighbor in `--help` and writes something else entirely: that one renders a
skill describing *this CLI's own commands*, from the specs compiled into the
binary. These are about using Mapbox; that one is about using `mapbox`. They
share the destination flags below and nothing else.

Neither command needs a token — the skills repository is public.

`/plugin marketplace add mapbox/mapbox-agent-skills` and
`npx skills add mapbox/mapbox-agent-skills` install the same content and are
not going away. This exists for the environments where npm is unavailable or
unapproved but a signed `mapbox` binary is, and so that people who install the
CLI find out the skills exist at all.

### Where they go

The destination flags are shared with `generate-skills` and behave
identically:

| Parameter | Effect |
| --- | --- |
| `--agent <AGENT>` | Repeatable. Defaults to whichever agents are installed. |
| `--global` | Write to the agent's home directory rather than this project. |
| `--dir <DIR>` | Write here and nowhere else. Conflicts with `--agent` and `--global`. |
| `--ref <REF>` | Branch, tag or commit to install from. Defaults to `main`; a commit SHA pins the install. |

Fifteen agents are known, and at project level most of them read the same
directory:

| Agent | `--agent` | This project | Home |
| --- | --- | --- | --- |
| Claude Code | `claude-code` | `.claude/skills` | `~/.claude/skills` |
| Codex | `codex` | `.agents/skills` | `~/.codex/skills` |
| Amp | `amp` | `.agents/skills` | `$XDG_CONFIG_HOME/agents/skills` |
| Cline | `cline` | `.agents/skills` | `~/.agents/skills` |
| Continue | `continue` | `.continue/skills` | `~/.continue/skills` |
| Cursor | `cursor` | `.agents/skills` | `~/.cursor/skills` |
| Gemini CLI | `gemini-cli` | `.agents/skills` | `~/.gemini/skills` |
| GitHub Copilot | `github-copilot` | `.agents/skills` | `~/.copilot/skills` |
| Goose | `goose` | `.goose/skills` | `$XDG_CONFIG_HOME/goose/skills` |
| Kiro CLI | `kiro-cli` | `.kiro/skills` | `~/.kiro/skills` |
| OpenCode | `opencode` | `.agents/skills` | `$XDG_CONFIG_HOME/opencode/skills` |
| Qwen Code | `qwen-code` | `.qwen/skills` | `~/.qwen/skills` |
| Roo Code | `roo` | `.roo/skills` | `~/.roo/skills` |
| Windsurf | `windsurf` | `.windsurf/skills` | `~/.codeium/windsurf/skills` |
| Zed | `zed` | `.agents/skills` | `~/.agents/skills` |

`.agents/skills` is the converging cross-tool convention; Claude Code is the
holdout. Agents that share a directory are one write, not several, so
`--agent codex --agent cursor --agent zed` installs once.

`CLAUDE_CONFIG_DIR` and `CODEX_HOME` relocate those two agents' directories,
skills included. `$XDG_CONFIG_HOME` falls back to `~/.config` on every
platform, macOS included, because that is where those agents look. An agent
not in the table is what `--dir` is for.

An agent counts as installed when its home directory exists — the presence of
somewhere to read a skill from, not a `PATH` lookup.

---

### `mapbox agent-skills list`

The published skills, with the first part of each description, and a `*` on
the ones already installed at the destinations that apply.

#### Parameters

The same destination flags as `install` — `--agent <AGENT>`, `--global`,
`--dir <DIR>` — and `--ref <REF>`, all described [above](#where-they-go).
Here they decide only which destinations the `*` is checked against.

Unlike `install`, this does not need a destination to exist: it lists what is
published even on a machine with no agent on it.

#### Examples

```sh
mapbox agent-skills list

mapbox agent-skills list --agent claude-code

mapbox agent-skills list --ref v1.0.0
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
20 skills published at mapbox/mapbox-agent-skills@main:
* mapbox-cartography      Expert guidance on map design…
  mapbox-ios-patterns     Official integration patterns…
  mapbox-token-security   Security best practices for…

* already installed here
```

</td><td>

```json
{
  "ref": "main",
  "repository": "mapbox/mapbox-agent-skills",
  "skills": [
    {
      "name": "mapbox-cartography",
      "description": "Expert guidance on map design principles…",
      "installed": [".claude/skills"]
    }
  ]
}
```

</td></tr>
</table>

The text column cuts each description to the line width; `json` carries them
whole.

---

### `mapbox agent-skills install`

Installs skills into every destination that applies. With no `NAME`, installs
all twenty.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `NAME` | Skill to install, repeatable. Defaults to all of them. An unknown name is an error that suggests the nearest published one. |
| `--force` | Replace a skill directory that is already there. |
| `--dry-run` | List the files it would write, then exit without writing them. |

Plus the destination flags and `--ref` [above](#where-they-go).

**An existing skill directory stops the install** — every conflict, across
every destination, is reported before anything is written. It is not a merge
and not an overwrite, because the directory may hold edits. `--force` replaces
it wholesale.

Each skill is written to a staging directory inside the destination and
renamed into place, so an interrupted install leaves either the old directory
or the complete new one. Upstream's `evals/` directory is test tooling and is
never installed. An archive entry that is not a regular file is skipped, and
one whose path would escape the destination stops the extraction outright.

#### Examples

```sh
mapbox agent-skills install

mapbox agent-skills install mapbox-cartography mapbox-token-security

mapbox agent-skills install --agent claude-code --global

mapbox agent-skills install --dir ./skills --ref v1.0.0

mapbox agent-skills install --dry-run
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
Installed 2 skills (9 files) from
mapbox/mapbox-agent-skills@main:
  .claude/skills (Claude Code, this project)
    mapbox-token-security
    mapbox-cartography
```

</td><td>

```json
{
  "dry_run": false,
  "ref": "main",
  "repository": "mapbox/mapbox-agent-skills",
  "skills": ["mapbox-token-security", "mapbox-cartography"],
  "destinations": [
    {
      "root": ".claude/skills",
      "source": "Claude Code, this project",
      "files": [".claude/skills/mapbox-cartography/SKILL.md"]
    }
  ]
}
```

</td></tr>
</table>

`--dry-run` opens with `Dry run — nothing was written.` and sets
`"dry_run": true`; everything else is the same, including the file list.

A conflict is `already_installed`, and names each directory in the way:

```
Error: These skills are already installed:
  ./out/mapbox-cartography
Fix: Pass --force to replace them, or name only the skills you want.
Next: mapbox agent-skills install --force
```

---

### `mapbox agent-skills update`

Re-installs the skills already here and reports what moved. With no `NAME`,
every skill that is installed at the destinations that apply.

It **never installs a skill that is not already there** — that is `install`'s
job, and an update that quietly added twenty directories because upstream
published them would be a different command. Naming one that is not installed
is an error suggesting `install`.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `NAME` | Skill to update, repeatable. Defaults to every skill already here. |
| `--dry-run` | Report what would change, then exit without changing it. |

Plus the destination flags — `--agent <AGENT>`, `--global`, `--dir <DIR>` —
and `--ref <REF>`, all [above](#where-they-go). There is no `--force`:
updating in place is the whole job.

#### What counts as changed

The installed files are compared with the published ones **byte for byte**,
in both directions: a file edited upstream, a file added, and a file left
behind that upstream no longer publishes all count. So does a local edit,
which `update` restores — that is what updating means.

Because it cannot tell an upstream change from one of yours, it **asks before
replacing anything**, at a terminal, the same `[y/N]` question every
destructive command asks; `--yes`/`MAPBOX_YES` answers it in advance and
`--dry-run` reports without asking. A skill installed for two agents is one
skill: the counts and the `updated`/`unchanged` arrays are per skill, and the
destinations say where it went.

There is no lock file and no recorded tree SHA. Upstream's `npx skills` keeps
one so that `update` can avoid *downloading* an unchanged skill; this command
has already downloaded every skill in one tarball before it could consult any
record, so there is nothing left to save, and comparing the bytes answers
exactly rather than approximately. Nothing this command writes has to be kept
in step with another tool's file.

#### Examples

```sh
mapbox agent-skills update

mapbox agent-skills update mapbox-cartography

mapbox agent-skills update --dry-run

mapbox agent-skills update --ref v1.1.0
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
Updated 1 skill from
mapbox/mapbox-agent-skills@main:
  .claude/skills (Claude Code, this project)
    mapbox-cartography
1 already up to date.
```

</td><td>

```json
{
  "dry_run": false,
  "ref": "main",
  "repository": "mapbox/mapbox-agent-skills",
  "updated": ["mapbox-cartography"],
  "unchanged": ["mapbox-token-security"],
  "destinations": [
    {
      "root": ".claude/skills",
      "source": "Claude Code, this project",
      "skills": ["mapbox-cartography"]
    }
  ]
}
```

</td></tr>
</table>

With nothing to do it says `Everything is up to date with
mapbox/mapbox-agent-skills@main.` and `"updated": []`.

---

### `mapbox agent-skills uninstall`

Removes installed skill directories. At least one `NAME` is required — an
empty list is not a license to remove everything.

**This is the one subcommand that makes no request.** It works from what is on
disk, which also means it can remove a skill that has since been unpublished.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `NAME` | Skill to remove, repeatable. Required. |
| `--force` | Remove a directory even though it holds no `SKILL.md`. |
| `--dry-run` | List what it would remove, then exit without removing it. |

Plus the destination flags — `--agent <AGENT>`, `--global`, `--dir <DIR>` —
[above](#where-they-go), which decide where it looks. There is no `--ref`:
nothing is fetched.

#### What stops it removing the wrong thing

There is no manifest, so this cannot know that *this* command installed a
given directory (see [`update`](#mapbox-agent-skills-update) for why there is
no manifest). Three things stand in for one:

- a `NAME` has to be one directory name. `..`, an absolute path and `a/b` are
  refused outright, `--force` included — this command removes directories,
  and `Path::join` on an absolute path replaces the destination rather than
  extending it;
- a directory holding no `SKILL.md` is refused until `--force`, since it is
  not shaped like an installed skill;
- at a terminal it asks before deleting, the same `[y/N]` question every
  destructive command asks — `--yes`/`MAPBOX_YES` answers it in advance;
- `--dry-run` lists the directories first.

A skill installed by `npx skills` looks exactly like one installed here, and
removing it will leave that tool's `.agents/.skill-lock.json` describing a
skill that is gone. Nothing here writes or repairs that file.

#### Examples

```sh
mapbox agent-skills uninstall mapbox-cartography

mapbox agent-skills uninstall mapbox-cartography mapbox-token-security

mapbox agent-skills uninstall mapbox-cartography --dry-run

mapbox agent-skills uninstall mapbox-cartography --global --agent claude-code
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
Removed 1 skill directory:
  .claude/skills/mapbox-cartography
```

</td><td>

```json
{
  "dry_run": false,
  "removed": [".claude/skills/mapbox-cartography"]
}
```

</td></tr>
</table>

`--dry-run` opens with `Dry run — nothing was removed.` and sets
`"dry_run": true`. A name that is not installed is `not_installed` and names
the directories it looked in.

---

## Completion

### `mapbox completion`

Prints a completion script for one shell on stdout. Makes no request, needs
no token, and writes nothing to disk.

The script is generated from the command tree this binary built, so it
completes exactly the commands this build has — a command group a spec sync added
is in it, and the withheld operations in the **Not shipped** column
[above](#api-command-groups) are absent from it for the same reason they are absent
from `--help`. Nothing about it is maintained by hand, and it cannot fall
behind the binary that printed it.

Commands, subcommands and flag names are completed. **Values are not** —
completing a style id, a tileset id or a username would mean an API request
and a live token in the middle of a keystroke.

`--output`/`-o` does not apply: the script is the result, and an envelope
around it would leave it unsourceable. Passing it explicitly earns a warning
on stderr; the ordinary `> file` redirect does not, and an exported
`MAPBOX_OUTPUT` does not.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `<SHELL>` | Required. One of `bash`, `zsh`, `fish`, `powershell`. |

#### Examples

Where each shell looks differs by machine — these are the common places, not
the only ones:

```sh
# bash: any file bash-completion loads
mapbox completion bash > ~/.local/share/bash-completion/completions/mapbox

# zsh: any directory on $fpath, and the filename has to be `_mapbox`
mapbox completion zsh > ~/.zfunc/_mapbox

# fish
mapbox completion fish > ~/.config/fish/completions/mapbox.fish

# for the current shell only, no file
source <(mapbox completion bash)
```

```powershell
# PowerShell: append to the profile, which is what $PROFILE names
mapbox completion powershell | Out-String | Invoke-Expression
mapbox completion powershell >> $PROFILE
```

#### Outputs

The same script in every output mode. The first lines of two of them:

<table>
<tr><th width="50%"><code>mapbox completion bash</code></th><th width="50%"><code>mapbox completion fish</code></th></tr>
<tr><td>

```
_mapbox() {
    local i cur prev opts cmd
    COMPREPLY=()
    if [[ "${BASH_VERSINFO[0]}" -ge 4 ]]; then
        cur="$2"
    else
        cur="${COMP_WORDS[COMP_CWORD]}"
    fi
```

</td><td>

```
complete -c mapbox -n "__fish_mapbox_needs_command" \
  -f -a "styles" -d 'Mapbox Styles API'
complete -c mapbox -n "__fish_mapbox_using_subcommand styles" \
  -f -a "list" -d 'List styles'
```

</td></tr>
</table>

A maintainer-only test harness checks these against the real shells: it
sources each script in the shell it names, and completes a command group, an
operation and a flag in the two that can be driven without a terminal.

---

## Generate skills

### `mapbox generate-skills`

Writes this CLI's own command surface out as an Agent Skill: a `mapbox-cli/`
directory holding `SKILL.md`, an `AGENTS.md` carrying the same prose, and one
`references/<service>.md` per command group. Makes no request and needs no token.

The content is rendered from the command tree this binary built, so it
describes exactly the commands that exist — the operations in the **Not
shipped** column [above](#api-command-groups), and the five liveness probes with
them, are absent from the skill for the same reason they are absent from
`--help`.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--agent <AGENT>` | `claude-code` or `codex`, repeatable. Defaults to every agent whose home directory is present. |
| `--global` | Write under the agent's home directory rather than this project. |
| `--dir <DIR>` | Write here and nowhere else. Conflicts with `--agent` and `--global`. |
| `--service <SERVICE>` | Describe only this command group, repeatable. Defaults to all of them. |
| `--force` | Replace a skill directory holding files this command did not write. |
| `--dry-run` | List the files it would write, then exit without writing them. |
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME` | Where each agent's home directory is, when it is not `~/.claude` / `~/.codex`. |

Without `--global`, a destination is relative to the current directory:
`.claude/skills` for Claude Code, `.agents/skills` for Codex.

#### Examples

```sh
mapbox generate-skills

mapbox generate-skills --agent claude-code --global

mapbox generate-skills --dry-run --dir ./out --service geocoder
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
Dry run — nothing was written.
Would write 3 files under ./out/mapbox-cli:
  SKILL.md
  AGENTS.md
  references/geocoder.md
```

</td><td>

```json
{
  "dry_run": true,
  "skill": "mapbox-cli",
  "destinations": [
    {
      "root": "./out/mapbox-cli",
      "source": null,
      "files": [
        "SKILL.md",
        "AGENTS.md",
        "references/geocoder.md"
      ]
    }
  ]
}
```

</td></tr>
</table>

Without `--dry-run` the first line reads `Wrote 3 files under …`, and `source`
names what chose the destination (`Claude Code, this project`) for anything
but `--dir`.

The directory is replaced whole rather than merged into. Every file carries a
``Generated by `mapbox generate-skills` `` marker in its first kibibyte, and a
directory holding anything without one is refused — with the paths named —
until `--force`.

---

## Uninstall

### `mapbox uninstall`

Removes the `mapbox` binary this process is running from — `std::env::current_exe()`,
not a path guessed from `PATH` or `scripts/install.sh`'s default. Nothing
else: stored credentials, other profiles' lock files, and the separately
installed `tilesets` binary all survive. Run `mapbox auth logout` first if
the credential store should go too.

At a terminal it asks before deleting, the same `[y/N]` `--yes` /
`MAPBOX_YES` question every other destructive command asks; piped or
scripted, it proceeds without asking, like everything else in this CLI.

On Unix the file is gone by the time the command returns — unlinking a
binary that is still executing is allowed; the process keeps its open
handle until it exits. Windows locks a running executable's file, so there
the deletion happens in a detached helper after this process exits, and the
message says so rather than claiming the file is already gone.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--dry-run` | Describe what this would delete, then exit without deleting it. |

#### Examples

```sh
mapbox uninstall

mapbox uninstall --yes

mapbox uninstall --dry-run
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
Removed /home/user/.local/bin/mapbox.
```

</td><td>

```json
{
  "path": "/home/user/.local/bin/mapbox",
  "removed": true
}
```

</td></tr>
</table>

`--dry-run` prints `Dry run — nothing was changed.\nWould delete …` and
`{ "dry_run": true, "path": … }` instead.

---

## Config

Settings that persist across shells and sessions — `~/.mapbox/config.json`
(or `$MAPBOX_CONFIG_DIR`), written the same way credentials are. One setting
today, `update-check`, which mirrors `MAPBOX_NO_UPDATE_CHECK` (see [Update
notices](../README.md#update-notices)) but stays off in every future shell
rather than only the one the environment variable was set in.

### `mapbox config get`

Prints a setting's current value: `on` in `text` mode, `true`/`false` in
`json`. Reading an unset `update-check` reports `on` — its default — rather
than failing, the same forgiving read the update-check cache itself uses.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `<key>` | Which setting to read. Only `update-check` exists today. |

#### Examples

```sh
mapbox config get update-check
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
on
```

</td><td>

```json
{
  "key": "update-check",
  "value": true
}
```

</td></tr>
</table>

### `mapbox config set`

Persists a setting to `~/.mapbox/config.json`, so it survives across shells
without an environment variable.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `<key>` | Which setting to change. Only `update-check` exists today. |
| `<value>` | `on` or `off`. |

#### Examples

```sh
mapbox config set update-check off

mapbox config set update-check on
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
update-check set to off.
```

</td><td>

```json
{
  "key": "update-check",
  "value": false
}
```

</td></tr>
</table>

### `mapbox config list`

Lists every setting and its current value — `get` answers one key at a
time, this answers all of them in one call, falling back to each one's
default the same way `get` does.

#### Parameters

None.

#### Examples

```sh
mapbox config list
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
update-check	on
```

</td><td>

```json
[
  {
    "key": "update-check",
    "value": true
  }
]
```

</td></tr>
</table>

### `mapbox config unset`

Clears a setting back to its default, rather than setting it to that
default value explicitly. The difference matters the next time this CLI
changes what a setting's default is: a cleared key picks up the new
default, a key explicitly set to the old default value does not.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `<key>` | Which setting to clear. Only `update-check` exists today. |

#### Examples

```sh
mapbox config unset update-check
```

#### Outputs

<table>
<tr><th width="50%"><code>text</code></th><th width="50%"><code>json</code></th></tr>
<tr><td>

```
update-check cleared, now on (default).
```

</td><td>

```json
{
  "key": "update-check",
  "value": true
}
```

</td></tr>
</table>

---

## Usage

### `mapbox usage`

Usage per Mapbox product, by day, for the account or one token. Calls the
Statistics API (`GET /statistics/v1`), which needs the `statistics:read`
scope. `mapbox auth login` requests it by default now; log in again if
your stored token predates that. Also takes `--token-id <id>` for one
token's usage instead of the whole account — see `mapbox accounts
list-tokens` for ids.

#### Parameters

| Parameter | Effect |
| --- | --- |
| `--period-start <YYYY-MM-DD>` | Start of the usage period, inclusive. Defaults to 30 days ago. |
| `--period-end <YYYY-MM-DD>` | End of the usage period, inclusive. Defaults to today. At most 31 days after `--period-start`. |
| `--product <name>` | Only show this product. Matches the API's own name for it case-insensitively, exactly or as a substring (e.g. `"search box"` matches `"Search Box API - Requests"`) — run without it first to see which names had usage. Client-side: the API has no such filter itself. |
| `--daily` | List each product's usage day by day instead of a sparkline. Only changes `-o text`; `-o json` always has the daily figures. |

#### Examples

```sh
mapbox usage

mapbox usage --period-start 2026-01-01 --period-end 2026-01-31

mapbox usage --product "Vector Tiles API"

mapbox usage --product "Vector Tiles API" --daily
```

#### Outputs

Run live. Numbers below are made up — the real response carries actual traffic
figures, which do not belong in a page committed to the repo — but the
shape, including the sort order (busiest product first), the sparkline, and
every line `-o text` prints around the table, is exactly what came back.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Usage · 2026-08-09 → 2026-09-08

PRODUCT                TOTAL  DAILY TREND
Directions API    67,840,000  ▁▄▇▆▆▅▅▇▇▇▇▆▅▅▄▇▇██▇▇▆▅▅▇▇██▇▇▆

Matrix API        45,260,000  ▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅

Vector Tiles API       6,300  ▆▁▆▁▁▁█▁▁▁▁▁▁█▁▁▁█▆▁█▄▁▁▁▁▄▁▁▁▁

Active days: 31

Generated 2026-09-08T09:49:10.827Z

Tips:
  `-o json` for the exact per-day numbers and the per-browser/country/host breakdown.
  `--daily` for the day-by-day numbers here instead of a sparkline.
  `--product "Directions API"` narrows to one product.
```

</td><td>

```json
{
  "data": {
    "period": { "start": "2026-08-09", "end": "2026-09-08" },
    "token_id": null,
    "activeDays": ["2026-08-09", "…", "2026-09-08"],
    "products": {
      "Directions API": {
        "daily": [{ "date": "2026-08-09", "usage": 1993852 }, "…"],
        "dimensions": { "browsers": ["…"], "countries": ["…"], "hosts": ["…"] }
      },
      "Matrix API": { "daily": ["…"], "dimensions": { "…": "…" } },
      "Vector Tiles API": { "daily": ["…"], "dimensions": { "…": "…" } }
    }
  },
  "generated_at": "2026-09-08T09:49:10.827Z"
}
```

</td></tr>
</table>

`-o text`'s table is this CLI's own summary (`render_text` in
`src/account_usage.rs`), not `render_human` in `src/output.rs`: the response
nests a `daily` array and a `dimensions` object under each product, too deep
for that generic renderer, which would fall back to the same pretty JSON
`-o json` prints. Each row is a product's total for the period plus a
sparkline of its daily values — a padded one: a day the API's `daily` array
leaves out (it omits a day rather than sending `usage: 0` for it) still gets
its own zero-height glyph at the right position in the line, computed from
`data.period`'s own start and end. Rows have a blank line between them —
without it, a sparkline's solid glyphs sitting flush against the next
row's read as cramped rather than dense, on an account with more than a
couple of products. Exact per-day numbers and the per-browser/country/host
breakdown are left to `-o json`; `--product` narrows the whole response,
both columns, to one product's row. A 403 covers two different causes the
API doesn't otherwise distinguish: the token missing `statistics:read` — a
login from before the scope was added, most likely, and fixed by logging in
again — or, less commonly, an account with no access to the Statistics API
at all, which needs Mapbox support. A 401 means the token itself is missing
or invalid.

`--daily` swaps every product's sparkline row for its own day-by-day
listing — same total, same period, newest day first, no `DAILY TREND`
column header (there is no single column to head), and each day
right-padded to the widest number in that product's own series rather than
across all products:

```
mapbox usage --product "Directions API" --daily
```

```
Usage · 2026-08-09 → 2026-09-08

Directions API — total 67,840,000
  2026-09-08  2,185,000
  …
  2026-08-11  2,200,000
  2026-08-10  2,150,000
  2026-08-09  2,100,000

Active days: 31

Generated 2026-09-08T09:49:10.827Z

Tips:
  `-o json` for the per-browser/country/host breakdown.
```

`-o json` is unaffected by `--daily` — the day-by-day figures are already
there under each product's `daily` array either way, which is why the tip
list drops the `--daily` suggestion once it's already in effect.

---

## Tilesets CLI

### `mapbox tilesets-cli <args…>`

Forwards everything verbatim to the separately installed `tilesets` binary
(PyPI `mapbox-tilesets`). On Unix it `exec`s, so this process is replaced and
never sees the child's output.

#### Parameters

| Parameter | Effect |
| --- | --- |
| Everything after the subcommand | Forwarded to `tilesets` untouched, including flags. |
| `--token`, `-t` *(before it)* | Injected into the child's environment, never its argv. |
| `--use-login` *(before it)* | Use stored credentials instead of `MAPBOX_ACCESS_TOKEN`. |
| `--profile <name>` *(before it)* | Which stored credentials to use. |
| `MAPBOX_TILESETS_CLI` | Path to a `tilesets` that is not on `PATH`. |

Globals must come **before** `tilesets-cli`; written after it they are
forwarded to the child, which does not know them. That earns a warning.

`--output`/`-o` does not apply — output comes from `tilesets`. Passing it
explicitly earns a warning; an exported `MAPBOX_OUTPUT` does not.

#### Examples

```sh
mapbox tilesets-cli list user

mapbox --use-login --profile work tilesets-cli upload-source user my-source data.geojson.ld
```

#### Outputs

Whatever `tilesets` prints. It is not uniform, and it is not always JSON:

<table>
<tr><th width="50%"><code>tilesets list</code> — bare IDs</th><th width="50%"><code>tilesets list -v</code> — JSON Lines</th></tr>
<tr><td>

```
user.cktok0000000000000000003c
user.city-boundaries
user.test-cli
```

</td><td>

```json
{"type":"vector","id":"user.city-boundaries",…}
{"type":"vector","id":"user.test-cli",…}
```

</td></tr>
</table>

So `mapbox tilesets-cli list <user> | jq .` fails, and `-v` is what you want.
`-v` output is JSON Lines, not an array — `jq .` works, `jq '.[0]'` does not.

The fix is `--output json` upstream in
[mapbox/tilesets-cli](https://github.com/mapbox/tilesets-cli); this CLI
does not translate the flag itself, by design, to avoid masking upstream's
own output contract.

---

## Errors

Errors go to **stderr in both modes**, and a failure writes nothing to
stdout — a redirected file is either a complete result or empty, never half
of each.

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Error: Not Authorized - Invalid Token (HTTP 401)
{
  "error_code": "INVALID_TOKEN",
  "message": "Not Authorized - Invalid Token"
}
Fix: The token was passed with `--token`, which outranks both MAPBOX_ACCESS_TOKEN and your login — so signing in again would change nothing. Check the token you passed, or drop the flag to use one of the others.
Next: mapbox auth whoami
Docs: https://docs.mapbox.com/api/search/geocoding-v6/
      https://docs.mapbox.com/api/accounts/tokens/
```

</td><td>

```json
{"body":{"error_code":"INVALID_TOKEN","message":"Not Authorized - Invalid Token"},"code":"http_401","docs":["https://docs.mapbox.com/api/search/geocoding-v6/","https://docs.mapbox.com/api/accounts/tokens/"],"fix":"The token was passed with `--token`, which outranks both MAPBOX_ACCESS_TOKEN and your login — so signing in again would change nothing. Check the token you passed, or drop the flag to use one of the others.","message":"Not Authorized - Invalid Token","next_actions":["mapbox auth whoami"],"status":401}
```

</td></tr>
</table>

A failure that is about the request rather than the credential names the
command that answers it — here a style id that does not exist, and the
listing the ids that do exist come from:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```
Error: Style not found (HTTP 404)
Fix: Nothing exists at that path. The id may be misspelled, or it may belong to an account other than the one this token is for.
Next: mapbox styles list --username user
      mapbox auth whoami
Docs: https://docs.mapbox.com/api/maps/styles/
```

</td><td>

```json
{"code":"http_404","docs":["https://docs.mapbox.com/api/maps/styles/"],"fix":"Nothing exists at that path. The id may be misspelled, or it may belong to an account other than the one this token is for.","message":"Style not found","next_actions":["mapbox styles list --username user","mapbox auth whoami"],"status":404}
```

</td></tr>
</table>

### Codes a caller can branch on

The error is the whole document — there is no `{"error": …}` wrapper, since
stderr carries nothing else machine-readable and stdout never carries a
failure. It always carries `code` and `message`. It carries `status` and
`body` when the failure came from an API response — `body` only when it says
something the message does not.

Three further fields carry what to do about it, each present only when there
is something to say. An empty list is never sent: a missing key already
answers "was this computed?".

| Field | Holds |
| --- | --- |
| `fix` | One line: why it failed, and what would make it work. |
| `next_actions` | Commands to run, and nothing else — no prose to strip before running one. |
| `docs` | The pages that bear on the failure: the command's own, plus the tokens page when it was the credential that was refused. |
| `request_id` | The response's request id, for quoting to Mapbox support. |

`request_id` is the one field whose two renderings differ on purpose. Under
`-o json` it is there on **every** failure that carried one, whatever the
status, because a caller logging failures wants it on all of them and a field
costs nothing to ignore. Under `-o text` it is printed for a **5xx only**:

```
Error: Internal server error (HTTP 500)
Fix: The service failed rather than refusing the request. Retry, and check https://status.mapbox.com if it persists.
Request ID: 01JC8K3Q7V9XZ4M2 (quote this to Mapbox support)
```

That is the failure a person escalates, and the id is what lets support find
the request in their logs. A 404 on a mistyped id is the reader's own to fix,
so an id under it would be noise on the common case.

The id is whatever identified the response: `x-request-id` from a service
that sends one, and otherwise `x-amz-cf-id`, the CloudFront id every Mapbox
response carries. Quote it as printed — support can trace either.

`mapbox agent-skills` is the one command whose failures carry no
`request_id`, and deliberately: it fetches from GitHub, which identifies
requests with its own header that Mapbox support cannot look up.

The advice is keyed on the HTTP status, with the command filling in what only
it knows — and it is read off the parsed spec, so a suggestion can only name
a command that exists. A 404 names the listing its ids come from, and the
account the failed request used. A 400 or 422 names `--schema`, which prints
what the command accepts without spending a request. A 403 asks about the
scope and the account, since the token was accepted there. A 5xx says to
retry and where to check whether the platform is degraded. A 401 is the
exception to the keying: its answer depends on which of `--token`, the
environment and a stored login supplied the token that failed, so it is
written where that is known (`src/auth.rs`) rather than in the status table
(`src/remedy.rs`). The example above is the typed-flag case, and the advice
names the flag rather than offering a login that could not outrank it.

| Code | Raised when |
| --- | --- |
| `http_<status>` | The API answered non-2xx. Carries `status` and the response `body`. |
| `request_failed` | Transport failure — proxy, DNS, TLS. Never carries the URL, because the access token rides in its query string. |

The CLI honors `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY`, and
needs no proxy configuration of its own. Two things that look like network
faults and are not:

- **`HTTP_PROXY` alone does not carry Mapbox requests.** Every base URL is
  `https`, and that variable covers `http` URLs only — so a request goes
  direct and a proxy-only network refuses it. `HTTPS_PROXY` or `ALL_PROXY` is
  the one to set.
- **SOCKS is not supported.** `ALL_PROXY=socks5://…` fails rather than being
  ignored, with `unsupported scheme socks5` in the message. `tests/proxy.rs`
  pins that wording, because a bare "the network failed" on a machine where
  every other tool works is the expensive version of this answer.

| `request_timed_out` | The request ran out of its time budget. Its own code because it is the one transport failure worth retrying or raising `--timeout` for. |
| `missing_path_parameters` | A `{username}`/`{owner}`/`{account}` placeholder went unresolved. |
| `invalid_path_parameter` | A path parameter was `.` or `..`, which would move the request to a different endpoint. Other URL syntax in a path parameter (`/`, `?`, `#`, `\`) is percent-encoded rather than refused, so it names a segment instead of changing the URL's shape. |
| `invalid_data` | `--data` was not valid JSON, or a `@<path>`/`@-` body was empty. |
| `invalid_file` | A file could not be read: one named by `--file`, or one named by `--data @<path>`. Also a `@<path>` that is not valid UTF-8, which a JSON body has to be. |
| `binary_response` | The response was bytes and stdout is a terminal. Redirect it to a file. |
| `missing_subcommand` | A command group was named with no operation. |
| `cancelled` | A delete was declined at the confirmation prompt. Nothing was sent. |
| `usage` | Anything else clap rejected. Exit code 2, not 1. |
| `error` | Unclassified. |

Exit codes: `0` success, `1` runtime failure, `2` usage error.

`--help` and `--version` are exempt — clap renders both, in every mode.
