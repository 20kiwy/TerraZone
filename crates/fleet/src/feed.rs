//! The feeds: requests built from the view's H3 covering, vendor answers parsed network-free
//! (flexible JSON, the keys vendors actually use), and the demo generator that keeps the layer
//! honest to look at before a vendor signs in. The HTTP itself lives in the app crate.

use crate::auth::{self};
use crate::{FleetLayer, FleetState, Provider, Vehicle, VehicleKind, h3grid};

/// One ask for the host's [`crate::FleetFetch`]. `body: Some((json, content-type))` marks a
/// POST — the Bolt sign-in and vehicle poll ride those; everything else is a plain GET.
#[derive(Clone, Debug, PartialEq)]
pub struct FleetRequest {
    /// The answer's return address: `{provider.id}/poll{n}` — unique per poll.
    pub key: String,
    pub provider: Provider,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// `Some((body, content-type))` = POST with this body; `None` = GET.
    pub body: Option<(String, String)>,
    /// The H3 cells this ask covers (empty for bbox/tile feeds) — the runtime keeps them so
    /// the grid the markers sit in draws on the map.
    pub cells: Vec<h3o::CellIndex>,
}

impl FleetRequest {
    pub fn key(provider: Provider, n: usize) -> String {
        format!("{}/poll{n}", format!("{provider:?}").to_lowercase())
    }
}

/// A view's geographic box in degrees: (west, south, east, north).
pub type ViewBox = (f64, f64, f64, f64);

/// The requests the current poll makes: one per live provider. A provider's own
/// [`AuthFlow`] describes its request — Bolt's session POST, the marine tile call with the
/// system's cookie, the public flight feed — and an endpoint override on the layer switches
/// it to template mode (`{res}`/`{cells}`/`{z}`/`{bbox}` filled, the auth's own header
/// riding). Returns the asks plus the providers that had to sit this round out (an
/// unreadable session, say) with the reason, so the runtime can show the honest state.
pub fn build_requests(st: &FleetState, view: ViewBox, zoom: f64, poll: u64) -> (Vec<FleetRequest>, Vec<(Provider, String)>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for layer in &st.layers {
        if !layer.wants_fetch() {
            continue;
        }
        if layer.endpoint.is_some() {
            out.push(build_one(layer, view, zoom, poll));
            continue;
        }
        let flow = auth::flow_for(layer.provider);
        let secret = layer.auth.secret().to_string();
        match flow.poll(&secret, view, zoom, poll) {
            Ok(req) => out.push(req),
            Err(reason) => skipped.push((layer.provider, reason)),
        }
    }
    (out, skipped)
}

fn build_one(layer: &FleetLayer, view: ViewBox, zoom: f64, poll: u64) -> FleetRequest {
    let res = h3grid::res_for_zoom(zoom);
    let template = layer.endpoint.as_deref().unwrap_or_else(|| layer.provider.default_endpoint());
    // The flight feed thinks in bounding boxes (north,south,west,east — the vendor's order);
    // the H3 vendors take the covering cells.
    let (url, key, cells) = if template.contains("{bbox}") {
        let bbox = format!("{:.3},{:.3},{:.3},{:.3}", view.3, view.1, view.0, view.2);
        let url = template.replace("{bbox}", &bbox).replace("{z}", &format!("{:.0}", zoom));
        (url, FleetRequest::key(layer.provider, poll as usize), Vec::new())
    } else {
        let cells = h3grid::cells_covering(view.0, view.1, view.2, view.3, res, 512);
        let joined = cells.iter().map(|c| crate::cell_key(*c)).collect::<Vec<_>>().join(",");
        let url = template.replace("{res}", &res.to_string()).replace("{cells}", &joined).replace("{z}", &format!("{:.0}", zoom));
        (url, FleetRequest::key(layer.provider, poll as usize), cells)
    };
    let mut headers = Vec::new();
    if let Some(h) = layer.auth.header() {
        headers.push(h);
    }
    // A JSON body is what every parser here reads; vendors that want something else get the
    // endpoint field to say so.
    headers.push(("Accept".into(), "application/json".into()));
    FleetRequest { key, provider: layer.provider, url, headers, body: None, cells }
}

/// The system's own handshake: the flow's own cookie-minting request (the marine homepage
/// visit whose `Set-Cookie` becomes the session). Only the system-managed radars bootstrap;
/// Bolt's credential is the user's and flights need none. The host answers with
/// `FleetMsg::Done`'s `set_cookie` (offline, the handshake keeps quietly retrying with the
/// poll cadence).
pub fn build_bootstrap(provider: Provider, poll: u64) -> Option<FleetRequest> {
    let req = auth::flow_for(provider).bootstrap(poll)?;
    Some(FleetRequest { key: req.key, provider, url: req.url, headers: req.headers, body: None, cells: Vec::new() })
}

/// The H3 grid of the fetched items: each marker's own cell, deduped, order-stable — the
/// hexagons the 2D map outlines under the icons so the feeds' tiles read as a grid.
pub fn grid_cells(markers: &[Vehicle], res: h3o::Resolution) -> Vec<h3o::CellIndex> {
    let mut out: Vec<h3o::CellIndex> = Vec::new();
    for v in markers {
        let Ok(ll) = h3o::LatLng::new(v.lat, v.lon) else { continue };
        let cell = ll.to_cell(res);
        if !out.contains(&cell) {
            out.push(cell);
        }
    }
    out
}

