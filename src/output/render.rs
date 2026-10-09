//! Turning a response into something a person reads.
//!
//! Tables for lists, aligned field lists for single objects, and numbered
//! feature lists for `geocoder`, `search` and `tilesets query`. Every
//! function here returns text rather than printing it, so the shape of each
//! rendering is testable; `super::emit_value` decides where it goes.

use serde_json::Value;

use super::style;

/// The list rendering a response earns, if it earns one.
///
/// An exact service match, never a "looks like GeoJSON" test: a new service
/// answering with a `FeatureCollection` keeps the pretty-JSON fallback until
/// somebody has looked at its features and decided what a line of them should
/// say. `None` — an unlisted service, no service at all, or a value that is
/// not a `FeatureCollection` — falls through to [`render_human`].
///
/// The names are `Operation::service`, which is the command group, not the
/// spec file: `tilesets query` comes from the tilequery spec but answers as
/// `tilesets`. An arm still named `tilequery` after #116 moved the command
/// silently dropped its list for every release since; see
/// `every_listed_service_is_a_real_command_group`. `tilesets`'s other
/// commands answer with tile bytes, which never reach here.
pub(super) fn list_rendering(value: &Value, service: Option<&str>) -> Option<Rendered> {
    match service {
        Some("search") => match search_feature_rows(value) {
            Some(rows) => Some(render_feature_list(&rows)),
            None => render_category_table(value),
        },
        Some("geocoder") => {
            render_geocoder_list(value).or_else(|| render_batch_feature_list(value))
        }
        Some("tilesets") => tilequery_feature_rows(value).map(|rows| render_feature_list(&rows)),
        _ => None,
    }
}

/// A human rendering, what it had to cut, and the first identifier in it.
pub(super) struct Rendered {
    pub(super) text: String,
    pub(super) shortened: bool,
    /// Whether the first line is a table's column names.
    header: bool,
    /// The labels and values of a field list, kept so they can be drawn
    /// again with the labels highlighted.
    fields: Option<Vec<(String, String)>>,
    /// The whole text again in color, for a rendering that styles more than
    /// one header line or a column of labels.
    colored: Option<String>,
    /// The first row's key, when the table has one — a real value for the
    /// `--id` suggestion, so the line can be copied and edited rather than
    /// filled in from scratch.
    pub(super) identifier: Option<String>,
}

/// Widest a table column may start out, before `fit_to_line` narrows it.
const CELL: usize = 32;
/// Width a table aims to stay inside. Not the terminal's — std cannot ask —
/// but narrow enough to survive a split pane.
pub(crate) const LINE: usize = 100;
/// Spaces between columns.
const SEPARATOR: usize = 2;
/// Narrowest a column may be squeezed to before the table is left to wrap.
const FLOOR: usize = 8;

/// `search`'s `FeatureCollection`, as one row per result instead of GeoJSON.
///
/// `search forward`/`reverse`/`category` are asked for a list of POIs to
/// scan, the way any other listing is scanned — but as
/// [`render_feature_list`], not a table: see there for why. `geocoder` and
/// `tilesets query` read the same way and reach the same renderer through their
/// own row-builders; every other service's GeoJSON stays pretty-printed — see
/// `shapes_a_table_would_misrepresent_are_left_alone` below — because it is a
/// handful of features whose nesting *is* the content a table would throw
/// away.
///
/// `None` for anything that is not a `FeatureCollection`, so a caller falls
/// back to rendering the original, untouched value.
fn search_feature_rows(value: &Value) -> Option<Vec<Value>> {
    let map = value.as_object()?;
    if map.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return None;
    }
    let features = map.get("features")?.as_array()?;
    let rows: Vec<Value> = features.iter().map(search_result_row).collect();
    if rows.iter().any(row_is_empty) {
        return None;
    }
    Some(rows)
}

/// `list-category`'s response as a table, with no `--id` suggestion.
///
/// `render_table` would offer one anyway, keyed on `name` — but `pick_row`
/// only accepts a bare top-level array, and `value` here is
/// `{"listItems": […], "attribution": …}`, an object. The suggestion would
/// be a command guaranteed to answer `not_a_list`, so it is dropped rather
/// than shown.
fn render_category_table(value: &Value) -> Option<Rendered> {
    let rows = search_category_rows(value)?;
    render_table(&rows).map(|rendered| Rendered {
        identifier: None,
        ..rendered
    })
}

/// `search list-category`'s response, as table rows.
///
/// Unlike a POI result, a category is three short strings —
/// `canonical_id` (what `search category` takes), `name` and `icon` — with
/// nothing long enough to need [`render_feature_list`]'s one-per-line
/// treatment, so this feeds the ordinary `columns_for` table instead.
/// `uuid` and `version` are left out: both are, per the docs, generated
/// fresh each request and "not intended for use as a persistent
/// identifier" — noise in a table meant to be read, not diffed.
///
/// `None` for anything without a `listItems` array (or one holding
/// something other than objects), so a caller falls back to the original
/// value — the same contract [`search_feature_rows`] gives.
fn search_category_rows(value: &Value) -> Option<Vec<Value>> {
    let items = value.as_object()?.get("listItems")?.as_array()?;
    items
        .iter()
        .map(|item| {
            let item = item.as_object()?;
            let mut row = serde_json::Map::new();
            if let Some(id) = item.get("canonical_id") {
                row.insert("canonical_id".to_string(), id.clone());
            }
            if let Some(name) = item.get("name") {
                row.insert("name".to_string(), name.clone());
            }
            if let Some(icon) = item.get("icon") {
                row.insert("icon".to_string(), icon.clone());
            }
            Some(Value::Object(row))
        })
        .collect()
}

/// A feature's position as `longitude,latitude` — the one piece all three
/// row-builders read the same way, so they read it here.
///
/// That order matches every coordinate parameter in this CLI (`--proximity`,
/// `--longitude`/`--latitude`), so the pair can be copied straight into one.
/// Taken off the GeoJSON geometry rather than `properties.coordinates`, which
/// puts latitude first and repeats the two numbers alongside fields
/// (`accuracy`, `routable_points`) a row has no room for.
///
/// `>= 2` because a GeoJSON position may legally carry a third element for
/// altitude; the first two are the pair either way.
fn coordinate_cell(feature: &Value) -> Option<Value> {
    let [lon, lat] = feature
        .get("geometry")
        .and_then(|g| g.get("coordinates"))
        .and_then(Value::as_array)
        .filter(|c| c.len() >= 2)
        .map(|c| [&c[0], &c[1]])?;
    Some(Value::String(format!("{lon},{lat}")))
}

/// One `search` result, cut down to what `search_feature_rows` keeps.
///
/// `Null` when there is no `properties` object to read at all — one of the
/// shapes [`row_is_empty`] has `search_feature_rows` fall back on.
fn search_result_row(feature: &Value) -> Value {
    let props = match feature.get("properties").and_then(Value::as_object) {
        Some(props) => props,
        None => return Value::Null,
    };

    let mut row = serde_json::Map::new();
    // `as_str` rather than a clone: `render_feature_list` reads every field
    // as a string and shows nothing at all for anything else, so a
    // wrong-typed value kept here would go missing without a word. Empty is
    // absent too, or the entry renders as a bare `1. `.
    if let Some(name) = props
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        row.insert("name".to_string(), Value::String(name.to_string()));
    }
    // The coordinates are why most callers run a search command at all —
    // the next thing to do with a result is almost always feed its location
    // somewhere else (`--proximity` on another search, a static map,
    // `search reverse`). See `coordinate_cell` for the shape.
    if let Some(coordinates) = coordinate_cell(feature) {
        row.insert("coordinates".to_string(), coordinates);
    }
    // `full_address` is the one meant to be read on its own — it already
    // concatenates `address` and `place_formatted` — so it wins when
    // present; `address` (street-level only) or `place_formatted`
    // (everything but the street) stand in for a result missing it.
    // `address` first of the two: one line of a POI's location is more use
    // as `15885 Dam Road` than as the city and country it sits in, which the
    // next result over most likely shares.
    //
    // `as_str` on each candidate rather than on the result, so a JSON `null`
    // — or anything else that is not a string — falls through to the next
    // one instead of ending the chain with a value nothing can render.
    if let Some(address) = props
        .get("full_address")
        .and_then(Value::as_str)
        .or_else(|| props.get("address").and_then(Value::as_str))
        .or_else(|| props.get("place_formatted").and_then(Value::as_str))
    {
        row.insert("address".to_string(), Value::String(address.to_string()));
    }
    if let Some(meters) = props.get("distance").and_then(Value::as_f64) {
        row.insert(
            "distance".to_string(),
            Value::String(format!("{:.1} km", meters / 1000.0)),
        );
    }
    // Answers "why did this match" — the thing a name and an address don't:
    // a `poi_category` search for `coffee` returning a bridge or a visitor
    // center is exactly the case where knowing *what kind of place* this is
    // matters as much as where it is. POI categories first, since they're
    // the more specific claim; `feature_type` (`poi`, `address`, `place`, …)
    // stands in for a result — an address, an administrative area — that
    // has no category at all.
    let category = props
        .get("poi_category")
        .and_then(Value::as_array)
        .filter(|categories| !categories.is_empty())
        .map(|categories| {
            categories
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .or_else(|| {
            props
                .get("feature_type")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if let Some(category) = category {
        row.insert("category".to_string(), Value::String(category));
    }
    Value::Object(row)
}

/// A `FeatureCollection`'s features, as one row per result — named
/// generically since `geocoder` is the first caller, not the last.
///
/// `search` and `tilesets query` keep their own row-builders
/// ([`search_feature_rows`], [`tilequery_feature_rows`]) because what they
/// read out of a feature genuinely differs: a search result carries a
/// distance and a POI category a geocoding result has no equivalent of, and
/// a tilequery result carries whatever attributes its tileset happened to
/// put on the feature. All three hand their rows to the same
/// [`render_feature_list`], which is where the shapes do agree.
///
/// `None` for anything that is not a `FeatureCollection`, so the caller
/// falls back to the original value.
fn feature_collection_rows(value: &Value) -> Option<Vec<Value>> {
    let map = value.as_object()?;
    if map.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return None;
    }
    let features = map.get("features")?.as_array()?;
    let rows: Vec<Value> = features.iter().map(feature_row).collect();
    if rows.iter().any(row_is_empty) {
        return None;
    }
    Some(rows)
}

/// Whether a row has nothing in it to render.
///
/// `Null` is what all three row-builders — [`search_result_row`],
/// [`feature_row`], [`tilequery_row`] — answer for a feature with no
/// `properties` at all. An object with no fields is the same failure one step
/// later: `properties` was there and held nothing that builder recognized.
/// Both fall back the same way, because a numbered `(unnamed)` with no lines
/// under it looks like a result that is genuinely blank rather than like a
/// shape this renderer does not understand.
fn row_is_empty(row: &Value) -> bool {
    match row.as_object() {
        // The test is a field's shape, not its name: a field holding an empty
        // object says no more than an absent one, whichever field it is.
        Some(fields) => fields
            .values()
            .all(|value| value.as_object().is_some_and(serde_json::Map::is_empty)),
        None => true,
    }
}

/// A geocoding `FeatureCollection`, as its list with the notice under it.
fn render_geocoder_list(value: &Value) -> Option<Rendered> {
    let rows = feature_collection_rows(value)?;
    let mut rendered = render_feature_list(&rows);
    if let Some(notice) = attribution(value) {
        rendered.text.push_str(&notice_text(notice, false));
        if let Some(colored) = &mut rendered.colored {
            colored.push_str(&notice_text(notice, true));
        }
    }
    Some(rendered)
}

/// The terms under a list, dimmed so the results stay what the eye lands on.
fn notice_text(notice: &str, color: bool) -> String {
    format!("\n\n{}", style::paint(notice, &style::muted(), color))
}

/// A `FeatureCollection`'s `attribution`, when it carries a usable one.
///
/// Geocoding v6 requires the field on every response: it states the terms the
/// results come under, and `-o text` dropping it left the one legally
/// interesting line of the answer readable only under `-o json`.
///
/// Note the asymmetry with [`render_batch_feature_list`], which refuses to
/// render at all when a second key sits beside `batch`. There the extra key
/// would be *lost* by rendering only what is recognized, so bailing out to
/// JSON is how nothing goes missing; here the extra key is the thing being
/// carried through, so there is nothing to bail out for.
fn attribution(value: &Value) -> Option<&str> {
    value
        .get("attribution")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|notice| !notice.is_empty())
}

/// One feature, cut down to what [`feature_collection_rows`] keeps.
///
/// `Null` when there is no `properties` object — the shape
/// `feature_collection_rows` rejects as unexpected.
fn feature_row(feature: &Value) -> Value {
    let props = match feature.get("properties").and_then(Value::as_object) {
        Some(props) => props,
        None => return Value::Null,
    };

    let mut row = serde_json::Map::new();
    // An empty `name` is treated as absent, same as in `tilequery_row` —
    // otherwise the entry renders as a bare `1. ` with a trailing space.
    if let Some(name) = props
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        row.insert("name".to_string(), Value::String(name.to_string()));
    }
    if let Some(coordinates) = coordinate_cell(feature) {
        row.insert("coordinates".to_string(), coordinates);
    }
    // `full_address` first; `place_formatted` stands in when it's missing
    // (a bare place or region). `as_str` on each rather than on the result,
    // so an explicit JSON `null` — or anything else that is not a string —
    // falls through to the stand-in instead of landing in the row raw for
    // `render_feature_list` to drop without saying so.
    if let Some(address) = props
        .get("full_address")
        .and_then(Value::as_str)
        .or_else(|| props.get("place_formatted").and_then(Value::as_str))
    {
        row.insert("address".to_string(), Value::String(address.to_string()));
    }
    if let Some(feature_type) = props.get("feature_type").and_then(Value::as_str) {
        row.insert(
            "category".to_string(),
            Value::String(feature_type.to_string()),
        );
    }
    Value::Object(row)
}

