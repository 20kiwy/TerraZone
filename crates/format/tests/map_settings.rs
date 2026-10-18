//! The map's basemap style and pinned tile layers survive a save → load exactly, and files saved
//! before tiles could be pinned still open with the defaults.
// Integration tests: unwrapping and panicking on failure is fine here, unlike in shipped code (AGENTS.md › Robustness).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use terrazon_doc::Document;
use terrazon_format::{SaveOptions, load, load_file, save_with};
use terrazon_map::{MapSettings, TileLayer, TileStyle};

/// A map document styled like the Tiles panel would leave it: a styled basemap and two pinned
/// tiles, one renamed, one restyled and half-transparent.
fn styled_map_document() -> Document {
    let mut d = Document::new(terrazon_map::WORLD_PT, terrazon_map::WORLD_PT);
    d.map.style = TileStyle { brightness: 15, contrast: -30, saturation: 80, hue: -45 };
    d.map.tile_layers = vec![
        TileLayer::new("tile-1".into(), "osm".into(), 4, 8, 8, None),
        TileLayer {
            opacity: 0.5,
            style: TileStyle { saturation: -100, ..TileStyle::default() },
            ..TileLayer::new("tile-2".into(), "esri-dark".into(), 3, 2, 5, Some("Ridge"))
        },
    ];
    d
}

#[test]
fn map_style_and_tile_layers_survive_a_save() {
    let d = styled_map_document();
    let bytes = save_with(&d, &SaveOptions::default()).unwrap();
    let back = load(&bytes).unwrap();
    assert_eq!(back.map.style, d.map.style);
    assert_eq!(back.map.tile_layers, d.map.tile_layers);
    // Byte-exact on those fields: re-saving the loaded document writes the same map members.
    let again: Value = serde_json::from_slice(&save_with(&back, &SaveOptions::default()).unwrap()).unwrap();
    let first: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(first["document"]["map"], again["document"]["map"]);
    let tile = &again["document"]["map"]["tileLayers"][1];
    assert_eq!((tile["id"].as_str(), tile["name"].as_str(), tile["opacity"].as_f64()), (Some("tile-2"), Some("Ridge"), Some(0.5)));
}

#[test]
fn a_document_without_tile_layers_opens_with_the_defaults() {
    // A document saved before tiles could be pinned has neither member in its `map` (serde wrote
    // the fields only once they existed): it opens with the plain basemap it always drew.
    let d = Document::new(600.0, 800.0);
    let mut v: Value = serde_json::from_slice(&save_with(&d, &SaveOptions::default()).unwrap()).unwrap();
    let map = v["document"]["map"].as_object_mut().unwrap();
    map.remove("style");
    map.remove("tileLayers");
    let back = load(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(back.map, MapSettings::default());
    // So does a wholly pre-map file (no `map` at all), which the loader opens without tiles.
    let mut v: Value = serde_json::from_slice(&save_with(&d, &SaveOptions::default()).unwrap()).unwrap();
    v["document"].as_object_mut().unwrap().remove("map");
    let f = load_file(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(!f.doc.map.visible, "a pre-map file opens as the picture it saved");
    assert_eq!(f.doc.map.style, TileStyle::default());
    assert!(f.doc.map.tile_layers.is_empty());
}