/// Parse one provider's answer into vehicles. The vendor-traced shapes come first — Bolt's
/// `{code, message, data:{vehicles:{taxi:{category: [ … ]}}}}` poll and the marine mobile
/// tile's `data.rows` — and anything else falls through to the flexible reader (a bare
/// array, an `{item: …}` wrapper, or the flight feed's `{id: [hex, lat, lon, heading, …]}`
/// shape; keys matched by the names vendors actually use).
pub fn parse(provider: Provider, bytes: &[u8]) -> Result<Vec<Vehicle>, String> {
    match provider {
        Provider::Bolt if looks_like_bolt_poll(bytes) => {
            // The vendor's own shape — its refusal reads as the error it is, never as
            // a generic "nothing here".
            return parse_bolt_poll(bytes);
        }
        Provider::Marine if looks_like_marine_tile(bytes) => {
            return parse_marine_tile(bytes);
        }
        _ => {}
    }
    parse_generic(provider, bytes)
}

/// The Bolt poll's fingerprint: a JSON object carrying a numeric `code`.
fn looks_like_bolt_poll(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .and_then(|v| v.get("code").cloned())
        .is_some_and(|c| c.is_i64() || c.is_u64())
}

/// The marine tile's fingerprint: `data.rows`, or `data` as a plain array.
fn looks_like_marine_tile(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .map(|v| v.pointer("/data/rows").is_some() || v.get("data").is_some_and(|d| d.is_array()))
        .unwrap_or(false)
}

/// Bolt's own poll shape: `{code, message, data:{vehicles:{…}}}` — the vehicles ride in
/// `taxi` (grouped by the zone-specific `icon_id`), rental bikes/scooters in `rent`, the
/// display names and groups in `category_details`, and the per-class artwork URLs in
/// `icons` (the CDN links the runtime downloads once per id and draws as the real pins).
/// `code != 0` is the vendor's own refusal and reads as the error it is — never as an
/// empty street.
fn parse_bolt_poll(bytes: &[u8]) -> Result<Vec<Vehicle>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer is not UTF-8 text".to_string())?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the answer is not JSON: {e}"))?;
    let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(-1);
    if code != 0 {
        let message = v.get("message").and_then(|x| x.as_str()).unwrap_or("(no message)");
        return Err(format!("the feed answered: {message} (code {code})"));
    }
    let Some(vehicles) = v.pointer("/data/vehicles").and_then(|t| t.as_object()) else {
        return Err("no vehicles block in the poll answer".into());
    };
    // The data-driven tables: display names + groups per id, and the artwork per id. A poll
    // without them (an older market?) falls back to the built-in id table.
    let details = v.pointer("/data/vehicles/category_details/taxi").and_then(|d| d.as_object());
    let detail_of = |id: &str| -> Option<(&str, &str)> {
        let d = details?.get(id)?;
        Some((d.get("name").and_then(|x| x.as_str()).unwrap_or(""), d.get("group").and_then(|x| x.as_str()).unwrap_or("")))
    };
    let icons = v.pointer("/data/vehicles/icons").and_then(|d| d.as_object());
    let icon_of = |group: &str, id: &str| -> Option<String> {
        icons?.get(group)?.get(id)?.get("icon_url").and_then(|x| x.as_str()).map(str::to_string).filter(|s| s.starts_with("http"))
    };
    let mut out = Vec::new();
    for (group_key, list) in vehicles {
        if group_key == "icons" || group_key == "category_details" || group_key == "initial_display_categories" {
            continue;
        }
        let Some(list) = list.as_object() else { continue };
        for (category_id, list) in list {
            let Some(list) = list.as_array() else { continue };
            for item in list {
                let (Some(lat), Some(lon)) =
                    (item.get("lat").and_then(|x| x.as_f64()), item.get("lng").or_else(|| item.get("lon")).and_then(|x| x.as_f64()))
                else {
                    continue;
                };
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    continue;
                }
                let icon_id = item
                    .get("icon_id")
                    .map(|x| match x {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                // Name: the vendor's own `category_details` first, the built-in table next,
                // the group key last.
                let name = detail_of(category_id)
                    .map(|(n, _)| n)
                    .filter(|n| !n.is_empty())
                    .map(String::from)
                    .or_else(|| crate::auth::bolt_type_name(&icon_id).map(String::from))
                    .or_else(|| (!category_id.is_empty()).then(|| prettify_category(category_id)));
                // Kind: the vendor's own group first; rentals are bikes by definition; the
                // name/category fallback last.
                let kind = if group_key == "rent" {
                    VehicleKind::Motorcycle
                } else {
                    detail_of(category_id)
                        .and_then(|(_, g)| crate::auth::bolt_group_kind(g))
                        .unwrap_or_else(|| crate::auth::bolt_kind_for(category_id, &icon_id))
                };
                // Artwork: the icons block keys the taxi ids numerically and the rental
                // kinds by name ("scooter", "ebike" are icon_ids).
                let icon_url = icon_of("taxi", &icon_id).or_else(|| icon_of("rent", &icon_id));
                out.push(Vehicle {
                    id: item.get("id").and_then(as_text).unwrap_or_else(|| "bolt".into()),
                    kind,
                    lon,
                    lat,
                    heading_deg: item.get("bearing").and_then(|x| x.as_f64()).map(|h| h as f32).unwrap_or(-1.0),
                    length_m: None,
                    name,
                    category: (!category_id.is_empty() && category_id.parse::<u64>().is_err()).then(|| prettify_category(category_id)),
                    icon_url,
                    alt_m: None,
                });
            }
        }
    }
    if out.is_empty() {
        return Err("the poll carried no vehicles".into());
    }
    Ok(out)
}