/// `tilesets query`'s features, as one row per result — same `feature_collection_rows`
/// contract, different fields: labeled layers (`poi_label`, `place_label`, a
/// named `road`, …) carry `properties.name`; unlabeled ones (`building`,
/// `landuse`, …) don't, and where the tileset sends `properties.type` as a
/// string that stands in. Not every tileset does — a raster-array feature has
/// neither — so a row with no name at all is a normal result, not a
/// malformed one. `properties.tilequery.layer` is the category and
/// `properties.tilequery.distance` the distance in meters; everything else
/// the tileset sent is carried under `extra` rather than dropped.
fn tilequery_feature_rows(value: &Value) -> Option<Vec<Value>> {
    let map = value.as_object()?;
    if map.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return None;
    }
    let features = map.get("features")?.as_array()?;
    let rows: Vec<Value> = features.iter().map(tilequery_row).collect();
    if rows.iter().any(row_is_empty) {
        return None;
    }
    Some(rows)
}

/// `Null` when there is no `properties` object — same contract as
/// [`feature_row`].
///
/// What a tileset puts in `properties` is its own business and nothing here
/// can know it in advance: a `mapbox-streets-v8` building carries `class`,
/// `height`, `min_height`, `extrude` and `underground`; a raster-array
/// feature carries `val` — one number per band, so a list — with `band`,
/// `zoom` and `units` inside its `tilequery` object. So everything a line can
/// carry that was not *consumed* by a named field above goes into `extra`,
/// because the attribute a caller queried *for* is exactly the one that must
/// not vanish. Consumed is the test, not the key's name: `type` is held back
/// only when it actually stood in for a missing `name`, and shown as an
/// attribute when `name` won. [`fits_on_a_line`] draws the limit; what it
/// rejects is a `-o json` away.
///
/// The `tilequery` object's leftovers come in on dotted keys
/// (`tilequery.band`), the same shape `field_pairs` flattens one level of
/// nesting onto — a tileset may carry a top-level `zoom` or `geometry` of its
/// own, and a bare key would let one quietly overwrite the other.
///
/// `extra` comes out alphabetically, since this crate's `serde_json` has no
/// `preserve_order` and a `Map` is a `BTreeMap`. Stable is what matters —
/// the source order is the API's, not the tileset author's.
fn tilequery_row(feature: &Value) -> Value {
    let props = match feature.get("properties").and_then(Value::as_object) {
        Some(props) => props,
        None => return Value::Null,
    };

    let mut row = serde_json::Map::new();
    // `name` is the human label where a layer has one (poi_label, place_label, a
    // named road, …); layers with no notion of a name (building, landuse, …)
    // never send the field, so falling through to `type` — the specific type,
    // e.g. `building:part` — never displaces a real name. Not every tileset the
    // caller can name is `mapbox-streets-v8`, so `type` is checked for `str`
    // rather than assumed: a vector tile attribute is a tag with no schema
    // behind it and can be any MVT value type.
    let label = props
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    // Read only when `name` gave nothing, so that `type` is *consumed* here
    // exactly when it is shown here. Everything not consumed is an attribute
    // like any other and belongs in `extra` below.
    let stand_in = match label {
        Some(_) => None,
        None => props.get("type").and_then(Value::as_str),
    };
    if let Some(name) = label.or(stand_in) {
        row.insert("name".to_string(), Value::String(name.to_string()));
    }
    if let Some(coordinates) = coordinate_cell(feature) {
        row.insert("coordinates".to_string(), coordinates);
    }
    let tilequery = props.get("tilequery").and_then(Value::as_object);
    if let Some(layer) = tilequery
        .and_then(|t| t.get("layer"))
        .and_then(Value::as_str)
    {
        row.insert("category".to_string(), Value::String(layer.to_string()));
    }
    if let Some(meters) = tilequery
        .and_then(|t| t.get("distance"))
        .and_then(Value::as_f64)
    {
        row.insert(
            "distance".to_string(),
            Value::String(format!("{meters:.1} m")),
        );
    }

    // A key is held back only where it was actually used above. `type` is the
    // one that bites: a `poi_label` feature carries both `name` ("Bangkok9")
    // and `type` ("Restaurant"), and once `name` wins the label, `type` is a
    // plain attribute like any other — holding it back anyway dropped the
    // category, which is half of what a POI result says.
    let consumed = |key: &str| match key {
        // A string `name` either became the label or was empty and said
        // nothing; anything else was never usable as one and stays an
        // attribute, so that a numeric `name` is shown rather than swallowed.
        "name" => props.get("name").is_some_and(Value::is_string),
        "type" => stand_in.is_some(),
        "tilequery" => tilequery.is_some(),
        _ => false,
    };
    let mut extra: serde_json::Map<String, Value> = props
        .iter()
        .filter(|(key, value)| !consumed(key) && fits_on_a_line(value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    // The `tilequery` object's own leftovers — `band`, `zoom`, `units` on a
    // raster-array result — sit a level down and read the same way as the
    // top-level ones, so they are flattened in beside them, on the dotted
    // keys `field_pairs` already flattens one level of nesting onto.
    // Qualifying them is not cosmetic: a tileset is free to carry a top-level
    // attribute named `geometry` or `zoom` too, and a bare key let one
    // silently overwrite the other on the way in.
    if let Some(tilequery) = tilequery {
        extra.extend(
            tilequery
                .iter()
                .filter(|(key, value)| {
                    !matches!(key.as_str(), "layer" | "distance") && fits_on_a_line(value)
                })
                .map(|(key, value)| (format!("tilequery.{key}"), value.clone())),
        );
    }
    if !extra.is_empty() {
        row.insert("extra".to_string(), Value::Object(extra));
    }

    Value::Object(row)
}

/// A `FeatureCollection`'s results, numbered: name, category and distance on
/// one line, the address on the next, coordinates on the one after, then one
/// `key: value` line per `extra` field.
///
/// Not a table. A table's column is a fixed width shared by every row, and a
/// street address is exactly the field that width can't be chosen for: narrow
/// enough to fit `--limit 10` results in 100 columns and it clips almost
/// every address to a few characters and an ellipsis, which is a worse answer
/// than the table not existing. A list costs a blank line between results
/// instead, and shows every name and every address whole.
///
/// One renderer for all three services that get a list. Which fields a row
/// carries is its own row-builder's business, and every field here is skipped
/// when absent: `extra` is `tilesets query`'s alone, and `distance` arrives
/// already formatted — `km` from `search`, `m` from `tilesets query` — so the unit
/// is the builder's decision rather than this function's.
fn render_feature_list(rows: &[Value]) -> Rendered {
    if rows.is_empty() {
        return Rendered {
            text: "(none)".to_string(),
            header: false,
            fields: None,
            colored: None,
            shortened: false,
            identifier: None,
        };
    }

    // Never clipped, so nothing to warn about; no column to suggest `--id`
    // against either.
    Rendered {
        text: feature_list_text(rows, false),
        header: false,
        fields: None,
        colored: Some(feature_list_text(rows, true)),
        shortened: false,
        identifier: None,
    }
}

/// [`render_feature_list`]'s text. In color, the name is bold and the
/// category, distance, coordinates and attribute names are dimmed, leaving
/// the name and address as what a reader scans.
fn feature_list_text(rows: &[Value], color: bool) -> String {
    let dim = |text: &str| style::paint(text, &style::muted(), color);
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.push_str("\n\n");
        }
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)");
        out.push_str(&format!(
            "{}. {}",
            index + 1,
            style::paint(name, style::BOLD, color)
        ));
        if let Some(category) = row.get("category").and_then(Value::as_str) {
            out.push_str(&format!(" {}", dim(&format!("({category})"))));
        }
        if let Some(distance) = row.get("distance").and_then(Value::as_str) {
            out.push_str(&format!(" {}", dim(&format!("— {distance}"))));
        }
        if let Some(address) = row.get("address").and_then(Value::as_str) {
            out.push_str(&format!("\n   {address}"));
        }
        if let Some(coordinates) = row.get("coordinates").and_then(Value::as_str) {
            out.push_str(&format!("\n   {}", dim(coordinates)));
        }
        // Only `tilesets query` fills this in; a geocoding row never carries it.
        if let Some(extra) = row.get("extra").and_then(Value::as_object) {
            for (key, value) in extra {
                let rendered = match value {
                    Value::Array(items) => join_list(items),
                    scalar => cell(Some(scalar)),
                };
                out.push_str(&format!("\n   {} {rendered}", dim(&format!("{key}:"))));
            }
        }
    }
    out
}

