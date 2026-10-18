//! `cargo xtask rail`: load and check the SGR railway atlas from the fleet crate's assets.
//!
//! The atlas (`crates/fleet/assets/tz-rail.json` — OSM's Tanzania railways and stations,
//! extracted once from GeoFabrik's Tanzania extract) is what the app embeds via
//! `terrazon_fleet::rail::embedded_atlas` and what the SGR trains ride. This command reads
//! the same file the way the parser does, proves the shape, and verifies the SGR corridor's
//! own stations are present by name — so a truncated, re-ordered or wrongly-extracted asset
//! fails here, in the tooling, before it ever reaches a map.

use std::path::Path;

/// The asset the fleet crate embeds (kept in step with `rail::embedded_atlas`'s include).
const ASSET: &str = "crates/fleet/assets/tz-rail.json";

/// The SGR corridor's stations (Dar es Salaam → Dodoma, the vendor's own ordered list) —
/// every one must be in the atlas by name, or the timetable's trains have nowhere to stop.
const SGR_CORRIDOR: &[&str] =
    &["Dar Es Salaam", "Pugu", "Soga", "Ruvu", "Ngerengere", "Morogoro", "Mkata", "Kilosa", "Kidete", "Gulwe", "Igandu", "Makutupora", "Dodoma"];

/// A stop name normalized for matching (the parser's own rule: lowercase, punctuation out).
fn normalize(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
}

pub fn run(root: &Path) -> Result<(), String> {
    let path = root.join(ASSET);
    let bytes = std::fs::read(&path).map_err(|e| format!("{ASSET}: {e} — the fleet crate's atlas asset is missing"))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("{ASSET} is not JSON: {e}"))?;

    // The parser's own shape rules: flat `[lon,lat,…]` line arrays (≥ 2 points survive) and
    // `[name, lon, lat]` station rows in range.
    let mut lines = 0usize;
    let mut points = 0usize;
    let mut dropped_lines = 0usize;
    for line in v.get("lines").and_then(|l| l.as_array()).map(Vec::as_slice).unwrap_or_default() {
        let Some(flat) = line.as_array() else {
            dropped_lines += 1;
            continue;
        };
        let pts = flat.as_chunks::<2>().0.iter().filter(|c| c[0].as_f64().is_some() && c[1].as_f64().is_some()).count();
        if pts < 2 {
            dropped_lines += 1;
            continue;
        }
        lines += 1;
        points += pts;
    }
    let mut stations: Vec<(String, f64, f64)> = Vec::new();
    let mut dropped_stations = 0usize;
    for st in v.get("stations").and_then(|s| s.as_array()).map(Vec::as_slice).unwrap_or_default() {
        let Some(row) = st.as_array() else {
            dropped_stations += 1;
            continue;
        };
        if row.len() < 3 {
            dropped_stations += 1;
            continue;
        }
        let (Some(name), Some(lon), Some(lat)) = (row[0].as_str(), row[1].as_f64(), row[2].as_f64()) else {
            dropped_stations += 1;
            continue;
        };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            dropped_stations += 1;
            continue;
        }
        stations.push((name.to_string(), lon, lat));
    }
    if lines == 0 {
        return Err(format!("{ASSET}: no railway lines survived the parse — the asset is truncated or reshaped"));
    }
    if stations.is_empty() {
        return Err(format!("{ASSET}: no named stations survived the parse"));
    }

    // The corridor check: every SGR station by normalized name.
    let names: Vec<String> = stations.iter().map(|(n, _, _)| normalize(n)).collect();
    let missing: Vec<&str> = SGR_CORRIDOR.iter().filter(|s| !names.contains(&normalize(s))).copied().collect();
    if !missing.is_empty() {
        return Err(format!("{ASSET}: the SGR corridor is incomplete — missing station(s): {}", missing.join(", ")));
    }

    // Coordinates sanity: the atlas is Tanzania's railways — every station within the
    // country's rough bounds (a swapped lon/lat pair lands far outside).
    let outside: Vec<String> = stations
        .iter()
        .filter(|(_, lon, lat)| !(-12.0..=-2.0).contains(lat) || !(28.0..=42.0).contains(lon))
        .map(|(n, _, _)| n.clone())
        .take(4)
        .collect();
    if !outside.is_empty() {
        return Err(format!("{ASSET}: station(s) outside Tanzania's bounds (lon/lat swapped?): {}", outside.join(", ")));
    }

    println!(
        "rail: {} OK — {lines} railway lines ({points} points), {} named stations, the {} SGR corridor stations all present",
        ASSET,
        stations.len(),
        SGR_CORRIDOR.len(),
    );
    if dropped_lines + dropped_stations > 0 {
        println!("  (skipped in parse: {dropped_lines} line(s), {dropped_stations} station row(s))");
    }
    let (dar, _, _) = stations.iter().find(|(n, _, _)| normalize(n) == normalize("Dar Es Salaam")).unwrap();
    let (dod, _, _) = stations.iter().find(|(n, _, _)| normalize(n) == normalize("Dodoma")).unwrap();
    println!("  terminals: {dar} → {dod}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizer_matches_the_parser_s_rule() {
        assert_eq!(normalize("Dar Es Salaam"), normalize("Dar es Salaam"));
        assert_eq!(normalize("Mang'ula"), "mangula");
    }

    #[test]
    fn the_corridor_list_is_complete() {
        assert_eq!(SGR_CORRIDOR.len(), 13);
        assert_eq!(SGR_CORRIDOR[0], "Dar Es Salaam");
        assert_eq!(SGR_CORRIDOR[12], "Dodoma");
    }
}