/// The marine mobile tile shape: `data.rows` (or a flat `data` array) of objects keyed the
/// way the vendor's app writes them — `LAT`/`LON` (or `Y`/`X`), `SHIP_ID`/`MMSI`,
/// `HEADING`/`COURSE`, `TYPE_NAME`, `SHIPNAME`. Numbers packed into strings parse too.
fn parse_marine_tile(bytes: &[u8]) -> Result<Vec<Vehicle>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer is not UTF-8 text".to_string())?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the answer is not JSON: {e}"))?;
    let rows = v
        .pointer("/data/rows")
        .and_then(|r| r.as_array())
        .or_else(|| v.get("data").and_then(|d| d.as_array()))
        .ok_or_else(|| "no rows in the tile answer".to_string())?;
    let mut out = Vec::new();
    for item in rows {
        let (Some(lat), Some(lon)) = (f_of(item, &["LAT", "lat", "Y", "y"]), f_of(item, &["LON", "lon", "LNG", "lng", "X", "x"])) else {
            continue;
        };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }
        let stype = text_of(item, &["TYPE_NAME", "SHIPTYPE", "shiptype", "TYPE", "type", "ship_type"]);
        let kind = match stype.as_deref().map(|s| s.to_ascii_lowercase()) {
            Some(t) if t.contains("boat") || t.contains("pilot") || t.contains("tug") || t.contains("fishing") => VehicleKind::Boat,
            _ => VehicleKind::Ship,
        };
        out.push(Vehicle {
            id: text_of(item, &["SHIP_ID", "ship_id", "MMSI", "mmsi", "id"]).unwrap_or_else(|| format!("mt-{lat:.5},{lon:.5}")),
            kind,
            lon,
            lat,
            heading_deg: f_of(item, &["HEADING", "heading", "COURSE", "course", "cog"]).map(|h| h as f32).unwrap_or(-1.0),
            length_m: f_of(item, &["LENGTH", "length", "loa", "len"]).map(|l| l as f32),
            name: text_of(item, &["SHIPNAME", "name", "NAME"]),
            category: stype.clone().filter(|s| !s.is_empty()),
            icon_url: None,
            alt_m: None,
        });
    }
    if out.is_empty() {
        return Err("the tile carried no vessels".into());
    }
    Ok(out)
}

/// A field read as text whether the vendor packed it as a number or a string
/// (Bolt's `"id": 123` and `"id": "123"` are the same vehicle).
fn as_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()).filter(|s| !s.trim().is_empty()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A field read as text whether the vendor packed it as a string or a number
/// (`SHIP_ID: 12` and `"MMSI": "123"` are the same ship).
fn text_of(item: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let obj = item.as_object()?;
    for k in keys {
        for (name, val) in obj {
            if name.eq_ignore_ascii_case(k) {
                if let Some(s) = val.as_str() {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                } else if let Some(n) = val.as_i64() {
                    return Some(n.to_string());
                }
            }
        }
    }
    None
}

/// "boda_boda" → "Boda Boda" — the vendor sends category keys lowercased; people read titles.
fn prettify_category(key: &str) -> String {
    key.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str().to_ascii_lowercase().as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn parse_generic(provider: Provider, bytes: &[u8]) -> Result<Vec<Vehicle>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer is not UTF-8 text".to_string())?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the answer is not JSON: {e}"))?;
    let empty = Vec::new();
    let items: Vec<(&String, &serde_json::Value)> = match &v {
        serde_json::Value::Array(a) => a.iter().map(|x| (&DUMMY_KEY, x)).collect(),
        serde_json::Value::Object(o) => {
            // A wrapper array of objects ({data: […]} / {positions: […]}) wraps the items; any
            // other object maps id → item, which is how the flight feed reads ({hex: [ … ]}).
            let items_of = |x: &serde_json::Value| x.is_array() && x.as_array().is_some_and(|a| a.first().is_some_and(serde_json::Value::is_object));
            if let Some(w) = o.values().find(|x| items_of(x)) {
                w.as_array().unwrap_or(&empty).iter().map(|x| (&DUMMY_KEY, x)).collect()
            } else {
                o.iter().collect()
            }
        }
        _ => return Err("the answer is neither an array nor an object".into()),
    };
    let mut out = Vec::new();
    for (k, item) in items {
        if let Some(v) = read_vehicle(provider, k, item) {
            out.push(v);
        }
    }
    if out.is_empty() {
        return Err("no vehicles in the answer (auth may be missing or the feed empty)".into());
    }
    Ok(out)
}

/// A stable key placeholder for array items.
static DUMMY_KEY: String = String::new();