/// `batch-geocode`'s `{"batch": [FeatureCollection, …]}`, one query's list
/// per block, each numbered from 1. `None` unless every entry is a
/// `FeatureCollection` `feature_collection_rows` accepts — one malformed
/// query falls the whole batch back to the untouched value, same as
/// `feature_collection_rows` itself does for one malformed feature.
fn render_batch_feature_list(value: &Value) -> Option<Rendered> {
    let map = value.as_object()?;
    if map.len() != 1 {
        return None;
    }
    let queries = map.get("batch")?.as_array()?;
    let lists: Vec<Vec<Value>> = queries
        .iter()
        .map(feature_collection_rows)
        .collect::<Option<_>>()?;

    // Every entry carries its own `attribution` and it is the API's terms
    // rather than the query's, so all fifty of them say the same thing. Once
    // under the whole batch, then — the same notice repeated under every
    // block would bury the results it belongs to. Two that genuinely differ
    // both survive, in the order they arrived.
    let mut notices: Vec<&str> = Vec::new();
    for notice in queries.iter().filter_map(attribution) {
        if !notices.contains(&notice) {
            notices.push(notice);
        }
    }

    let text = |color: bool| {
        let mut out = String::new();
        for (index, rows) in lists.iter().enumerate() {
            if index > 0 {
                out.push_str("\n\n");
            }
            // One query needs no header: there is nothing to tell it apart from.
            if lists.len() > 1 {
                let header = format!("Query {}:", index + 1);
                out.push_str(&format!("{}\n", style::paint(&header, style::BOLD, color)));
            }
            let list = render_feature_list(rows);
            match (color, list.colored) {
                (true, Some(colored)) => out.push_str(&colored),
                _ => out.push_str(&list.text),
            }
        }
        for notice in &notices {
            out.push_str(&notice_text(notice, color));
        }
        out
    };

    Some(Rendered {
        text: text(false),
        header: false,
        fields: None,
        colored: Some(text(true)),
        shortened: false,
        identifier: None,
    })
}

/// Renders a response for a person, or gives up.
///
/// Deliberately driven by the response in hand rather than by the spec that
/// described it: the shape is right there at runtime, it needs no `$ref`
/// resolution, and it works the same for a service nobody has looked at.
/// Giving up is a real answer — GeoJSON, a bare value, or rows with nothing
/// in common all read better as JSON than as a table pretending they fit.
pub(super) fn render_human(value: &Value) -> Option<Rendered> {
    match value {
        Value::Array(rows) => render_table(rows),
        Value::Object(map) => {
            // A listing the API wrapped in a one-key object is still a
            // listing: `{"path":[…]}`, `{"icons":[…]}`. Without this it falls
            // all the way through to pretty-printed JSON — one style's
            // iconset is 3 MB that way, against 440 lines as a table.
            if let Some(table) = wrapped_list(map).and_then(render_table) {
                return Some(table);
            }
            // Field lists never clip: they have the room.
            field_pairs(value).map(|fields| Rendered {
                text: field_lines(&fields, false),
                header: false,
                fields: Some(fields),
                colored: None,
                shortened: false,
                identifier: None,
            })
        }
        _ => None,
    }
}

/// The rows of a listing that arrived inside a one-key object.
///
/// Only one key: two of them (`{"files":[…],"folders":[…]}`) are two listings,
/// and picking one to show would hide the other.
fn wrapped_list(map: &serde_json::Map<String, Value>) -> Option<&[Value]> {
    if map.len() != 1 {
        return None;
    }
    map.values().next()?.as_array().map(Vec::as_slice)
}

/// A table, when the array really is rows of like things.
fn render_table(rows: &[Value]) -> Option<Rendered> {
    if rows.is_empty() {
        return Some(Rendered {
            text: "(none)".to_string(),
            header: false,
            fields: None,
            colored: None,
            shortened: false,
            identifier: None,
        });
    }
    let objects: Vec<&serde_json::Map<String, Value>> =
        rows.iter().filter_map(Value::as_object).collect();
    if objects.len() != rows.len() {
        return None;
    }

    let columns = columns_for(&objects);
    if columns.is_empty() {
        return None;
    }

    let mut widths: Vec<usize> = columns
        .iter()
        .map(|name| {
            objects
                .iter()
                .map(|row| table_cell(row.get(name)).chars().count())
                .chain(std::iter::once(name.chars().count()))
                .max()
                .unwrap_or(0)
                .min(CELL)
        })
        .collect();
    fit_to_line(&columns, &mut widths);

    let mut out = String::new();
    let header: Vec<String> = columns.iter().map(|c| c.to_uppercase()).collect();
    out.push_str(&join_row(&header, &widths));

    let mut shortened = false;
    for row in &objects {
        let cells: Vec<String> = columns.iter().map(|c| table_cell(row.get(c))).collect();
        shortened |= cells
            .iter()
            .zip(&widths)
            .any(|(text, width)| text.chars().count() > *width);
        out.push('\n');
        out.push_str(&join_row(&cells, &widths));
    }

    // A table is for scanning and for copying an identifier out of; it is not
    // where you read a value in full. Say where that is, once, and only when
    // something was actually clipped — on stderr, because it is advice about
    // the result rather than part of it.
    let identifier = columns
        .first()
        .filter(|name| matches!(name.as_str(), "id" | "name"))
        .and_then(|name| Some(objects.first()?.get(name)?.as_str()?.to_string()));

    Some(Rendered {
        text: out,
        header: true,
        fields: None,
        colored: None,
        shortened,
        identifier,
    })
}

/// A single object as the `(name, value)` pairs of a field list.
///
/// One level of nesting is flattened onto dotted keys, because dropping it
/// loses the answer: `accounts retrieve-token` puts everything worth reading
/// inside `token`, and a scalars-only view rendered the whole response as
/// `code  TokenValid`. Deeper than that, or an array, and the structure is
/// the information — those fall back to JSON.
fn field_pairs(value: &Value) -> Option<Vec<(String, String)>> {
    let object = value.as_object()?;
    let mut fields: Vec<(String, String)> = Vec::new();

    for (key, value) in object {
        match value {
            v if is_scalar(v) => fields.push((key.clone(), cell(Some(v)))),
            Value::Array(items) if items.iter().all(is_scalar) => {
                fields.push((key.clone(), join_list(items)))
            }
            Value::Object(nested) => {
                for (inner, v) in nested {
                    let name = format!("{key}.{inner}");
                    match v {
                        v if is_scalar(v) => fields.push((name, cell(Some(v)))),
                        Value::Array(items) if items.iter().all(is_scalar) => {
                            fields.push((name, join_list(items)))
                        }
                        _ => return None,
                    }
                }
            }
            _ => return None,
        }
    }

    if fields.is_empty() {
        return None;
    }

    Some(fields)
}

/// Aligned `label  value` lines, with the labels in bold.
///
/// Every key/value list a person reads goes through here — a response's
/// fields, `auth whoami`, `doctor` — so they align and highlight the same
/// way. Padding is added outside the escapes, so color never shifts a
/// column.
pub fn field_lines<L: AsRef<str>>(fields: &[(L, String)], color: bool) -> String {
    let width = fields
        .iter()
        .map(|(label, _)| label.as_ref().chars().count())
        .max()
        .unwrap_or(0);
    fields
        .iter()
        .map(|(label, value)| {
            let label = label.as_ref();
            let pad = " ".repeat(width - label.chars().count() + 2);
            format!("{}{pad}{value}", style::paint(label, style::BOLD, color))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A list of scalars on one line. One row of a table cannot afford this, but
/// a field list can — and `retrieve-token`'s thirteen scopes are the answer
/// to the question that command is usually asked.
fn join_list(items: &[Value]) -> String {
    if items.is_empty() {
        return "(none)".to_string();
    }
    let rendered: Vec<String> = items.iter().map(|v| cell(Some(v))).collect();
    let joined = rendered.join(", ");
    if joined.chars().count() <= LINE {
        return joined;
    }

    let mut shown = String::new();
    let mut used = 0usize;
    for (i, item) in rendered.iter().enumerate() {
        if used + item.chars().count() + 2 > LINE.saturating_sub(12) {
            return format!("{shown}… (+{} more)", rendered.len() - i);
        }
        if !shown.is_empty() {
            shown.push_str(", ");
            used += 2;
        }
        shown.push_str(item);
        used += item.chars().count();
    }
    shown
}

/// The columns worth showing.
///
/// Three rules, each earned on a real response — `accounts list-tokens` for
/// one account returns ninety-six rows and eleven fields, and a table of all
/// eleven is wider than any terminal and mostly noise:
///
/// - **Most rows must have it.** Three of those ninety-six carry a `token`,
///   and a column blank for the other ninety-three earns none of the width
///   it costs. Tidiness, not secrecy — ask for `--usage pk` and every row
///   has a token, so every row shows one.
/// - **It must vary.** `usage` is `sk` on every row and `default` is `no` on
///   every row. A column with one value in it says nothing a sentence
///   couldn't, and costs width the varying columns need.
/// - **It must not repeat another column.** `modified` equals `created` on
///   every row; showing both is the same date twice.
///
/// `id` and `name` lead because that is what a reader looks for first.
fn columns_for(rows: &[&serde_json::Map<String, Value>]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for row in rows {
        for (key, value) in row.iter() {
            if !is_scalar(value) {
                continue;
            }
            match counts.iter_mut().find(|(name, _)| name == key) {
                Some((_, n)) => *n += 1,
                None => counts.push((key.clone(), 1)),
            }
        }
    }

    let majority = rows.len().div_ceil(2);
    let mut columns: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n >= majority)
        .map(|(name, _)| name)
        .collect();

    columns.sort_by_key(|name| match name.as_str() {
        "id" => 0,
        "name" => 1,
        _ => 2,
    });

    // A single row has nothing to vary against, so both rules below would
    // strip it to nothing.
    if rows.len() > 1 {
        let column_of = |name: &str| -> Vec<String> {
            rows.iter().map(|row| table_cell(row.get(name))).collect()
        };

        let mut kept: Vec<String> = Vec::new();
        let mut seen: Vec<Vec<String>> = Vec::new();
        for name in columns {
            let values = column_of(&name);
            let varies = values.iter().any(|v| v != &values[0]);
            if varies && !seen.contains(&values) {
                seen.push(values);
                kept.push(name);
            }
        }
        columns = kept;
    }

    columns
}

/// Narrows a table until it fits, widest column first.
///
/// Trimming trailing columns instead would drop whatever the API happened to
/// put last, which on `list-tokens` is the note — the one field a person is
/// actually scanning for. Narrowing costs some characters of the longest
/// values and keeps every column; the untruncated values are a `-o json`
/// away. `FLOOR` stops a column shrinking past the point of carrying
/// anything, and a header never gets clipped.
fn fit_to_line(headers: &[String], widths: &mut [usize]) {
    let total = |w: &[usize]| w.iter().sum::<usize>() + SEPARATOR * w.len().saturating_sub(1);

    while total(widths) > LINE {
        let floors: Vec<usize> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                // An identifier is for copying — half of one is no use at
                // all, so it keeps its width and the other columns pay.
                if i == 0 && matches!(h.as_str(), "id" | "name") {
                    return widths[i];
                }
                h.chars().count().max(FLOOR)
            })
            .collect();

        let widest = widths
            .iter()
            .enumerate()
            .filter(|(i, w)| **w > floors[*i])
            .max_by_key(|(_, w)| **w)
            .map(|(i, _)| i);

        match widest {
            Some(i) => widths[i] -= 1,
            // Everything is at its floor; a very wide table simply wraps.
            None => break,
        }
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

/// Whether one `key: value` line can carry a value honestly.
///
/// A scalar, or a list of them — tilequery's `val` is one number per band,
/// so a single-band query is a one-element array and dropping arrays would
/// have lost the answer itself. Anything deeper is structure, and structure
/// is what `-o json` is for.
fn fits_on_a_line(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().all(is_scalar),
        other => is_scalar(other),
    }
}

/// One value, rendered whole. Absent and null both read as `-`: a blank
/// leaves a reader wondering whether the value is empty or the field is.
///
/// Nothing is shortened here. A field list has room for the real value, and
/// the table clips to its own fitted column widths — doing it twice once
/// gave `expires  2026-09-01` for a token with hours left on it.
fn cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "-".to_string(),
        Some(Value::Bool(b)) => if *b { "yes" } else { "no" }.to_string(),
        Some(Value::String(s)) => s.to_string(),
        Some(other) => other.to_string(),
    }
}

