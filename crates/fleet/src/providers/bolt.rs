//! The Bolt provider's own API module: the vendor's vehicle poll — its shape, its
//! fingerprints and its refusal semantics — separated from the shared fetch plumbing the
//! way the vendor separates it. The sign-in flow lives in [`crate::auth`] (all auth
//! together); the request building lives in [`crate::feed`] (all fetch together); this
//! module is the poll answer's reader.

use crate::{Vehicle, VehicleKind};

/// The Bolt poll's fingerprint: a JSON object carrying a numeric `code`.
pub fn looks_like_poll(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .and_then(|v| v.get("code").cloned())
        .is_some_and(|c| c.is_i64() || c.is_u64())
}

/// Bolt's own poll shape: `{code, message, data:{vehicles:{…}}}` — the vehicles ride in
/// `taxi` (grouped by the zone-specific `icon_id`), rental bikes/scooters in `rent`, the
/// display names and groups in `category_details`, and the per-class artwork URLs in
/// `icons` (the CDN links the runtime downloads once per id and draws as the real pins).
/// `code != 0` is the vendor's own refusal and reads as the error it is — never as an
/// empty street.
pub fn parse_poll(bytes: &[u8]) -> Result<Vec<Vehicle>, String> {
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
                    id: item.get("id").and_then(crate::feed::as_text).unwrap_or_else(|| "bolt".into()),
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

/// "boda_boda" → "Boda Boda" — the vendor sends category keys lowercased; people read titles.
pub fn prettify_category(key: &str) -> String {
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
