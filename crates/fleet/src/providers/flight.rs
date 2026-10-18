//! The Flight provider's own API module: the vendor's public feed — the flexible reader's
//! array shape and the aircraft-kind read from ICAO type designators — separated from the
//! shared fetch plumbing. The feed needs no credential (the flow says so in
//! [`crate::auth`]); the request building lives in [`crate::feed`]; this module reads the
//! answer and names the airframe.

use crate::{Provider, Vehicle, VehicleKind, feed::read_vehicle};

/// The aircraft answer's reader: the object keyed by hex (the flexible reader's own shape)
/// and — the vendor's newer wrapper — `{"aircraft": […]}`. An honest empty (a quiet box)
/// reads as zero vehicles, not an error.
pub fn parse_feed(bytes: &[u8]) -> Result<Vec<Vehicle>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer is not UTF-8 text".to_string())?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the answer is not JSON: {e}"))?;
    if let Some(list) = v.get("aircraft").and_then(|x| x.as_array()) {
        let out: Vec<Vehicle> = list.iter().enumerate().filter_map(|(i, item)| read_vehicle(Provider::Flight, &i.to_string(), item)).collect();
        return Ok(out);
    }
    crate::feed::parse_generic(Provider::Flight, bytes)
}

/// The aircraft kind from the vendor's model code (ICAO type designator, `B788`, `DH8C`,
/// `H145`, …) — the by-type read the flight layer draws: rotorcraft fly the helicopter
/// icon, wide-bodies (freighters included) the heavy plane, light GA the small plane, and
/// everything else the everyday airliner.
pub fn kind_for(type_code: &str) -> VehicleKind {
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