fn f_of(item: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    let obj = item.as_object()?;
    for k in keys {
        for (name, val) in obj {
            if name.eq_ignore_ascii_case(k) {
                if let Some(n) = val.as_f64() {
                    return Some(n);
                }
                // The flight feed packs numbers into string arrays positionally; stringly numbers
                // parse too.
                if let Some(s) = val.as_str()
                    && let Ok(n) = s.trim().parse::<f64>()
                {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn s_of(item: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let obj = item.as_object()?;
    for k in keys {
        for (name, val) in obj {
            if name.eq_ignore_ascii_case(k)
                && let Some(s) = val.as_str()
            {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn read_vehicle(provider: Provider, id: &str, item: &serde_json::Value) -> Option<Vehicle> {
    // The flight feed's array shape: [squawk, lat, lon, track, alt, speed, …, type_code,
    // registration, …, origin, destination, flight, …, callsign] — the indices the vendor's
    // own web client reads (verified live: `"DH8C"`/"B788" ride index 8, the registration
    // index 9, the callsign 16).
    if let Some(arr) = item.as_array() {
        if arr.len() >= 4 {
            let lat = arr.get(1).and_then(serde_json::Value::as_f64);
            let lon = arr.get(2).and_then(serde_json::Value::as_f64);
            let heading = arr.get(3).and_then(serde_json::Value::as_f64);
            if let (Some(lat), Some(lon)) = (lat, lon) {
                let type_code = arr.get(8).and_then(serde_json::Value::as_str).unwrap_or("");
                let reg = arr.get(9).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty());
                let flight = arr.get(13).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty());
                let callsign = arr.get(16).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty());
                // Index 4 is the altitude in feet (the vendor's own array order) — metres
                // for the 3D elevation lift.
                let alt_m = arr.get(4).and_then(serde_json::Value::as_f64).filter(|a| *a > 0.0).map(|ft| (ft * 0.3048) as f32);
                return Some(Vehicle {
                    id: if id.is_empty() { "aircraft".into() } else { id.to_string() },
                    kind: flight_kind_for(type_code),
                    lon,
                    lat,
                    heading_deg: heading.map(|h| h as f32).unwrap_or(-1.0),
                    length_m: None,
                    name: reg.or(flight).or(callsign).map(String::from),
                    category: (!type_code.is_empty()).then(|| type_code.to_string()),
                    icon_url: None,
                    alt_m,
                });
            }
        }
        return None;
    }
    let lat = f_of(item, &["lat", "latitude", "LAT"])?;
    let lon = f_of(item, &["lon", "lng", "longitude", "LON"])?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    let type_name = s_of(item, &["type", "kind", "category", "vtype", "vehicleType"]).unwrap_or_default();
    let kind = match provider {
        Provider::Flight => flight_kind_for(&type_name),
        Provider::Marine => {
            let len = f_of(item, &["length", "loa", "len"]);
            match len {
                Some(l) if l < 20.0 => VehicleKind::Boat,
                _ => VehicleKind::Ship,
            }
        }
        Provider::Bolt => {
            let t = type_name.to_ascii_lowercase();
            if t.contains("bike") || t.contains("moto") || t.contains("scoot") {
                VehicleKind::Motorcycle
            } else if t.contains("van") || t.contains("cargo") || t.contains("xl") {
                VehicleKind::Van
            } else {
                VehicleKind::Car
            }
        }
        Provider::Sgr => VehicleKind::Train,
    };
    Some(Vehicle {
        id: s_of(item, &["id", "uuid", "mmsi", "icao", "reg", "callsign"])
            .unwrap_or_else(|| if id.is_empty() { "vehicle".into() } else { id.to_string() }),
        kind,
        lon,
        lat,
        heading_deg: f_of(item, &["heading", "track", "course", "dir", "hdg"]).map(|h| h as f32).unwrap_or(-1.0),
        length_m: f_of(item, &["length", "loa", "len"]).map(|l| l as f32),
        name: s_of(item, &["name", "callsign", "mmsi", "reg", "icao", "flight"]),
        category: (!type_name.is_empty()).then_some(type_name),
        icon_url: None,
        // The flight fields report feet (FR24's alt / alt_geom); everything else metres.
        alt_m: f_of(item, &["alt", "altitude", "alt_geom"]).map(|a| if a > 2_000.0 { (a * 0.3048) as f32 } else { a as f32 }),
    })
}

/// The aircraft kind from the vendor's model code (ICAO type designator, `B788`, `DH8C`,
/// `H145`, …) — the by-type read the flight layer draws: rotorcraft fly the helicopter
/// icon, wide-bodies (freighters included) the heavy plane, light GA the small plane, and
/// everything else the everyday airliner.
pub fn flight_kind_for(type_code: &str) -> VehicleKind {
    let c = type_code.trim().to_ascii_uppercase();
    if c.is_empty() {
        return VehicleKind::Plane;
    }
    // Rotorcraft families first — the exact codes and the prefixes that only helicopters
    // use (a `B4` Bell is a rotorcraft unless it is a BAe 146, `B46x`).
    const HELI: &[&str] = &[
        "A109", "A119", "A139", "AS35", "AS50", "AS65", "B06", "B105", "B106", "B107", "B212", "B214", "B222", "B230", "B407", "B412", "B427",
        "B429", "B430", "B439", "B525", "EC13", "EC20", "EC25", "EC35", "EC45", "EC55", "EC6", "EC7", "H12", "H13", "H14", "H15", "H16", "H17",
        "H21", "H22", "H25H", "KA26", "KA27", "KA32", "KA50", "KA52", "KA62", "MD52", "MD53", "MD60", "MI2", "MI8", "MI17", "MI24", "MI26", "MI28",
        "MI34", "MI38", "OH58", "R22", "R24", "R44", "R66", "S61", "S64", "S65", "S67", "S70", "S75", "S76", "S92", "S97", "UH1", "UH60", "V22",
        "G2", "SW4",
    ];
    if HELI.iter().any(|h| c == *h) {
        return VehicleKind::Helicopter;
    }
    if c.starts_with("EC")
        || c.starts_with("MI")
        || c.starts_with("UH")
        || c.starts_with("AH")
        || c.starts_with("OH")
        || [
            "AS3", "AS5", "AS6", "AW1", "AW3", "AW6", "AW9", "B06", "B21", "B22", "B40", "B41", "B42", "B43", "KA2", "KA3", "KA5", "R22", "R24",
            "R44", "R66", "S61", "S64", "S65", "S67", "S70", "S75", "S76", "S92", "V2", "G2", "SW4", "H12", "H13", "H14", "H15", "H16", "H17", "H21",
            "H22",
        ]
        .iter()
        .any(|h| c.starts_with(h))
    {
        return VehicleKind::Helicopter;
    }
    // Heavy / wide-body (the freighters read here — the type code is the airframe, and the
    // big ones are the cargo planes the feed is asked about).
    if c == "C17"
        || ["B74", "B76", "B77", "B78", "A30", "A31", "A33", "A34", "A35", "A38", "MD11", "DC10", "K35", "IL7", "IL9", "A124", "AN12", "L101", "C5M"]
            .iter()
            .any(|h| c.starts_with(h))
    {
        return VehicleKind::PlaneHeavy;
    }
    // Light general aviation (Cessna singles and twins, Pipers, SR22s, light jets — the
    // four-character codes; the three-character `C17` Globemaster hit the heavy branch above).
    if c.starts_with("C1")
        || c.starts_with("C2")
        || [
            "C4", "C5", "C6", "C7", "C8", "C9", "P28", "PA2", "PA3", "SR2", "DA2", "DA4", "M20", "BE1", "BE3", "BE5", "BE7", "BE9", "LJ4", "LJ6",
            "LJ7", "GLF", "GL5", "GL6", "E5", "EA5", "FA5", "FA7", "SF5", "JS3", "JS4", "TBM",
        ]
        .iter()
        .any(|h| c.starts_with(h))
    {
        return VehicleKind::PlaneLight;
    }
    VehicleKind::Plane
}

/// The demo feed: deterministic sample traffic in the covering cells, drifting one step per
/// tick — the layer's own look, clearly labelled demo, generated offline so the icons, the
/// marine sizing and the 3D markers can be seen before any vendor signs in. The density
/// follows the zoom the way a real fleet would: road traffic crowds a street view (one to
/// three per block, biased to the road kinds), and the flight kinds cruise at altitude so
/// the 3D environment can lift them.
pub fn demo_markers(view: ViewBox, zoom: f64, tick: u64) -> Vec<Vehicle> {
    // Street zooms ride the ladder's finest rung (res 9, a city block): the feed's own
    // ladder stops at res 8 so a poll can't flood the vendor, but the demo asks nobody —
    // and a street view covering three 500 m cells can't read as traffic however the
    // per-cell count scales. Wide views keep the feed's rung.
    let res = if zoom >= 15.0 { h3o::Resolution::try_from(9u8).unwrap_or_else(|_| h3grid::res_for_zoom(zoom)) } else { h3grid::res_for_zoom(zoom) };
    let cells = h3grid::cells_covering(view.0, view.1, view.2, view.3, res, 96);
    // Street zooms pack the blocks; wide views keep one vehicle per cell (the 0.16.0 look).
    let per_cell = if zoom >= 12.5 { 2 } else { 1 };
    let mut out = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        // One vehicle per cell where the cell's hash says so (a sparse, steady fleet).
        let h = hash(u64::from(*cell));
        if h % 4 == 3 {
            continue;
        }
        let (clon, clat) = h3grid::cell_center(*cell);
        let drift = |k: u64| -> f64 {
            let v = hash(h ^ (k << 8) ^ tick.wrapping_mul(0x9e37_79b9)) as f64 / u64::MAX as f64;
            (v - 0.5) * 0.6 * h3grid::cell_span_deg(res)
        };
        // The demo shows the whole type system offline: street traffic, ships of the marine
        // ladder, and the flight layer's by-type read — the everyday airliner, the wide-body
        // freighter, the rotorcraft — each in its own marker (see `flight_kind_for`). At
        // street zooms the road kinds lead (a city block reads as traffic, not an airshow).
        let fine = u8::from(res) >= 8;
        let which = if fine { h % 10 } else { h % 7 };
        let (kind, length) = match which {
            0 => (VehicleKind::Car, None),
            1 => (VehicleKind::Motorcycle, None),
            2 => (VehicleKind::Ship, Some(18.0 + (h % 90) as f32)),
            3 => (VehicleKind::Van, None),
            4 => (VehicleKind::Plane, None),
            5 => (VehicleKind::PlaneHeavy, Some(44.0 + (h % 35) as f32)),
            6 => (VehicleKind::Helicopter, None),
            // The street-only rungs: a city block is cars, vans and bikes mostly.
            7 => (VehicleKind::Car, None),
            8 => (VehicleKind::Van, None),
            _ => (VehicleKind::Motorcycle, None),
        };
        // Flyers cruise: the airliners a few hundred metres up, the rotorcraft low — the
        // 3D environment lifts them by this and drops a leader line to the ground.
        let alt_m = match kind {
            VehicleKind::Helicopter => Some(120.0 + (h % 180) as f32),
            VehicleKind::Plane => Some(350.0 + (h % 550) as f32),
            VehicleKind::PlaneHeavy => Some(600.0 + (h % 900) as f32),
            _ => None,
        };
        for j in 0..per_cell {
            // Extra vehicles in a cell jitter to their own corner of the block.
            let sub = hash(h ^ (j as u64 + 1).wrapping_mul(0x2545_f491_4f6c_dd1d));
            let spread = |k: u64| -> f64 {
                let v = hash(sub ^ (k << 8) ^ tick.wrapping_mul(0x9e37_79b9)) as f64 / u64::MAX as f64;
                (v - 0.5) * 0.5 * h3grid::cell_span_deg(res)
            };
            out.push(Vehicle {
                id: format!("demo-{}-{}-{j}", crate::cell_key(*cell), i),
                kind,
                lon: clon + drift(1) + spread(1),
                lat: clat + drift(2) + spread(2),
                heading_deg: (sub % 360) as f32,
                length_m: length,
                name: Some(format!("demo {i}")),
                category: None,
                icon_url: None,
                alt_m: alt_m.map(|a| a + 40.0 * j as f32),
            });
        }
    }
    out
}

fn hash(mut v: u64) -> u64 {
    v ^= v >> 33;
    v = v.wrapping_mul(0xff51_afd7_ed55_8ccd);
    v ^= v >> 33;
    v = v.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    v ^= v >> 33;
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Auth;
    use crate::sizing;

    fn state() -> FleetState {
        let mut st = FleetState::default();
        let session = crate::auth::BoltSession {
            session_id: "sess".into(),
            device_id: "dev".into(),
            auth_username: "uid_7".into(),
            phone: "+255700000000".into(),
            created_ms: 0,
        };
        st.layer_mut(Provider::Bolt).auth = Auth::Token(session.to_secret());
        st.layer_mut(Provider::Flight).auth = Auth::Cookie("c".into());
        st.layer_mut(Provider::Marine).auth = Auth::System("".into());
        st
    }

    const SINZA: ViewBox = (39.18, -6.80, 39.27, -6.74);

    #[test]
    fn requests_go_to_live_layers_only_with_their_auth() {
        let (reqs, _skipped) = build_requests(&state(), SINZA, 16.0, 7);
        assert_eq!(reqs.len(), 3, "a pending system handshake asks for no data yet; the public SGR sweep does");
        let bolt = reqs.iter().find(|r| r.provider == Provider::Bolt).unwrap();
        assert!(bolt.url.contains("mobility/search/poll?"), "bolt rides the vendor poll: {}", bolt.url);
        match &bolt.body {
            Some((body, ct)) => {
                assert_eq!(ct, "application/json");
                assert!(body.contains(r#""stage":"category_selection""#), "{body}");
            }
            None => panic!("the bolt poll is a POST"),
        }
        assert!(bolt.headers.iter().any(|(k, v)| k == "Authorization" && v.starts_with("Basic ")));
        let flight = reqs.iter().find(|r| r.provider == Provider::Flight).unwrap();
        assert!(flight.url.contains("bounds="), "the flight feed takes a bounds box: {}", flight.url);
        assert!(flight.headers.iter().any(|(k, v)| k == "Cookie" && v == "c"));
        let sgr = reqs.iter().find(|r| r.provider == Provider::Sgr).unwrap();
        assert!(sgr.url.contains("sgrticket-api.trc.co.tz"), "the sweep asks the TRC API: {}", sgr.url);
        assert!(!sgr.headers.iter().any(|(k, _)| k == "Cookie"), "the schedule service is public");
        // Poll number keeps keys unique per poll.
        let (a, _) = build_requests(&state(), SINZA, 16.0, 7);
        let (b, _) = build_requests(&state(), SINZA, 16.0, 8);
        assert_ne!(a[0].key, b[0].key);
        // An unreadable session sits the round out, with the reason — not silently.
        let mut stale = state();
        stale.layer_mut(Provider::Bolt).auth = Auth::Token("not json".into());
        let (reqs, skipped) = build_requests(&stale, SINZA, 16.0, 9);
        assert!(!reqs.iter().any(|r| r.provider == Provider::Bolt));
        assert!(skipped.iter().any(|(p, r)| *p == Provider::Bolt && r.contains("sign in again")), "{skipped:?}");
        // An endpoint override switches the layer to template mode.
        let mut custom = state();
        custom.layer_mut(Provider::Flight).endpoint = Some("https://example.com/feed?bbox={bbox}".into());
        let (reqs, _) = build_requests(&custom, SINZA, 16.0, 10);
        let flight = reqs.iter().find(|r| r.provider == Provider::Flight).unwrap();
        assert!(flight.url.contains("bbox=-6.740,-6.800,39.180,39.270"), "bbox rides north-first: {}", flight.url);
    }

    #[test]
    fn the_flight_feed_polls_without_any_credential() {
        let mut st = FleetState::default();
        st.layer_mut(Provider::Flight).auth = Auth::System(String::new());
        let (reqs, _) = build_requests(&st, SINZA, 16.0, 1);
        assert!(reqs.iter().any(|r| r.provider == Provider::Flight), "public feed: the system just asks");
    }

    #[test]
    fn the_system_bootstraps_only_the_radars() {
        assert!(build_bootstrap(Provider::Bolt, 1).is_none(), "Bolt's credential is the user's");
        let marine = build_bootstrap(Provider::Marine, 1).unwrap();
        assert!(marine.url.starts_with("https://www.marinetraffic.com"), "{}", marine.url);
        assert!(marine.key.starts_with("marine-bootstrap"));
        assert!(build_bootstrap(Provider::Flight, 2).is_none(), "the flight feed is public — nothing to mint");
    }

    #[test]
    fn grid_cells_dedupe_and_stay_stable() {
        let ms = demo_markers(SINZA, 16.0, 1);
        assert!(!ms.is_empty());
        let cells = grid_cells(&ms, h3o::Resolution::Eight);
        assert!(!cells.is_empty());
        let len = cells.len();
        assert_eq!(grid_cells(&ms, h3o::Resolution::Eight), cells, "deterministic");
        assert!(grid_cells(&ms[..len.min(1)], h3o::Resolution::Eight).len() == 1, "one marker, one cell");
        // Every cell actually contains the marker that named it.
        for v in &ms[..2] {
            let cell = h3o::LatLng::new(v.lat, v.lon).unwrap().to_cell(h3o::Resolution::Eight);
            assert!(cells.contains(&cell));
        }
    }

    #[test]
    fn street_zooms_pack_the_demo_and_flyers_carry_altitude() {
        // The 0.17.0 shot's failure: a street view held ~5 markers — the fleet read absent.
        // Street zooms now pack 1–3 vehicles per block; a wide view keeps the sparse look.
        let street = demo_markers(SINZA, 16.0, 1);
        assert!(!street.is_empty());
        let tiny = demo_markers((39.222, -6.782, 39.224, -6.780), 10.0, 1);
        assert!(street.len() > tiny.len() || street.len() >= 12, "street {} vs tiny {}", street.len(), tiny.len());
        // Flyers cruise at altitude (the 3D environment lifts them by it); ground kinds don't.
        let flying = street.iter().filter(|v| v.kind.flies()).count();
        if flying > 0 {
            assert!(street.iter().filter(|v| v.kind.flies()).all(|v| v.alt_m.is_some_and(|a| a > 50.0)), "every flyer reports an altitude");
        }
        assert!(street.iter().filter(|v| !v.kind.flies()).all(|v| v.alt_m.is_none()), "ground traffic sits on the ground");
    }

    #[test]
    fn parses_object_array_and_flight_shapes() {
        // Object array (the marine/bolt shape).
        let marine = br#"{"data":[{"mmsi":"123","lat":-6.78,"lon":39.22,"heading":180,"length":45,"name":"Trawler"}]}"#;
        let v = parse(Provider::Marine, marine).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, VehicleKind::Ship);
        assert_eq!(v[0].id, "123");
        assert_eq!(v[0].length_m, Some(45.0));
        assert_eq!(v[0].name.as_deref(), Some("Trawler"));

        // Bare array.
        let bolt = br#"[{"id":"d1","lat":-6.778,"lon":39.223,"type":"bike","heading":90.0},{"id":"d2","lat":-6.7,"lon":39.2,"type":"van"}]"#;
        let v = parse(Provider::Bolt, bolt).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].kind, VehicleKind::Motorcycle);
        assert_eq!(v[1].kind, VehicleKind::Van);

        // The flight feed's {hex: [..]} shape — the vendor's own indices: type at 8, the
        // registration at 9, the callsign at 16.
        let flight = br#"{"full_count":2,"version":5,"abc123":["abc123",-6.78,39.22,270.0,11000,850,"","","C208","5H-XYZ",0,"","","TSS100",0,1216,"",0,"AUK"],"xyz":["xyz",0.0,0.0,0.0,0,0,""]}"#;
        let v = parse(Provider::Flight, flight).unwrap();
        assert_eq!(v.len(), 2, "metadata keys (full_count, version) skip: {v:?}");
        assert_eq!(v[0].kind, VehicleKind::PlaneLight, "a Cessna 208 is light GA");
        assert_eq!(v[0].heading_deg, 270.0);
        assert_eq!(v[0].name.as_deref(), Some("5H-XYZ"), "the registration names the plane");
        assert_eq!(v[0].category.as_deref(), Some("C208"), "the type code rides as the category");
        // The altitude rides too (index 4, feet → metres) — the 3D elevation lift.
        let alt = v[0].alt_m.expect("the array's alt index is read");
        assert!((alt - 11_000.0 * 0.3048).abs() < 1.0, "11000 ft ≈ {alt} m");

        // The by-type read: heavy wide-bodies, helicopters, everyday airliners.
        let heavy = br#"{"h1":["h1",-6.7,39.2,90.0,35000,430,"","","B788","5H-TCJ"]}"#;
        assert_eq!(parse(Provider::Flight, heavy).unwrap()[0].kind, VehicleKind::PlaneHeavy, "a 787 is a heavy");
        let heli = br#"{"h2":["h2",-6.7,39.2,90.0,1200,80,"","","H145","5H-HELO"]}"#;
        assert_eq!(parse(Provider::Flight, heli).unwrap()[0].kind, VehicleKind::Helicopter);
        let mid = br#"{"h3":["h3",-6.7,39.2,90.0,32000,410,"","","A320","5H-FZN"]}"#;
        assert_eq!(parse(Provider::Flight, mid).unwrap()[0].kind, VehicleKind::Plane);
        let freighter = br#"{"h4":["h4",-6.7,39.2,90.0,33000,420,"","","B77L","N770FT"]}"#;
        assert_eq!(parse(Provider::Flight, freighter).unwrap()[0].kind, VehicleKind::PlaneHeavy, "the freighter airframe reads heavy");
    }

    #[test]
    fn parses_the_bolt_poll_and_the_marine_tile_shapes() {
        // Bolt: category map + zone-specific icon ids — with the vendor's own data-driven
        // tables (category_details names/groups, icons artwork) when the market sends them.
        let bolt = br#"{"code":0,"message":"OK","data":{"vehicles":{"taxi":{"boda_boda":[{"id":"b1","lat":-6.778,"lng":39.223,"bearing":90.0,"icon_id":"278"}],"premium":[{"id":"p1","lat":-6.8,"lng":39.3,"bearing":0,"icon_id":"9999"}]}}}}"#;
        let v = parse(Provider::Bolt, bolt).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].kind, VehicleKind::Motorcycle, "icon 278 is the boda");
        assert_eq!(v[0].name.as_deref(), Some("Boda"));
        assert_eq!(v[1].kind, VehicleKind::Car);
        assert_eq!(v[1].name.as_deref(), Some("Premium"), "the category names an unknown id");
        // A refusal is the error it is.
        let refused = br#"{"code":401,"message":"unauthorized"}"#;
        let err = parse(Provider::Bolt, refused).unwrap_err();
        assert!(err.contains("unauthorized"), "{err}");

        // The data-driven market answer: names + groups + CDN artwork straight from the
        // vendor (Tallinn's shape — ids the built-in table has never seen).
        let europe = br#"{"code":0,"message":"OK","data":{"vehicles":{
            "taxi":{"3428":[{"id":"2426","lat":59.439,"lng":24.754,"bearing":176.6,"icon_id":"3428"}],
                    "3432":[{"id":"99","lat":59.44,"lng":24.76,"bearing":10.0,"icon_id":"3432"}]},
            "rent":{"1":[{"id":"474097","lat":59.437,"lng":24.7545,"bearing":0,"icon_id":"scooter"}]},
            "icons":{"taxi":{"3428":{"icon_url":"https://images.bolt.eu/store/pin3428.png"}},
                     "rent":{"scooter":{"icon_url":"https://images.bolt.eu/mobile/ic_pin_scooter_v2.png"}}},
            "category_details":{"taxi":{"3428":{"name":"Bolt","group":"standard"},"3432":{"name":"XL","group":"xl"}}},
            "initial_display_categories":{"taxi":["3428"]}}}}"#;
        let v = parse(Provider::Bolt, europe).unwrap();
        assert_eq!(v.len(), 3, "taxi and rent vehicles both read: {v:?}");
        let bolt_car = v.iter().find(|x| x.id == "2426").unwrap();
        assert_eq!(bolt_car.kind, VehicleKind::Car, "the group 'standard' drives a car");
        assert_eq!(bolt_car.name.as_deref(), Some("Bolt"), "the vendor's own display name");
        assert_eq!(bolt_car.icon_url.as_deref(), Some("https://images.bolt.eu/store/pin3428.png"), "the artwork rides the marker");
        let xl = v.iter().find(|x| x.id == "99").unwrap();
        assert_eq!(xl.kind, VehicleKind::Van, "group 'xl' is the van");
        let scooter = v.iter().find(|x| x.id == "474097").unwrap();
        assert_eq!(scooter.kind, VehicleKind::Motorcycle, "rentals ride the bike icon");
        assert_eq!(scooter.icon_url.as_deref(), Some("https://images.bolt.eu/mobile/ic_pin_scooter_v2.png"));
    }

    #[test]
    fn parses_the_marine_tile_shapes() {
        // Marine mobile tile: data.rows with the app's SHOUTING keys.
        let tile = br#"{"data":{"rows":[{"SHIP_ID":12,"LAT":"-6.78","LON":39.22,"HEADING":180,"TYPE_NAME":"Tanker","SHIPNAME":"Ever Given"}]}}"#;
        let v = parse(Provider::Marine, tile).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "12");
        assert_eq!(v[0].kind, VehicleKind::Ship);
        assert_eq!(v[0].heading_deg, 180.0);
        assert_eq!(v[0].name.as_deref(), Some("Ever Given"));
        // Small workboats read as boats.
        let tile = br#"{"data":[{"MMSI":"1","lat":-6.7,"lon":39.2,"TYPE_NAME":"Pilot boat"}]}"#;
        assert_eq!(parse(Provider::Marine, tile).unwrap()[0].kind, VehicleKind::Boat);
    }

    #[test]
    fn parse_rejects_junk_and_empty_answers() {
        assert!(parse(Provider::Bolt, b"not json").is_err());
        assert!(parse(Provider::Bolt, br#"{"error":"unauthorized"}"#).is_err(), "an error object has no vehicles");
        assert!(parse(Provider::Bolt, b"[]").is_err(), "an empty array means the feed answered with nothing");
    }

    #[test]
    fn demo_is_deterministic_but_drifts() {
        let a = demo_markers(SINZA, 16.0, 1);
        let b = demo_markers(SINZA, 16.0, 1);
        let c = demo_markers(SINZA, 16.0, 2);
        assert_eq!(a, b, "same tick, same fleet");
        assert!(!a.is_empty(), "a city box has demo traffic");
        let ids_a: std::collections::BTreeSet<_> = a.iter().map(|v| v.id.clone()).collect();
        let ids_c: std::collections::BTreeSet<_> = c.iter().map(|v| v.id.clone()).collect();
        assert_eq!(ids_a, ids_c, "the same vehicles drift; none respawn");
        assert!(a.iter().any(|v| v.kind == VehicleKind::Ship && v.length_m.is_some()), "the marine sizing shows");
    }

    #[test]
    fn sizing_and_icons_agree() {
        for v in demo_markers(SINZA, 16.0, 3) {
            let class = sizing::class_of(v.kind, v.length_m);
            assert!(sizing::px(class) >= 7.0);
            assert!(!v.kind.icon().is_empty());
        }
    }
}
