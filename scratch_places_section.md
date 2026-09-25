## Places

Full detail for a place — hours, phone, website, photos, address,
coordinates, activity data — by the `mapbox_id` a Search Box API result
already returned. Curated by hand down to the parameters documented at
docs.mapbox.com/api/search/places — see `custom-openapi/README.md` for why
this command group doesn't come from the vendored specs the way most
others do.

**This API has no search or suggest of its own.** It only resolves ids
`search forward`/`reverse`/`category` already returned — the detail-view
follow-up to a search result, not a way to find places by name or location.

### `mapbox places get`

Full detail for one place.

#### Parameters

`<mapbox-id>` (positional) is required — from a Search Box API result.

#### Examples

```sh
mapbox places get dXJuOm1ieHBvaTpmYTE5Y2NhMC0yZmQ3LTQwMzgtYTEzNy02MzFmNGEwZDI5ODA
```

#### Outputs

Captured live, a real place found via `search forward --q "Ferry Building
San Francisco"`:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```json
{
  "mapbox_id": "dXJuOm1ieHBvaTpmYTE5Y2NhMC0yZmQ3LTQwMzgtYTEzNy02MzFmNGEwZDI5ODA",
  "name": "Ferry Building",
  "full_address": "San Francisco, California, 94105, United States",
  "primary_category": "food",
  "categories": ["cafe", "food", "food_and_drink"],
  "status": "active",
  "permanently_closed": false,
  "opening_hours": "Sa 08:00-14:00",
  "phone": "+14152373318",
  "website": "http://crumbleandwhisk.com/",
  "score": { "closed": 0, "reality": 0.973, "popularity": 0.275 },
  "coordinates": {
    "latitude": 37.79557765,
    "longitude": -122.39332918,
    "source": "poi",
    "routable_points": [
      { "name": "driving", "latitude": 37.795594, "longitude": -122.393338 }
    ]
  },
  "address": {
    "city": "San Francisco",
    "neighborhood": "Financial District",
    "postcode": "94105",
    "region": "California",
    "region_code_full": "US-CA",
    "country": "United States",
    "country_code": "US"
  }
}
```

</td><td>

```json
{"mapbox_id":"dXJuOm1ieHBvaTpmYTE5Y2NhMC0yZmQ3LTQwMzgtYTEzNy02MzFmNGEwZDI5ODA","name":"Ferry Building","full_address":"San Francisco, California, 94105, United States","primary_category":"food","categories":["cafe","food","food_and_drink"],"status":"active","permanently_closed":false,"opening_hours":"Sa 08:00-14:00","phone":"+14152373318","website":"http://crumbleandwhisk.com/","score":{"closed":0,"reality":0.973,"popularity":0.275},"coordinates":{"latitude":37.79557765,"longitude":-122.39332918,"source":"poi","routable_points":[{"name":"driving","latitude":37.795594,"longitude":-122.393338}]},"address":{"city":"San Francisco","neighborhood":"Financial District","postcode":"94105","region":"California","region_code_full":"US-CA","country":"United States","country_code":"US"}}
```

</td></tr>
</table>

Dropped `attributes` (14 boolean amenity flags — wheelchair access, payment
types, and the like), `created_at`/`updated_at`, and most `address` fields
that were `null` for this place, for length; the real response carries
them too. `brand` is `null` here since this isn't a chain location.

### `mapbox places batch`

Full detail for up to 100 places in one call — hydrates a whole list of
search results in one round trip instead of one `get` per id.

#### Parameters

`--data`/`-d` carries `{"ids": [...]}`, up to 100 `mapbox_id` strings.

#### Examples

```sh
mapbox places batch -d '{"ids": ["dXJuOm1ieHBvaTpmYTE5Y2NhMC0yZmQ3LTQwMzgtYTEzNy02MzFmNGEwZDI5ODA", "dXJuOm1ieHBvaTo4N2YzMmY2YS00MjkwLTQzNmItYWQyMi1hMzBhMzcxNWVmNzM"]}'
```

#### Outputs

Captured live, the same Ferry Building above plus a second real place:

<table>
<tr><th width="50%">Terminal — <code>-o text</code></th><th width="50%">Agent — <code>-o json</code></th></tr>
<tr><td>

```json
{
  "results": [
    { "mapbox_id": "…fa19cca0…", "name": "Ferry Building" },
    { "mapbox_id": "…87f32f6a…", "name": "Golden Gate Bridge" }
  ]
}
```

</td><td>

```json
{"results":[{"mapbox_id":"…fa19cca0…","name":"Ferry Building"},{"mapbox_id":"…87f32f6a…","name":"Golden Gate Bridge"}]}
```

</td></tr>
</table>

Each entry in `results` is the full record `get` returns, trimmed to
`mapbox_id`/`name` here for length. Not captured live: `206` with
`missing`/`unprocessed` alongside `results` — the documented shape for a
batch where some ids didn't resolve. Both ids used to write this page were
real and resolved, so triggering it would have meant fabricating a
plausibly-shaped but fake id, which defeats the point of a live capture.
