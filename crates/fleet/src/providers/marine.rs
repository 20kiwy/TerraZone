//! The Marine provider's own API module: the vendor's mobile tile answer — its `data.rows`
//! shape and its fingerprints — separated from the shared fetch plumbing. The system-minted
//! session (the homepage bootstrap whose `Set-Cookie` becomes the credential) lives in
//! [`crate::auth`]; the request building lives in [`crate::feed`]; this module reads the
//! answer.

use crate::{Vehicle, VehicleKind, feed::f_of, feed::text_of};

/// The marine tile's fingerprint: `data.rows`, or `data` as a plain array.
pub fn looks_like_tile(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .map(|v| v.pointer("/data/rows").is_some() || v.get("data").is_some_and(|d| d.is_array()))
        .unwrap_or(false)
}

/// The marine mobile tile shape: `data.rows` (or a flat `data` array) of objects keyed the
/// way the vendor's app writes them — `LAT`/`LON` (or `Y`/`X`), `SHIP_ID`/`MMSI`,
/// `HEADING`/`COURSE`, `TYPE_NAME`, `SHIPNAME`. Numbers packed into strings parse too.
pub fn parse_tile(bytes: &[u8]) -> Result<Vec<Vehicle>, String> {
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
            scheduled: false,
        });
    }
    if out.is_empty() {
        return Err("the tile carried no vessels".into());
    }
    Ok(out)
}