/// A table cell: the value, with a timestamp reduced to its date.
fn table_cell(value: Option<&Value>) -> String {
    let text = cell(value);
    shorten_timestamp(&text)
}

/// `2026-06-10T09:10:53.850Z` reads as `2026-06-10`. The time is real
/// information, but not at a glance and not at 24 characters a column — the
/// full value is a `-o json` away.
fn shorten_timestamp(text: &str) -> String {
    let looks_iso = text.len() >= 20
        && text.as_bytes()[10] == b'T'
        && text.ends_with('Z')
        && text[..10].chars().all(|c| c.is_ascii_digit() || c == '-');

    if looks_iso {
        text[..10].to_string()
    } else {
        text.to_string()
    }
}

fn join_row(cells: &[String], widths: &[usize]) -> String {
    let padded: Vec<String> = cells
        .iter()
        .zip(widths)
        .map(|(text, width)| {
            let len = text.chars().count();
            if len > *width {
                let clipped: String = text.chars().take(width.saturating_sub(1)).collect();
                return format!("{clipped}…");
            }
            format!("{text}{}", " ".repeat(width - len))
        })
        .collect();
    padded.join(&" ".repeat(SEPARATOR)).trim_end().to_string()
}

/// The rendered text, with a table's column names, a field list's labels or
/// a feature list's names in bold.
pub(super) fn styled(rendered: &Rendered, color: bool) -> String {
    if !color {
        return rendered.text.clone();
    }
    if let Some(colored) = &rendered.colored {
        return colored.clone();
    }
    if let Some(fields) = &rendered.fields {
        return field_lines(fields, true);
    }
    if !rendered.header {
        return rendered.text.clone();
    }
    match rendered.text.split_once('\n') {
        Some((header, rows)) => format!("{}\n{rows}", style::paint(header, style::BOLD, true)),
        None => style::paint(&rendered.text, style::BOLD, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(json: &str) -> Value {
        serde_json::from_str(json).expect("test fixture parses")
    }

    #[test]
    fn only_a_tables_first_line_is_bold() {
        let table =
            render_table(rows(r#"[{"id":"a","name":"x"}]"#).as_array().unwrap()).expect("renders");
        let colored = styled(&table, true);
        let (header, rest) = colored.split_once('\n').expect("two lines");
        assert!(header.starts_with(style::BOLD) && header.ends_with(style::RESET));
        assert!(!rest.contains('\x1b'), "{rest:?}");
        assert_eq!(style::strip(&colored), table.text);
        assert_eq!(styled(&table, false), table.text);
    }

    #[test]
    fn a_field_lists_labels_are_bold_and_stay_aligned() {
        let fields = render_human(&rows(r#"{"id":"a","owner":"x"}"#)).expect("renders");
        assert_eq!(fields.text, "id     a\nowner  x");
        let colored = styled(&fields, true);
        assert_eq!(
            colored,
            format!(
                "{b}id{r}     a\n{b}owner{r}  x",
                b = style::BOLD,
                r = style::RESET
            )
        );
        assert_eq!(style::strip(&colored), fields.text);
    }

    #[test]
    fn a_feature_lists_names_are_bold_and_its_details_muted() {
        let value = rows(
            r#"{"type":"FeatureCollection","attribution":"NOTICE: terms","features":[{"type":"Feature","geometry":{"coordinates":[24.941822,60.167507],"type":"Point"},"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}}]}"#,
        );
        let list = list_rendering(&value, Some("geocoder")).expect("renders");
        let colored = styled(&list, true);
        let (b, r) = (style::BOLD, style::RESET);
        let d = |text: &str| style::paint(text, &style::muted(), true);
        assert_eq!(
            colored,
            format!(
                "1. {b}Helsinki{r} {}\n   Helsinki, Uusimaa, Finland\n   {}\n\n{}",
                d("(place)"),
                d("24.941822,60.167507"),
                d("NOTICE: terms"),
            )
        );
        assert_eq!(style::strip(&colored), list.text);
        assert_eq!(styled(&list, false), list.text);
    }

    #[test]
    fn a_batchs_query_headers_are_bold_and_color_changes_no_text() {
        let value = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","attribution":"NOTICE: terms","features":[{"properties":{"name":"Helsinki"}}]},
                {"type":"FeatureCollection","attribution":"NOTICE: terms","features":[{"properties":{"name":"Tampere"}}]}
            ]}"#,
        );
        let list = list_rendering(&value, Some("geocoder")).expect("renders");
        let colored = styled(&list, true);
        assert!(
            colored.starts_with(&format!("{}Query 1:{}", style::BOLD, style::RESET)),
            "{colored:?}"
        );
        assert_eq!(style::strip(&colored), list.text);
    }

    #[test]
    fn an_array_of_like_objects_becomes_a_table() {
        let out = render_human(&rows(
            r#"[{"id":"a1","name":"First"},{"id":"b2","name":"Second"}]"#,
        ))
        .expect("a table")
        .text;

        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            ["ID  NAME", "a1  First", "b2  Second"]
        );
    }

    /// Every rule that keeps a column out, on one fixture: `usage` never
    /// varies, `modified` repeats `created`, and `token` is on one row in
    /// four.
    #[test]
    fn columns_that_say_nothing_are_left_out() {
        let mut fixture = Vec::new();
        for i in 0..4 {
            let token = if i == 0 { r#","token":"pk.x""# } else { "" };
            fixture.push(format!(
                r#"{{"id":"r{i}","created":"2026-01-0{i}T00:00:00Z","modified":"2026-01-0{i}T00:00:00Z","usage":"sk"{token}}}"#
            ));
        }
        let out = render_human(&rows(&format!("[{}]", fixture.join(","))))
            .expect("a table")
            .text;

        let header = out.lines().next().expect("a header");
        assert!(header.contains("ID"), "{header}");
        assert!(header.contains("CREATED"), "{header}");
        for absent in ["MODIFIED", "USAGE", "TOKEN"] {
            assert!(!header.contains(absent), "{absent} should be out: {header}");
        }
    }

    /// The identifier a table was keyed by, so the advice underneath names a
    /// real value instead of a placeholder the reader has to fill in.
    #[test]
    fn a_table_reports_the_identifier_it_showed() {
        let keyed = render_human(&rows(
            r#"[{"id":"a1","name":"First"},{"id":"b2","name":"S"}]"#,
        ))
        .expect("a table");
        assert_eq!(keyed.identifier.as_deref(), Some("a1"));

        let unkeyed =
            render_human(&rows(r#"[{"a":"1","b":"x"},{"a":"2","b":"y"}]"#)).expect("a table");
        assert_eq!(unkeyed.identifier, None);
    }

    #[test]
    fn an_identifier_keeps_its_width_while_the_rest_give_way() {
        let long = "c".repeat(25);
        let out = render_human(&rows(&format!(
            r#"[{{"id":"{long}","a":"{p}","b":"{p}","c":"{p}","d":"{q}"}},
                {{"id":"other","a":"x","b":"y","c":"z","d":"w"}}]"#,
            p = "p".repeat(40),
            q = "q".repeat(40)
        )))
        .expect("a table")
        .text;

        assert!(out.contains(&long), "the id was clipped: {out}");
        assert!(
            out.lines().all(|l| l.chars().count() <= LINE + 25),
            "line ran away: {out}"
        );
    }

    #[test]
    fn a_single_object_becomes_a_field_list_including_one_level_of_nesting() {
        let out = render_human(&rows(
            r#"{"code":"TokenValid","token":{"usage":"tk","scopes":["a","b"]}}"#,
        ))
        .expect("a field list")
        .text;

        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                "code          TokenValid",
                "token.scopes  a, b",
                "token.usage   tk"
            ]
        );
    }

    /// Several APIs wrap a listing in one key — `{"path":[…]}` from
    /// `get-breadcrumb`, `{"icons":[…]}` from `get-iconset`. The rows are
    /// what the caller asked for, so they get the table.
    #[test]
    fn a_listing_wrapped_in_one_key_still_becomes_a_table() {
        let out = render_human(&rows(
            r#"{"icons":[{"name":"tunnel","size":3},{"name":"bridge","size":4}]}"#,
        ))
        .expect("a table")
        .text;

        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            ["NAME    SIZE", "tunnel  3", "bridge  4"]
        );
    }

    /// Two keys are two listings, and showing one would hide the other.
    /// `styles list-files` returns `files` and `folders` together.
    #[test]
    fn two_wrapped_listings_are_left_to_json() {
        assert!(render_human(&rows(r#"{"files":[{"id":"a"}],"folders":[{"id":"b"}]}"#)).is_none());
    }

    /// The wrapper only applies when the rows are rows. A one-key object
    /// holding scalars is a record with one field, and the field list says
    /// so better than a failed table would.
    #[test]
    fn one_key_holding_scalars_stays_a_field_list() {
        let out = render_human(&rows(r#"{"scopes":["styles:read","fonts:read"]}"#))
            .expect("a field list")
            .text;

        assert_eq!(out.trim(), "scopes  styles:read, fonts:read");
    }

    /// A field list has room, so nothing is shortened there — the table is
    /// the only place a timestamp loses its time.
    #[test]
    fn only_the_table_shortens_a_timestamp() {
        let field = render_human(&rows(r#"{"expires":"2026-09-01T23:50:16.000Z"}"#))
            .unwrap()
            .text;
        assert!(field.contains("23:50:16"), "{field}");

        let table = render_human(&rows(
            r#"[{"id":"a","expires":"2026-09-01T23:50:16.000Z"},{"id":"b","expires":"2026-09-02T01:00:00.000Z"}]"#,
        ))
        .unwrap()
        .text;
        assert!(
            table.contains("2026-09-01") && !table.contains("23:50:16"),
            "{table}"
        );
    }

    /// Structure that a two-column view would throw away stays as JSON.
    #[test]
    fn shapes_a_table_would_misrepresent_are_left_alone() {
        assert!(render_human(&rows(
            r#"{"type":"FeatureCollection","features":[{"a":1}]}"#
        ))
        .is_none());
        assert!(render_human(&rows(r#"[1,2,3]"#)).is_none());
        assert!(render_human(&rows(r#""just a string""#)).is_none());
    }

    /// `render_human` itself never special-cases `search` — the test above
    /// still holds for a bare GeoJSON value. The carve-out lives one level
    /// up, in `search_feature_rows`, which only `emit_value` reaches for
    /// when the caller's `service` is `search`.
    #[test]
    fn search_extracts_one_row_per_feature_everything_else_does_not() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"properties":{"name":"Starbucks","feature_type":"poi","full_address":"1 Main St","distance":19568}},
                {"properties":{"name":"Peet's","feature_type":"poi","full_address":"2 Main St","distance":30021}}
            ]}"#,
        );

        let extracted = search_feature_rows(&fc).expect("a FeatureCollection extracts");
        let list = render_feature_list(&extracted).text;
        assert!(
            list.contains("1. Starbucks") && list.contains("2. Peet's"),
            "{list}"
        );

        // Not a FeatureCollection: nothing to extract, so the caller falls
        // back to rendering the original value untouched.
        assert!(search_feature_rows(&rows(r#"[{"name":"a"}]"#)).is_none());
        assert!(search_feature_rows(&rows(r#"{"scopes":["a"]}"#)).is_none());
    }

    /// A shape close enough to fool the `type` check but not the rest of it
    /// — one feature with no `properties` among otherwise-normal ones — has
    /// to fall back to the untouched value too, not panic partway through.
    /// `emit_value` has no other path once `search_feature_rows` says yes,
    /// so this is the one place that can catch a response `search` never
    /// actually sends but a spec change or a stub server might.
    #[test]
    fn a_feature_collection_search_cannot_parse_falls_back_whole() {
        let one_broken_feature = rows(
            r#"{"type":"FeatureCollection","features":[
                {"properties":{"name":"Starbucks"}},
                {"geometry":{"coordinates":[0,0]}}
            ]}"#,
        );
        assert!(search_feature_rows(&one_broken_feature).is_none());

        let features_not_an_array = rows(r#"{"type":"FeatureCollection","features":"nope"}"#);
        assert!(search_feature_rows(&features_not_an_array).is_none());

        let no_features_at_all = rows(r#"{"type":"FeatureCollection"}"#);
        assert!(search_feature_rows(&no_features_at_all).is_none());
    }

    /// An empty row is the same failure as a missing `properties` object one
    /// step later — the feature was there and nothing in it was recognized —
    /// and has to fall back the same way rather than print a numbered
    /// `(unnamed)` with nothing under it. `feature_collection_rows` and
    /// `tilequery_feature_rows` guard this; search was still checking only
    /// for `Null`.
    #[test]
    fn a_search_feature_with_nothing_recognizable_falls_back_to_json() {
        for properties in ["{}", r#"{"mapbox_id":"opaque-id-nobody-reads"}"#] {
            let fc = rows(&format!(
                r#"{{"type":"FeatureCollection","features":[{{"properties":{properties}}}]}}"#
            ));
            assert!(
                search_feature_rows(&fc).is_none(),
                "properties {properties} should fall back"
            );
        }

        // A position on its own is still something worth showing, so a
        // feature carrying one is not empty and still renders.
        let coordinates_only = rows(
            r#"{"type":"FeatureCollection","features":[{"geometry":{"coordinates":[0,0]},"properties":{}}]}"#,
        );
        assert_eq!(
            render_feature_list(&search_feature_rows(&coordinates_only).expect("a list")).text,
            "1. (unnamed)\n   0,0"
        );
    }

    /// `feature_row`'s rule, applied to the two fields `search_result_row`
    /// was still cloning in raw. A conforming search response always sends
    /// strings here, so this is defensive — but `render_feature_list` shows
    /// nothing at all for a non-string, which is the silent drop the rest of
    /// this work exists to close.
    #[test]
    fn a_search_row_drops_a_non_string_name_and_address() {
        let wrong_types = rows(
            r#"{"properties":{"name":42,"full_address":{"line1":"x"},"address":["y"],"place_formatted":7}}"#,
        );
        let row = search_result_row(&wrong_types);
        assert!(row.get("name").is_none(), "{row}");
        assert!(row.get("address").is_none(), "{row}");

        // The chain steps past a wrong-typed candidate rather than ending on
        // it, the same way it steps past an absent one.
        let null_full_address =
            rows(r#"{"properties":{"name":"a","full_address":null,"address":"15885 Dam Road"}}"#);
        assert_eq!(
            search_result_row(&null_full_address)["address"],
            "15885 Dam Road"
        );

        // And an empty `name` is absent, not a bare `1. ` with nothing after.
        let blank = rows(r#"{"properties":{"name":"","feature_type":"poi"}}"#);
        assert!(search_result_row(&blank).get("name").is_none());
    }

    /// `list-category`'s response is short flat objects, not POIs — a table
    /// suits it, and `uuid`/`version` are noise a table shouldn't spend
    /// columns on.
    #[test]
    fn list_category_extracts_table_rows_without_uuid_or_version() {
        let response = rows(
            r#"{"listItems":[
                {"canonical_id":"food_and_drink","icon":"fast-food","name":"Food and Drink","uuid":"71fed985-…","version":"25:6bd9…"},
                {"canonical_id":"lodging","icon":"lodging","name":"Lodging","uuid":"8de7b125-…","version":"25:6bd9…"}
            ],"attribution":"© 2026 Mapbox"}"#,
        );

        let extracted = search_category_rows(&response).expect("listItems extracts");
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted[0]["canonical_id"], "food_and_drink");
        assert_eq!(extracted[0]["name"], "Food and Drink");
        assert_eq!(extracted[0]["icon"], "fast-food");
        assert!(extracted[0].get("uuid").is_none());
        assert!(extracted[0].get("version").is_none());

        let table = render_table(&extracted).expect("a table").text;
        assert!(table.contains("CANONICAL_ID"), "{table}");
        assert!(
            table.contains("food_and_drink") && table.contains("Lodging"),
            "{table}"
        );

        // No `listItems` at all: nothing to extract, caller falls back.
        assert!(search_category_rows(&rows(r#"{"attribution":"x"}"#)).is_none());
        assert!(search_category_rows(&rows(r#"{"listItems":"nope"}"#)).is_none());
        assert!(search_category_rows(&rows(r#"{"listItems":[1,2,3]}"#)).is_none());
    }

    /// End to end through `emit_value`'s own branching, not just against the
    /// extraction helper: a `search` response with no `type`/`features` but
    /// a `listItems` array renders as the category table, and a shape that
    /// is neither still falls back to `render_human`.
    #[test]
    fn search_renders_a_category_list_as_a_table_not_a_feature_collection() {
        let response = rows(
            r#"{"listItems":[{"canonical_id":"food_and_drink","icon":"fast-food","name":"Food and Drink"}],"attribution":"© 2026 Mapbox"}"#,
        );
        assert!(search_feature_rows(&response).is_none());
        let extracted = search_category_rows(&response).expect("listItems extracts");
        let table = render_table(&extracted).expect("a table").text;
        assert!(table.contains("food_and_drink"), "{table}");

        // Neither shape: falls through to the generic renderer, same as any
        // other service's response would.
        assert!(search_category_rows(&rows(r#"{"scopes":["a"]}"#)).is_none());
    }

    /// The format the readability complaint against the table asked for:
    /// name and distance on one line, the whole address on the next, never
    /// clipped — checked end to end from a raw `FeatureCollection` through
    /// both rendering stages, not just against a hand-built row.
    #[test]
    fn a_search_list_never_clips_an_address() {
        let long_address =
            "15885 Very Long Street Name That Would Never Fit In A Table Column, Clearlake, California 95422, United States";
        let fc = rows(&format!(
            r#"{{"type":"FeatureCollection","features":[
                {{"properties":{{"name":"Starbucks","full_address":"{long_address}","distance":19568}}}}
            ]}}"#
        ));

        let extracted = search_feature_rows(&fc).unwrap();
        let list = render_feature_list(&extracted).text;

        assert_eq!(list, format!("1. Starbucks — 19.6 km\n   {long_address}"));
    }

    /// Pins docs/commands.md's three `search` Outputs examples to what the
    /// code actually renders for the exact JSON each example's own `-o json`
    /// column shows — a doc example missing the coordinates line (once true
    /// of `category`'s, caught only by hand) fails here mechanically instead
    /// of at the next reader who tries the command and gets a fourth line.
    #[test]
    fn search_outputs_examples_in_the_docs_match_the_real_render() {
        let forward = rows(
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-122.059627,37.56153],"type":"Point"},"properties":{"name":"34170 Gannon Terrace","mapbox_id":"{mapbox_id}","feature_type":"address","full_address":"34170 Gannon Terrace, Fremont, California 94555, United States","distance":20045}}]}"#,
        );
        assert_eq!(
            render_feature_list(&search_feature_rows(&forward).unwrap()).text,
            "1. 34170 Gannon Terrace (address) — 20.0 km\n   34170 Gannon Terrace, Fremont, California 94555, United States\n   -122.059627,37.56153"
        );

        let reverse = rows(
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-118.471584,34.023345],"type":"Point"},"properties":{"name":"1827 21st Street","feature_type":"address","full_address":"1827 21st Street, Santa Monica, California 90404, United States"}}]}"#,
        );
        assert_eq!(
            render_feature_list(&search_feature_rows(&reverse).unwrap()).text,
            "1. 1827 21st Street (address)\n   1827 21st Street, Santa Monica, California 90404, United States\n   -118.471584,34.023345"
        );

        let category = rows(
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-122.6180785,38.9307594],"type":"Point"},"properties":{"name":"Starbucks","feature_type":"poi","brand":["Starbucks"],"poi_category":["café","coffee","coffee shop"],"full_address":"15885 Dam Road, Clearlake, California 95422, United States","distance":19568}}]}"#,
        );
        assert_eq!(
            render_feature_list(&search_feature_rows(&category).unwrap()).text,
            "1. Starbucks (café, coffee, coffee shop) — 19.6 km\n   15885 Dam Road, Clearlake, California 95422, United States\n   -122.6180785,38.9307594"
        );
    }

    /// The coordinates are the reason most callers run a search command at
    /// all — the next step with a result is almost always feeding its
    /// location somewhere else — so they're on the list even though nothing
    /// else here comes from `geometry` rather than `properties`.
    /// `longitude,latitude`, matching the order every coordinate parameter
    /// in this service already takes (`--proximity`, `--longitude`
    /// `--latitude`), not `properties.coordinates`'s `latitude` first.
    #[test]
    fn a_search_row_reads_coordinates_from_the_geometry_not_properties() {
        let feature = rows(
            r#"{
                "geometry": {"type": "Point", "coordinates": [-122.059627, 37.56153]},
                "properties": {
                    "name": "a",
                    "coordinates": {"longitude": 37.56153, "latitude": -122.059627}
                }
            }"#,
        );
        assert_eq!(
            search_result_row(&feature)["coordinates"],
            "-122.059627,37.56153"
        );

        // No geometry at all: nothing to show, not a made-up pair.
        let no_geometry = rows(r#"{"properties":{"name":"a"}}"#);
        assert!(search_result_row(&no_geometry).get("coordinates").is_none());
    }

    #[test]
    fn a_search_list_shows_coordinates_on_their_own_line() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[-122.059627,37.56153]},"properties":{"name":"a","full_address":"1 Main St"}}
            ]}"#,
        );
        let extracted = search_feature_rows(&fc).unwrap();
        let list = render_feature_list(&extracted).text;
        assert_eq!(list, "1. a\n   1 Main St\n   -122.059627,37.56153");
    }

    /// `render_search_list` was a byte-for-byte copy of `render_feature_list`
    /// minus the `extra` lines — which a search row never carries — so the
    /// two were merged. This pins that the survivor still renders a search
    /// result exactly as the deleted one did, on a row filling every field a
    /// search result can: name, category, distance, address and coordinates.
    #[test]
    fn a_search_row_renders_through_the_shared_list_and_carries_no_extra() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[{"geometry":{"coordinates":[-122.6180785,38.9307594]},"properties":{"name":"Starbucks","poi_category":["café","coffee"],"full_address":"15885 Dam Road, Clearlake, California 95422, United States","distance":19568}}]}"#,
        );

        let extracted = search_feature_rows(&fc).expect("a FeatureCollection extracts");
        assert!(
            extracted[0].get("extra").is_none(),
            "the `extra` path is inert for search: {}",
            extracted[0]
        );
        assert_eq!(
            render_feature_list(&extracted).text,
            "1. Starbucks (café, coffee) — 19.6 km\n   15885 Dam Road, Clearlake, \
             California 95422, United States\n   -122.6180785,38.9307594"
        );
    }

    /// One coordinate reader for all three row-builders, so a position reads
    /// the same way whichever service asked for it.
    ///
    /// `search`'s own copy filtered on `len() == 2` and so dropped the line
    /// entirely — the field its own comment calls the reason most callers run
    /// the command — for a position carrying altitude. Sharing the reader
    /// gives it the `>= 2` the other two already had.
    #[test]
    fn every_row_builder_reads_a_three_element_position_the_same_way() {
        let feature = rows(
            r#"{"geometry":{"coordinates":[-122.059627,37.56153,12.5]},
                "properties":{"name":"a","type":"b"}}"#,
        );

        for row in [
            search_result_row(&feature),
            feature_row(&feature),
            tilequery_row(&feature),
        ] {
            assert_eq!(row["coordinates"], "-122.059627,37.56153", "{row}");
        }

        // And nothing invented where there is no usable position to read.
        assert!(coordinate_cell(&rows(r#"{"properties":{"name":"a"}}"#)).is_none());
        assert!(coordinate_cell(&rows(r#"{"geometry":{"coordinates":[1]}}"#)).is_none());
    }

    /// The columns are hand-picked, not `columns_for`'s usual majority-scalar
    /// rule — this pins which ones survive and which don't, since nothing
    /// else in the type system says so.
    #[test]
    fn a_search_row_keeps_name_one_address_and_a_km_distance_drops_the_rest() {
        let feature = rows(
            r#"{"properties":{
                "name":"Starbucks",
                "mapbox_id":"opaque-id-nobody-reads",
                "feature_type":"poi",
                "maki":"cafe",
                "language":"en",
                "address":"15885 Dam Road",
                "full_address":"15885 Dam Road, Clearlake, California 95422, United States",
                "poi_category":["cafe","coffee"],
                "distance":19568
            }}"#,
        );
        let row = search_result_row(&feature);

        assert_eq!(row["name"], "Starbucks");
        // `full_address` wins over the bare `address` it contains.
        assert_eq!(
            row["address"],
            "15885 Dam Road, Clearlake, California 95422, United States"
        );
        assert_eq!(row["distance"], "19.6 km");
        // `poi_category`'s entries, joined — not `feature_type`, which only
        // stands in when a result has no category at all.
        assert_eq!(row["category"], "cafe, coffee");
        for dropped in ["mapbox_id", "feature_type", "maki", "language"] {
            assert!(row.get(dropped).is_none(), "kept {dropped}: {row}");
        }
    }

    /// `full_address` is the common case; `place_formatted` and bare
    /// `address` are what a result missing it might have instead.
    #[test]
    fn a_search_row_falls_back_through_the_address_fields_it_has() {
        let with_place = rows(r#"{"properties":{"name":"a","place_formatted":"Clearlake, CA"}}"#);
        assert_eq!(search_result_row(&with_place)["address"], "Clearlake, CA");

        let with_address_only = rows(r#"{"properties":{"name":"a","address":"15885 Dam Road"}}"#);
        assert_eq!(
            search_result_row(&with_address_only)["address"],
            "15885 Dam Road"
        );

        // Both stand-ins present and no `full_address`: the street line wins,
        // being the half that says which building rather than which city.
        let with_both = rows(
            r#"{"properties":{"name":"a","address":"15885 Dam Road","place_formatted":"Clearlake, CA"}}"#,
        );
        assert_eq!(search_result_row(&with_both)["address"], "15885 Dam Road");

        // `full_address` outranks both, since it is already the two joined.
        let with_all = rows(
            r#"{"properties":{"name":"a","full_address":"15885 Dam Road, Clearlake, CA","address":"15885 Dam Road","place_formatted":"Clearlake, CA"}}"#,
        );
        assert_eq!(
            search_result_row(&with_all)["address"],
            "15885 Dam Road, Clearlake, CA"
        );

        let with_none = rows(r#"{"properties":{"name":"a"}}"#);
        assert!(search_result_row(&with_none).get("address").is_none());
    }

    /// `feature_type` only stands in for `category` when there is no
    /// `poi_category` — an address result, say, which has neither.
    #[test]
    fn a_search_row_falls_back_to_feature_type_with_no_poi_category() {
        let no_category = rows(r#"{"properties":{"name":"a","feature_type":"address"}}"#);
        assert_eq!(search_result_row(&no_category)["category"], "address");

        let empty_category =
            rows(r#"{"properties":{"name":"a","feature_type":"place","poi_category":[]}}"#);
        assert_eq!(search_result_row(&empty_category)["category"], "place");

        let neither = rows(r#"{"properties":{"name":"a"}}"#);
        assert!(search_result_row(&neither).get("category").is_none());
    }

    #[test]
    fn geocoder_extracts_one_row_per_feature_everything_else_does_not() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}},
                {"properties":{"name":"Tampere","feature_type":"place","full_address":"Tampere, Pirkanmaa, Finland"}}
            ]}"#,
        );

        let extracted = feature_collection_rows(&fc).expect("a FeatureCollection extracts");
        let list = render_feature_list(&extracted).text;
        assert!(
            list.contains("1. Helsinki") && list.contains("2. Tampere"),
            "{list}"
        );

        assert!(feature_collection_rows(&rows(r#"[{"name":"a"}]"#)).is_none());
        assert!(feature_collection_rows(&rows(r#"{"scopes":["a"]}"#)).is_none());
    }

    /// A shape close enough to fool the `type` check but not the rest of it
    /// must fall back whole, not panic partway through.
    #[test]
    fn a_feature_collection_that_cannot_parse_falls_back_whole() {
        let one_broken_feature = rows(
            r#"{"type":"FeatureCollection","features":[
                {"properties":{"name":"Helsinki"}},
                {"geometry":{"coordinates":[0,0]}}
            ]}"#,
        );
        assert!(feature_collection_rows(&one_broken_feature).is_none());

        let features_not_an_array = rows(r#"{"type":"FeatureCollection","features":"nope"}"#);
        assert!(feature_collection_rows(&features_not_an_array).is_none());

        let no_features_at_all = rows(r#"{"type":"FeatureCollection"}"#);
        assert!(feature_collection_rows(&no_features_at_all).is_none());
    }

    /// An empty row is the same failure as a missing `properties` object, one
    /// step later: the feature was there and nothing in it was recognized.
    /// Both have to fall the whole collection back to JSON — a numbered
    /// `(unnamed)` with no lines under it reads as a result that is genuinely
    /// blank rather than as a shape this renderer does not understand.
    #[test]
    fn a_feature_with_no_recognized_properties_falls_back_to_json() {
        for properties in ["{}", r#"{"mapbox_id":"opaque-id-nobody-reads"}"#] {
            let fc = rows(&format!(
                r#"{{"type":"FeatureCollection","features":[{{"properties":{properties}}}]}}"#
            ));
            assert!(
                feature_collection_rows(&fc).is_none(),
                "properties {properties} should fall back"
            );
        }

        // Tilequery is mostly shielded from this by `extra`, but a feature
        // whose properties are all nested still lands there.
        let queried = rows(
            r#"{"type":"FeatureCollection","features":[{"properties":{"context":{"tile":{"z":16}}}}]}"#,
        );
        assert!(tilequery_feature_rows(&queried).is_none());
    }

    /// No results is an answer, not a failure — `(none)` says so, where a
    /// fallback to `{"features": []}` would make the reader parse JSON to
    /// learn nothing was found.
    #[test]
    fn an_empty_feature_collection_renders_as_none_for_both_services() {
        let empty = rows(r#"{"type":"FeatureCollection","features":[]}"#);

        for rendered in [
            list_rendering(&empty, Some("geocoder")),
            list_rendering(&empty, Some("tilesets")),
        ] {
            assert_eq!(rendered.expect("a list").text, "(none)");
        }
    }

    /// The list is three services' exception, not a shape-sniffing rule:
    /// every other service's GeoJSON still has to reach pretty JSON, which is
    /// what the `None`s here mean.
    #[test]
    fn only_the_three_listed_services_turn_a_feature_collection_into_a_list() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[24.94,60.16]},"properties":{"name":"Helsinki","feature_type":"place"}}
            ]}"#,
        );

        for service in [Some("maps"), Some("styles"), Some("geocoding"), None] {
            assert!(
                list_rendering(&fc, service).is_none(),
                "{service:?} should not get a list"
            );
        }
        assert!(render_human(&fc).is_none(), "and nothing else renders it");

        for service in LISTED_SERVICES {
            assert!(
                list_rendering(&fc, Some(service)).is_some(),
                "{service} should get a list"
            );
        }
    }

    /// Every service `list_rendering` matches, by name.
    const LISTED_SERVICES: [&str; 3] = ["search", "geocoder", "tilesets"];

    /// The match is on a string, so a command group renamed or merged in the
    /// specs leaves its arm unreachable without a warning — the tests above
    /// pass that string straight in and keep passing. #116 moved
    /// `tilequery get` to `tilesets query` exactly that way. Checking each
    /// name against the bundled specs is what ties the arm to a real command.
    #[test]
    fn every_listed_service_is_a_real_command_group() {
        let services: Vec<String> = crate::spec::effective_services()
            .expect("the bundled specs parse")
            .into_iter()
            .map(|service| service.name)
            .collect();
        for listed in LISTED_SERVICES {
            assert!(
                services.iter().any(|name| name == listed),
                "`{listed}` has a list rendering but no command group by that name: {services:?}"
            );
        }
    }

    #[test]
    fn a_feature_list_never_clips_an_address() {
        let long_address =
            "1600 Pennsylvania Avenue Northwest, Washington, District of Columbia 20500, United States";
        let fc = rows(&format!(
            r#"{{"type":"FeatureCollection","features":[
                {{"properties":{{"name":"The White House","feature_type":"poi","full_address":"{long_address}"}}}}
            ]}}"#
        ));

        let extracted = feature_collection_rows(&fc).unwrap();
        let list = render_feature_list(&extracted).text;

        assert_eq!(list, format!("1. The White House (poi)\n   {long_address}"));
    }

    /// Pins docs/commands.md's `forward-geocode` example to the real render,
    /// attribution line and all.
    #[test]
    fn geocoder_outputs_example_in_the_docs_matches_the_real_render() {
        let forward = rows(
            r#"{"type":"FeatureCollection","attribution":"NOTICE: © 2026 Mapbox and its suppliers. All rights reserved. This response and the information it contains may not be retained.","features":[{"type":"Feature","geometry":{"coordinates":[24.941822,60.167507],"type":"Point"},"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}}]}"#,
        );
        assert_eq!(
            render_geocoder_list(&forward).expect("a list").text,
            "1. Helsinki (place)\n   Helsinki, Uusimaa, Finland\n   24.941822,60.167507\n\n\
             NOTICE: © 2026 Mapbox and its suppliers. All rights reserved. This response and \
             the information it contains may not be retained."
        );
    }

    /// The terms the results come under are part of the answer. Unlike the
    /// `batch` guard, an unexpected key here is carried, not a reason to bail.
    #[test]
    fn attribution_follows_the_list_when_the_response_carries_one() {
        let notice = "NOTICE: © 2026 Mapbox and its suppliers. All rights reserved.";
        let with = rows(&format!(
            r#"{{"type":"FeatureCollection","attribution":"{notice}",
                "features":[{{"properties":{{"name":"Helsinki","feature_type":"place"}}}}]}}"#
        ));
        assert_eq!(
            render_geocoder_list(&with).expect("a list").text,
            format!("1. Helsinki (place)\n\n{notice}")
        );

        let without = rows(
            r#"{"type":"FeatureCollection","features":[{"properties":{"name":"Helsinki","feature_type":"place"}}]}"#,
        );
        assert_eq!(
            render_geocoder_list(&without).expect("a list").text,
            "1. Helsinki (place)"
        );

        // A blank one says nothing and would just add two empty lines.
        let blank = rows(
            r#"{"type":"FeatureCollection","attribution":"  ","features":[{"properties":{"name":"Helsinki"}}]}"#,
        );
        assert_eq!(
            render_geocoder_list(&blank).expect("a list").text,
            "1. Helsinki"
        );
    }

    /// Geocoding v6 requires `attribution` on every entry of a batch, and it
    /// is the same notice each time — fifty copies of it would bury the fifty
    /// results. One copy, under the whole batch. Two that genuinely differ
    /// both survive, since neither can be assumed to cover the other.
    #[test]
    fn a_batchs_repeated_attribution_is_printed_once_under_the_whole_batch() {
        let repeated = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","attribution":"NOTICE: terms","features":[{"properties":{"name":"Helsinki"}}]},
                {"type":"FeatureCollection","attribution":"NOTICE: terms","features":[{"properties":{"name":"Tampere"}}]}
            ]}"#,
        );
        assert_eq!(
            render_batch_feature_list(&repeated).expect("a batch").text,
            "Query 1:\n1. Helsinki\n\nQuery 2:\n1. Tampere\n\nNOTICE: terms"
        );

        let differing = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","attribution":"NOTICE: first","features":[{"properties":{"name":"Helsinki"}}]},
                {"type":"FeatureCollection","attribution":"NOTICE: second","features":[{"properties":{"name":"Tampere"}}]}
            ]}"#,
        );
        assert_eq!(
            render_batch_feature_list(&differing).expect("a batch").text,
            "Query 1:\n1. Helsinki\n\nQuery 2:\n1. Tampere\n\nNOTICE: first\n\nNOTICE: second"
        );
    }

    /// `longitude,latitude`, from the geometry, not `properties.coordinates`
    /// (`latitude` first there).
    #[test]
    fn a_feature_row_reads_coordinates_from_the_geometry_not_properties() {
        let feature = rows(
            r#"{
                "geometry": {"type": "Point", "coordinates": [24.941822, 60.167507]},
                "properties": {
                    "name": "a",
                    "coordinates": {"longitude": 60.167507, "latitude": 24.941822}
                }
            }"#,
        );
        assert_eq!(feature_row(&feature)["coordinates"], "24.941822,60.167507");

        let no_geometry = rows(r#"{"properties":{"name":"a"}}"#);
        assert!(feature_row(&no_geometry).get("coordinates").is_none());
    }

    /// A tileset may carry a top-level attribute with the same name as one of
    /// `tilequery`'s own — `geometry` is well within `mapbox-streets-v8`
    /// range. Before the dotted keys, the nested one overwrote the top-level
    /// one on its way into `extra`, and neither the value nor the fact that
    /// anything had been lost showed up anywhere.
    #[test]
    fn a_tilequery_key_does_not_overwrite_a_top_level_one_of_the_same_name() {
        let feature = rows(
            r#"{"geometry":{"coordinates":[0,0]},"properties":{
                "type":"building",
                "geometry":"multipolygon",
                "zoom":14,
                "tilequery":{"layer":"building","distance":0,"geometry":"polygon","zoom":16}
            }}"#,
        );

        let row = tilequery_row(&feature);
        assert_eq!(row["extra"]["geometry"], "multipolygon");
        assert_eq!(row["extra"]["tilequery.geometry"], "polygon");
        assert_eq!(row["extra"]["zoom"], 14);
        assert_eq!(row["extra"]["tilequery.zoom"], 16);
        assert_eq!(
            render_feature_list(&[row]).text,
            "1. building (building) — 0.0 m\n   0,0\n   geometry: multipolygon\n   \
             tilequery.geometry: polygon\n   tilequery.zoom: 16\n   zoom: 14"
        );
    }

    /// A GeoJSON position may legally carry altitude as a third element.
    /// Reading only the first two is right; refusing the whole line because
    /// there is a third is not — that dropped the coordinates entirely.
    #[test]
    fn a_three_element_position_still_produces_a_coordinate_line() {
        let geocoded = rows(
            r#"{"geometry":{"coordinates":[24.941822,60.167507,12.5]},"properties":{"name":"a"}}"#,
        );
        assert_eq!(feature_row(&geocoded)["coordinates"], "24.941822,60.167507");

        let queried = rows(
            r#"{"geometry":{"coordinates":[24.9414,60.1699,3]},"properties":{"type":"building"}}"#,
        );
        assert_eq!(tilequery_row(&queried)["coordinates"], "24.9414,60.1699");
    }

    #[test]
    fn a_feature_list_shows_coordinates_on_their_own_line() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[24.941822,60.167507]},"properties":{"name":"a","full_address":"Helsinki, Uusimaa, Finland"}}
            ]}"#,
        );
        let extracted = feature_collection_rows(&fc).unwrap();
        let list = render_feature_list(&extracted).text;
        assert_eq!(
            list,
            "1. a\n   Helsinki, Uusimaa, Finland\n   24.941822,60.167507"
        );
    }

    #[test]
    fn a_feature_row_keeps_name_one_address_and_a_category_drops_the_rest() {
        let feature = rows(
            r#"{"properties":{
                "name":"Helsinki",
                "mapbox_id":"opaque-id-nobody-reads",
                "feature_type":"place",
                "name_preferred":"Helsinki",
                "full_address":"Helsinki, Uusimaa, Finland",
                "place_formatted":"Uusimaa, Finland",
                "context":{"country":{"name":"Finland"}}
            }}"#,
        );

        let row = feature_row(&feature);
        assert_eq!(
            row,
            rows(
                r#"{"name":"Helsinki","address":"Helsinki, Uusimaa, Finland","category":"place"}"#
            )
        );
    }

    /// An explicit JSON `null` for `full_address` must fall through to
    /// `place_formatted` the same way an absent key does.
    #[test]
    fn a_null_full_address_falls_through_to_place_formatted() {
        let feature = rows(
            r#"{"properties":{
                "name":"Uusimaa",
                "feature_type":"region",
                "full_address":null,
                "place_formatted":"Finland"
            }}"#,
        );

        assert_eq!(feature_row(&feature)["address"], "Finland");
    }

    /// `tilequery_row`'s rule, applied to the two fields `feature_row` was
    /// still cloning in raw: a wrong-typed value is dropped, never carried
    /// into the row for `render_feature_list` to swallow without a word.
    #[test]
    fn a_feature_row_drops_a_non_string_name_and_address() {
        let wrong_types = rows(
            r#"{"properties":{"name":42,"full_address":{"line1":"x"},"place_formatted":["y"]}}"#,
        );
        let row = feature_row(&wrong_types);
        assert!(row.get("name").is_none(), "{row}");
        assert!(row.get("address").is_none(), "{row}");

        // And a wrong-typed `full_address` falls through to `place_formatted`
        // the same way an explicit null does.
        let one_usable = rows(r#"{"properties":{"full_address":42,"place_formatted":"Finland"}}"#);
        assert_eq!(feature_row(&one_usable)["address"], "Finland");

        // An empty `name` is absent, the same as in `tilequery_row` — kept it
        // would render as `1. ` with a trailing space.
        let blank_name = rows(r#"{"properties":{"name":"","feature_type":"place"}}"#);
        let row = feature_row(&blank_name);
        assert!(row.get("name").is_none(), "{row}");
        assert_eq!(render_feature_list(&[row]).text, "1. (unnamed) (place)");
    }

    #[test]
    fn a_batch_renders_each_querys_list_under_its_own_numbered_header() {
        let batch = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","features":[{"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}}]},
                {"type":"FeatureCollection","features":[{"properties":{"name":"Tampere","feature_type":"place","full_address":"Tampere, Pirkanmaa, Finland"}}]}
            ]}"#,
        );

        assert_eq!(
            render_batch_feature_list(&batch)
                .expect("a batch renders")
                .text,
            "Query 1:\n1. Helsinki (place)\n   Helsinki, Uusimaa, Finland\n\n\
             Query 2:\n1. Tampere (place)\n   Tampere, Pirkanmaa, Finland"
        );
    }

    /// `Query 1:` over the only query on screen names something the reader
    /// has nothing to tell apart from. Two or more keep their headers —
    /// `a_batch_renders_each_querys_list_under_its_own_numbered_header`
    /// pins that side.
    #[test]
    fn a_single_query_batch_renders_without_a_header() {
        let one = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","features":[{"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}}]}
            ]}"#,
        );

        assert_eq!(
            render_batch_feature_list(&one).expect("a batch").text,
            "1. Helsinki (place)\n   Helsinki, Uusimaa, Finland"
        );
    }

    #[test]
    fn a_batch_with_one_malformed_query_falls_back_whole() {
        let batch = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","features":[{"properties":{"name":"Helsinki"}}]},
                {"type":"FeatureCollection","features":[{"geometry":{"coordinates":[0,0]}}]}
            ]}"#,
        );
        assert!(render_batch_feature_list(&batch).is_none());

        assert!(render_batch_feature_list(&rows(r#"{"batch":"nope"}"#)).is_none());
        assert!(render_batch_feature_list(&rows(r#"[1,2,3]"#)).is_none());
        // A second key alongside `batch` would be lost by rendering `batch`
        // alone, so this must fall back too, not just anything with a
        // `batch` array.
        assert!(render_batch_feature_list(&rows(
            r#"{"batch":[{"type":"FeatureCollection","features":[]}],"other":1}"#
        ))
        .is_none());
    }

    /// A `building` feature has no `name` property at all — falls through
    /// to `type`, `layer` and `distance` as before, with the tile attributes
    /// that have no named slot (`height`, and `tilequery.geometry`) carried
    /// under `extra` instead of dropped.
    #[test]
    fn tilequery_row_falls_back_to_type_layer_and_distance_without_a_name() {
        let feature = rows(
            r#"{
                "geometry": {"type": "Point", "coordinates": [24.9414, 60.1699]},
                "properties": {
                    "type": "building:part",
                    "height": 9.3,
                    "tilequery": {"layer": "building", "distance": 0.855, "geometry": "polygon"}
                }
            }"#,
        );

        assert_eq!(
            tilequery_row(&feature),
            rows(
                r#"{"name":"building:part","coordinates":"24.9414,60.1699","category":"building","distance":"0.9 m","extra":{"height":9.3,"tilequery.geometry":"polygon"}}"#
            )
        );

        assert!(tilequery_row(&rows(r#"{"geometry":{}}"#))
            .as_object()
            .is_none());
    }

    /// A raster-array tileset sends no `type` to stand in for a name and no
    /// `distance`, and puts the answer the caller asked for in `val` — one
    /// number per band, so a *list*, which a scalars-only rule would have
    /// dropped along with the point of the query. The fixture is the
    /// tilequery spec's own `rasterarray` response example.
    #[test]
    fn a_raster_array_tilequery_row_keeps_its_value_band_zoom_and_units() {
        let feature = rows(
            r#"{
                "type": "Feature",
                "id": null,
                "geometry": {"type": "Point", "coordinates": [-122.459, 37.7754]},
                "properties": {
                    "val": [1.23],
                    "tilequery": {"layer": "data", "band": "frame-0", "zoom": 6, "units": "m"}
                }
            }"#,
        );

        assert_eq!(
            tilequery_row(&feature),
            rows(
                r#"{"coordinates":"-122.459,37.7754","category":"data",
                    "extra":{"tilequery.band":"frame-0","tilequery.units":"m",
                             "tilequery.zoom":6,"val":[1.23]}}"#
            )
        );

        // Several bands, several numbers, all of them on the one line.
        let three_bands =
            rows(r#"{"properties":{"val":[1.23,4.5,6.78],"tilequery":{"layer":"data"}}}"#);
        assert_eq!(
            render_feature_list(&[tilequery_row(&three_bands)]).text,
            "1. (unnamed) (data)\n   val: 1.23, 4.5, 6.78"
        );
    }

    /// An object, or a list of them, has no honest `key: value` line and is
    /// skipped rather than stringified — but skipping it must not take the
    /// values beside it, scalar or flat list, with it.
    #[test]
    fn tilequery_extras_keep_scalars_and_flat_lists_and_skip_what_is_deeper() {
        let feature = rows(
            r#"{"geometry":{"coordinates":[0,0]},"properties":{
                "type":"building",
                "class":"residential",
                "underground":false,
                "bands":["a","b"],
                "context":{"tile":{"z":16}},
                "outline":[[0,0],[1,1]],
                "tilequery":{"layer":"building","distance":0,"geometry":"polygon"}
            }}"#,
        );

        let row = tilequery_row(&feature);
        let extra = row["extra"].as_object().expect("extras");
        assert_eq!(
            extra.keys().collect::<Vec<_>>(),
            ["bands", "class", "tilequery.geometry", "underground"]
        );
        assert_eq!(
            render_feature_list(&[row]).text,
            "1. building (building) — 0.0 m\n   0,0\n   bands: a, b\n   class: residential\n   \
             tilequery.geometry: polygon\n   underground: no"
        );
    }

    /// Pins docs/commands.md's two `tilesets query` examples to the real
    /// render — the vector one, whose `height` has to reach the text column
    /// now that it reaches the JSON one, and the raster-array one, which has
    /// no name to show and carries its sample under `val`.
    #[test]
    fn tilequery_outputs_examples_in_the_docs_match_the_real_render() {
        let vector = rows(
            r#"{"features":[{"geometry":{"coordinates":[24.94,60.16],"type":"Point"},"properties":{"height":6.2,"tilequery":{"distance":0,"layer":"building"},"type":"building"},"type":"Feature"}],"type":"FeatureCollection"}"#,
        );
        assert_eq!(
            render_feature_list(&tilequery_feature_rows(&vector).expect("a list")).text,
            "1. building (building) — 0.0 m\n   24.94,60.16\n   height: 6.2"
        );

        let raster_array = rows(
            r#"{"features":[{"geometry":{"coordinates":[-122.459,37.7754],"type":"Point"},"id":null,"properties":{"tilequery":{"band":"frame-0","layer":"data","units":"m","zoom":6},"val":[1.23]},"type":"Feature"}],"type":"FeatureCollection"}"#,
        );
        assert_eq!(
            render_feature_list(&tilequery_feature_rows(&raster_array).expect("a list")).text,
            "1. (unnamed) (data)\n   -122.459,37.7754\n   tilequery.band: frame-0\n   \
             tilequery.units: m\n   tilequery.zoom: 6\n   val: 1.23"
        );
    }

    /// A `poi_label` feature carries both `name` (the place) and `type`
    /// (its category) — `name` wins the label, matching what a human expects
    /// to read, and `type` then has to survive as an attribute. Holding it
    /// back because it *might* have been the label lost "Restaurant"
    /// entirely, which is half of what a POI result says.
    ///
    /// Asserted as a whole row, not just `name`: checking one field is what
    /// let that loss through in the first place.
    #[test]
    fn tilequery_row_prefers_name_over_type_and_keeps_type_as_an_attribute() {
        let feature = rows(
            r#"{
                "geometry": {"coordinates": [24.9414, 60.1699]},
                "properties": {
                    "name": "Bangkok9",
                    "type": "Restaurant",
                    "tilequery": {"layer": "poi_label", "distance": 4.65, "geometry": "point"}
                }
            }"#,
        );

        let row = tilequery_row(&feature);
        assert_eq!(
            row,
            rows(
                r#"{"name":"Bangkok9","coordinates":"24.9414,60.1699","category":"poi_label",
                    "distance":"4.7 m",
                    "extra":{"tilequery.geometry":"point","type":"Restaurant"}}"#
            )
        );
        assert_eq!(row["extra"]["type"], "Restaurant");
    }

    /// An empty `name` string is treated as absent, not as a blank line —
    /// and not re-emitted under `extra` either, where it would render as a
    /// bare `name:` with nothing after it. It was read, and it said nothing.
    #[test]
    fn tilequery_row_falls_back_to_type_when_name_is_an_empty_string() {
        let feature =
            rows(r#"{"geometry":{"coordinates":[0,0]},"properties":{"name":"","type":"suburb"}}"#);
        assert_eq!(
            tilequery_row(&feature),
            rows(r#"{"name":"suburb","coordinates":"0,0"}"#)
        );
    }

    /// Not every tileset is `mapbox-streets-v8`; a `type` attribute that
    /// isn't a string must never become the `name` for `render_feature_list`
    /// to choke on with `unwrap_or("(unnamed)")` silently hiding it. It is
    /// still an attribute the tileset sent, though, so it stays under
    /// `extra` rather than vanishing.
    #[test]
    fn tilequery_row_keeps_a_non_string_type_as_an_attribute_never_as_the_name() {
        let feature = rows(r#"{"geometry":{"coordinates":[0,0]},"properties":{"type":42}}"#);
        assert_eq!(
            tilequery_row(&feature),
            rows(r#"{"coordinates":"0,0","extra":{"type":42}}"#)
        );
    }

    /// Same for `name`: a non-string `name` never becomes the label and
    /// `type` stands in, while the value itself stays visible under `extra`.
    ///
    /// Asserted as a whole row, so that what becomes of the value it could
    /// not use is pinned rather than left to chance.
    #[test]
    fn tilequery_row_drops_a_non_string_name_and_falls_back_to_type() {
        let feature =
            rows(r#"{"geometry":{"coordinates":[0,0]},"properties":{"name":42,"type":"road"}}"#);
        assert_eq!(
            tilequery_row(&feature),
            rows(r#"{"name":"road","coordinates":"0,0","extra":{"name":42}}"#)
        );
    }

    #[test]
    fn tilequery_extracts_one_row_per_feature_with_distance_on_the_first_line() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[24.9414,60.1699]},"properties":{"type":"retail","tilequery":{"layer":"building","distance":0}}}
            ]}"#,
        );
        let extracted = tilequery_feature_rows(&fc).expect("a FeatureCollection extracts");
        assert_eq!(
            render_feature_list(&extracted).text,
            "1. retail (building) — 0.0 m\n   24.9414,60.1699"
        );

        assert!(tilequery_feature_rows(&rows(r#"[{"type":"retail"}]"#)).is_none());
    }

    /// `render_table` alone would suggest `--id <name>` here, the same as it
    /// does for any other table — but `pick_row` only accepts a bare
    /// top-level array, and this response is `{"listItems": […],
    /// "attribution": …}`, an object. That suggestion is a command
    /// guaranteed to fail, so `render_category_table` must not carry it.
    #[test]
    fn list_category_never_suggests_an_id_pick_row_cannot_use() {
        let response = rows(
            r#"{"listItems":[{"canonical_id":"food_and_drink","name":"Food and Drink"}],"attribution":"x"}"#,
        );
        let rendered = render_category_table(&response).expect("a table");
        assert_eq!(rendered.identifier, None);

        // The identifier really would exist if this went through the plain
        // `render_table` path unwrapped — confirming the suppression is what
        // is doing the work here, not some other reason it is absent.
        let extracted = search_category_rows(&response).expect("listItems extracts");
        assert_eq!(
            render_table(&extracted).expect("a table").identifier,
            Some("Food and Drink".to_string())
        );
    }
}
