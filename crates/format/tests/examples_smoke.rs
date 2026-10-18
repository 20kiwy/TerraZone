//! The shipped example files open, and the pre-map ones open without tiles: they are
//! illustrations on artboards, not documents over the map (the map release's loader keeps them
//! that way, and a world-square document keeps its tiles).
use terrazon_format::{load, sniff};

#[test]
fn shipped_examples_open() {
    for f in ["dusk-poster", "feature-sheet", "neon-drive", "perspective-city", "ribbons"] {
        let bytes = std::fs::read(format!("../../examples/{f}.terrazon")).unwrap();
        assert!(sniff(&bytes), "{f} is sniffed as native");
        let doc = load(&bytes).unwrap();
        assert!(!doc.layers.is_empty(), "{f} has layers");
        assert!(doc.node_count() > 1, "{f} has art");
        // Pre-map files open as the pictures they are: the basemap starts hidden.
        assert!(!doc.map.visible, "{f} opens without tiles");
    }
}
