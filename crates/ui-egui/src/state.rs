//! Serializable UI state (read and driven by the control channel).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use terrazon_geom::Point;

use crate::theme::Brightness;

/// Per-document view (camera).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct View {
    /// Screen points per document point (1.0 = 100%).
    pub zoom: f64,
    /// Document point at the centre of the canvas.
    pub center: Point,
    /// False until the view was fitted to the artboard on first display.
    pub fitted: bool,
    /// View rotation in degrees (Rotate View tool).
    pub rotation: f64,
    /// The artboard the status bar's navigator is on (an index): Fit Artboard in Window and Actual
    /// Size show it.
    #[serde(default)]
    pub artboard: usize,
    /// View › 3D Environment: the canvas area renders the scene3d viewport (DEM terrain, imagery
    /// and placed assets) instead of the 2D map/paper. Per document: the dual canvas.
    #[serde(default)]
    pub mode3d: bool,
}

impl Default for View {
    fn default() -> Self {
        Self { zoom: 1.0, center: Point::new(306.0, 396.0), fitted: false, rotation: 0.0, artboard: 0, mode3d: false }
    }
}

impl View {
    /// The view a document opens at: the one it was saved with, else fitted on first display.
    pub fn of(st: &terrazon_engine::DocState) -> Self {
        match &st.view {
            Some(v) if v.zoom.is_finite() && v.zoom > 0.0 && v.center.x.is_finite() && v.center.y.is_finite() => {
                // A map's saved view reaches from the whole world to street level.
                let (zmin, zmax) = if st.doc.map.visible { (0.002, 65_536.0) } else { (0.0313, 640.0) };
                Self {
                    zoom: v.zoom.clamp(zmin, zmax),
                    center: v.center,
                    fitted: true,
                    rotation: if v.rotation.is_finite() { v.rotation } else { 0.0 },
                    artboard: 0,
                    mode3d: false,
                }
            }
            _ => Self::default(),
        }
    }
}

/// Illustrator's preset zoom stops, in percent. The ladder continues below 3.13 % down to 0.2 %
/// (a map's whole-world view; non-map documents still clamp at 3.13 % in the view) and past
/// 64000 % reaches a map's street level (non-map documents still clamp at 640×).
pub const ZOOM_STOPS: [f64; 37] = [
    0.2, 0.28, 0.41, 0.62, 0.93, 1.39, 2.08, 3.13, 4.17, 6.25, 8.33, 12.5, 16.67, 25.0, 33.33, 50.0, 66.67, 100.0, 150.0, 200.0, 300.0, 400.0, 600.0,
    800.0, 1200.0, 1600.0, 2400.0, 3200.0, 4800.0, 6400.0, 12800.0, 25600.0, 64000.0, 128000.0, 512000.0, 1600000.0, 6400000.0,
];

pub fn next_zoom(z: f64, up: bool) -> f64 {
    let pct = z * 100.0;
    let stop = if up {
        // Past the last stop (street level) the ladder stays: the view clamps for non-map documents.
        ZOOM_STOPS.iter().find(|s| **s > pct + 0.01).copied().unwrap_or(ZOOM_STOPS[ZOOM_STOPS.len() - 1])
    } else {
        ZOOM_STOPS.iter().rev().find(|s| **s < pct - 0.01).copied().unwrap_or(ZOOM_STOPS[0])
    };
    stop / 100.0
}

/// Format like Illustrator's tab/zoom field: up to 2 decimals, no trailing zeros.
pub fn zoom_label(z: f64) -> String {
    let s = format!("{:.2}", z * 100.0);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    format!("{s}%")
}

/// The Web-Mercator zoom level of a view scale: the map's tiles are picked with
/// `z = REF_ZOOM + log2(scale)` (`terrazon_map::tiles_for_view`), so `z4` is 100 %, `z0` the whole
/// world and `z19` street level.
pub fn map_zoom_level(z: f64) -> f64 {
    terrazon_map::REF_ZOOM as f64 + z.max(1e-12).log2()
}

/// Zoom shown the map way: the Web-Mercator level first, then the scale (`z4 · 100%`).
pub fn zoom_label_map(z: f64) -> String {
    let lvl = format!("{:.2}", map_zoom_level(z));
    let lvl = lvl.trim_end_matches('0').trim_end_matches('.');
    format!("z{lvl} · {}", zoom_label(z))
}

/// Does the active document show the Web-Mercator map (so zoom reads as map levels)?
pub fn is_map_doc(app: &crate::TerrazoneApp) -> bool {
    app.session.active().is_some_and(|st| st.doc.map.visible)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockTab {
    #[default]
    Properties,
    Layers,
    Libraries,
    Tiles,
}

impl DockTab {
    /// The tab's panel id (`window.panel`), English label and icon (when the dock is collapsed).
    pub fn info(self) -> (&'static str, &'static str, &'static str) {
        match self {
            DockTab::Properties => ("properties", "Properties", "dc-options"),
            DockTab::Layers => ("layers", "Layers", "layers"),
            DockTab::Libraries => ("libraries", "Libraries", "library"),
            DockTab::Tiles => ("tiles", "Tiles", "map"),
        }
    }

    pub const ALL: [DockTab; 4] = [DockTab::Properties, DockTab::Layers, DockTab::Libraries, DockTab::Tiles];

    /// The tab whose panel id is `id`.
    pub fn from_id(id: &str) -> Option<DockTab> {
        DockTab::ALL.into_iter().find(|t| t.info().0 == id)
    }
}

/// Every panel `window.panel` shows, as (id, English label): the dock tabs, then the icon panels.
pub fn all_panels() -> impl Iterator<Item = (&'static str, &'static str)> {
    let tabs = DockTab::ALL.into_iter().map(|t| (t.info().0, t.info().1));
    tabs.chain(ICON_PANELS.iter().map(|&(id, label, _)| (id, label)))
}

/// Panels that live as collapsed icons in the dock (Essentials Classic).
pub const ICON_PANELS: &[(&str, &str, &str)] = &[
    ("color", "Color", "palette"),
    ("colorGuide", "Color Guide", "dc-color-guide"),
    ("swatches", "Swatches", "swatch-book"),
    ("brushes", "Brushes", "paintbrush"),
    ("symbols", "Symbols", "spray-can"),
    ("patternOptions", "Pattern Options", "grid-3x3"),
    ("stroke", "Stroke", "dc-stroke"),
    ("gradient", "Gradient", "dc-gradient"),
    ("transparency", "Transparency", "dc-transparency"),
    ("appearance", "Appearance", "dc-appearance"),
    ("graphicStyles", "Graphic Styles", "dc-graphic-styles"),
    ("artboards", "Artboards", "dc-artboards"),
    ("transform", "Transform", "dc-transform-panel"),
    ("align", "Align", "dc-align"),
    ("pathfinder", "Pathfinder", "dc-pathfinder"),
    ("character", "Character", "type"),
    ("paragraph", "Paragraph", "pilcrow"),
    ("glyphs", "Glyphs", "text-cursor-input"),
    ("charStyles", "Character Styles", "type"),
    ("openType", "OpenType", "dc-touch-type"),
    ("paraStyles", "Paragraph Styles", "pilcrow"),
    ("tabs", "Tabs", "align-horizontal-justify-start"),
    ("history", "History", "history"),
    ("actions", "Actions", "dc-actions"),
    ("info", "Info", "info"),
    ("docInfo", "Document Info", "dc-list-view"),
    ("navigator", "Navigator", "map"),
    ("separations", "Separations Preview", "printer"),
    ("imageTrace", "Image Trace", "image"),
    ("magicWand", "Magic Wand", "wand-sparkles"),
    (crate::panels::flattener_preview::ID, "Flattener Preview", "eye"),
    ("attributes", "Attributes", "settings"),
    ("colorThemes", "Color Themes", "sun"),
    (crate::panels::links::ID, "Links", "link"),
    (crate::panels::asset_export::ID, "Asset Export", "share-2"),
    (crate::panels::css_properties::ID, "CSS Properties", "globe"),
    (crate::panels::terrain3d::ID, "3D Terrain", "globe"),
    (crate::panels::camera3d::ID, "3D Camera", "rotate-3d"),
    (crate::panels::fleet::ID, "Fleet", "globe"),
];

/// Groups of icon panels separated by dividers in the collapsed column.
pub const ICON_PANEL_GROUPS: &[&[&str]] = &[
    &["color", "colorGuide"],
    &["swatches", "brushes", "symbols", "patternOptions"],
    &["stroke", "gradient", "transparency"],
    &["appearance", "graphicStyles"],
    &["artboards"],
    &["transform", "align", "pathfinder"],
    &["character", "paragraph", "glyphs"],
    &["history", "actions", "info", "navigator"],
    &["terrain3d", "camera3d"],
    &["fleet"],
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewFlags {
    pub rulers: bool,
    pub grid: bool,
    pub guides: bool,
    pub smart_guides: bool,
    pub bounding_box: bool,
    pub outline: bool,
    pub pixel_preview: bool,
    pub snap_to_grid: bool,
    pub snap_to_point: bool,
    pub artboards: bool,
    pub edges: bool,
    /// View → Trim View: hide everything outside the artboards.
    pub trim_view: bool,
    /// View → Hide/Show Corner Widget (live corner widgets on rectangles).
    pub corner_widgets: bool,
    /// View → Snap to Pixel.
    pub snap_to_pixel: bool,
    /// View → Show/Hide Text Threads.
    pub text_threads: bool,
    /// View → Show/Hide Gradient Annotator.
    pub gradient_annotator: bool,
    /// Type → Show Hidden Characters.
    pub hidden_chars: bool,
}

impl Default for ViewFlags {
    fn default() -> Self {
        Self {
            rulers: false,
            grid: false,
            guides: true,
            smart_guides: true,
            bounding_box: true,
            outline: false,
            pixel_preview: false,
            snap_to_grid: false,
            snap_to_point: true,
            artboards: true,
            edges: true,
            trim_view: false,
            corner_widgets: true,
            snap_to_pixel: false,
            text_threads: true,
            gradient_annotator: true,
            hidden_chars: false,
        }
    }
}

/// An open modal dialog. `fields` are string-valued so agents can set them uniformly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dialog {
    pub kind: String,
    pub fields: serde_json::Map<String, Value>,
}

impl Dialog {
    pub fn new(kind: &str, fields: Value) -> Self {
        Self { kind: kind.to_string(), fields: fields.as_object().cloned().unwrap_or_default() }
    }
    pub fn f64(&self, k: &str, d: f64) -> f64 {
        match self.fields.get(k) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(d),
            Some(Value::String(s)) => terrazon_doc::Unit::Points.parse(s).unwrap_or(d),
            _ => d,
        }
    }
    pub fn str(&self, k: &str) -> String {
        match self.fields.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => String::new(),
        }
    }
    pub fn bool(&self, k: &str) -> bool {
        self.fields.get(k).and_then(Value::as_bool).unwrap_or(false)
    }
}

/// The Fleet panel's in-progress sign-in state (transient; the doc's `fleet` holds the truth,
/// the vault holds the secrets). Bolt signs in with a phone number and the 4-digit code the
/// vendor sends — the draft walks those two steps.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FleetDraft {
    /// The phone number the 4-digit code goes to (step one).
    pub phone: String,
    /// The 4-digit code (step two).
    pub code: String,
    /// Which step the editor shows.
    pub stage: OtpStage,
    /// The pending session the code confirms (set between the two steps).
    #[serde(skip)]
    pub pending: Option<terrazon_fleet::PendingSession>,
    /// The last auth failure, shown under the editor.
    pub error: Option<String>,
    pub endpoint: Option<String>,
    /// The SGR seat picker: which train's coaches are expanded (its `tripsNo`).
    #[serde(skip)]
    pub sgr_train: Option<String>,
    /// The seat selection's landing: the coach the click chose (`tripsNo`, coach id) — the
    /// flow ends there (booking needs the vendor's own login; TerraZone stops honestly).
    #[serde(skip)]
    pub sgr_coach: Option<(String, i64)>,
    /// The SGR tree: whether the expanded train's coach subtree is open (the chain rows
    /// always show; the coaches fold like the layers panel's groups).
    #[serde(skip)]
    pub sgr_cars_open: bool,
    /// The Waze roads tree: which road type's group is expanded (its Waze road-type code).
    #[serde(skip)]
    pub waze_open: Option<u8>,
}

/// The two steps of the phone-code sign-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OtpStage {
    /// Typing the phone number; the button sends the code.
    #[default]
    Phone,
    /// The code is on its way; the editor wants the 4 digits.
    Code,
}

/// The credentials vault: the app-level store (saved with the settings, `ui.json`) where
/// Bolt's session and the system-minted cookies outlive every document. Rows are keyed by
/// `(service, name)` — the same keys the flows' [`terrazon_fleet::Credential`] name
/// (`bolt/session`, `marinetraffic/cf_cookie`) plus the flow's own extras (`bolt/device`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetVault {
    entries: std::collections::BTreeMap<String, String>,
}

impl FleetVault {
    pub fn get(&self, service: &str, name: &str) -> Option<&str> {
        self.entries.get(&format!("{service}/{name}")).map(String::as_str)
    }

    pub fn put(&mut self, service: &str, name: &str, secret: String) {
        self.entries.insert(format!("{service}/{name}"), secret);
    }

    pub fn remove(&mut self, service: &str, name: &str) -> Option<String> {
        self.entries.remove(&format!("{service}/{name}"))
    }

    /// The rows of one service whose name starts with `prefix` — the vault's
    /// `(service, name)` keys are flat strings, and the app keeps structured rows in it
    /// (Bolt's downloaded vehicle artwork rides as `icon:<url>` entries).
    pub fn prefixed(&self, service: &str, prefix: &str) -> Vec<(String, String)> {
        let head = format!("{service}/{prefix}");
        self.entries.range(head.clone()..).take_while(|(k, _)| k.starts_with(&head)).map(|(k, v)| (k[head.len()..].to_string(), v.clone())).collect()
    }

    /// Whether any row is kept (the settings line that says a vault exists).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// The interface language older versions saved here (`ja`, `cs`…). It now lives in the
    /// `interfaceLanguage` preference, which [`crate::prefs_dialog::restore`] carries it over to;
    /// it is never written back.
    #[serde(rename = "language", skip_serializing)]
    pub legacy_language: Option<String>,
    pub brightness: Brightness,
    pub dock_tab: DockTab,
    /// The dock's tabbed group (Properties | Layers | Libraries) is collapsed to icons at the top of
    /// the icon column (the dock's double arrow, `window.collapseDock`).
    #[serde(default)]
    pub dock_collapsed: bool,
    /// Icon panel currently popped out of the collapsed column (also `properties`, `layers` or
    /// `libraries` while the dock is collapsed).
    pub open_panel: Option<String>,
    pub control_bar: bool,
    pub toolbar: bool,
    pub toolbar_double: bool,
    /// Advanced toolbar (every tool group) instead of the categorized Basic toolbar.
    #[serde(default)]
    pub toolbar_advanced: bool,
    #[serde(default = "yes")]
    pub task_bar: bool,
    /// Where the Contextual Task Bar was dragged or pinned. Not saved, as in Illustrator: the bar
    /// starts under the selection at every launch.
    #[serde(skip)]
    pub task_bar_place: TaskBarPlace,
    /// Last tool shown in each toolbar slot (keyed by the slot's first tool id).
    #[serde(default)]
    pub slot_tool: std::collections::BTreeMap<String, String>,
    /// Tool flyouts torn off the toolbar into floating panels.
    #[serde(default)]
    pub floating_flyouts: Vec<FloatingFlyout>,
    /// Panels dragged out of the dock: each group floats as its own stack of tabs.
    #[serde(default)]
    pub floating_panels: Vec<FloatingPanels>,
    /// The Tools panel floats with its top-left corner here (dragged out by its title bar); `None`:
    /// docked at the window's left edge.
    #[serde(default)]
    pub toolbar_pos: Option<[f32; 2]>,
    pub status_bar: bool,
    pub dock: bool,
    pub view: ViewFlags,
    pub dialog: Option<Dialog>,
    /// Toolbar group whose flyout is open.
    pub flyout: Option<usize>,
    /// Last tool shown for each toolbar group (flyout selection sticks).
    pub group_tool: Vec<String>,
    pub status: String,
    pub palette_open: bool,
    pub palette_query: String,
    /// Screen mode: 0 normal, 1 full screen with menu, 2 full screen, 3 presentation. Not saved:
    /// the app always starts in Normal Screen Mode, with its menus and panels (#472: a saved
    /// Presentation Mode came back on restart with no way out).
    #[serde(skip)]
    pub screen_mode: u8,
    /// Draw Normal / Behind / Inside.
    pub draw_mode: u8,
    pub about: bool,
    /// Recorded actions (Actions panel), persisted with the UI preferences.
    #[serde(default = "crate::panels::actions::default_sets")]
    pub action_sets: Vec<crate::panels::actions::ActionSet>,
    /// Recording in progress: (set index, action name, journal length when recording started).
    #[serde(skip)]
    pub recording: Option<(usize, String, usize)>,
    /// Keyboard shortcut overrides: command id or `tool:<id>` → chord ("" = none).
    #[serde(default)]
    pub shortcut_overrides: std::collections::BTreeMap<String, String>,
    /// Name of the shortcut set (preset or "Custom").
    #[serde(default = "default_shortcut_set", deserialize_with = "crate::shortcut_editor::deserialize_set_name")]
    pub shortcut_set: String,
    /// Current workspace (Window → Workspace).
    #[serde(default = "default_workspace")]
    pub workspace: String,
    /// User-saved workspaces (New Workspace…).
    #[serde(default)]
    pub custom_workspaces: Vec<crate::workspaces::Workspace>,
    /// File → Open Recent Files, most recent first.
    #[serde(default)]
    pub recent_files: Vec<String>,
    /// Type → Recent Fonts, most recent first.
    #[serde(default)]
    pub recent_fonts: Vec<String>,
    /// Families starred in the font menus (the ★ filter shows only these).
    #[serde(default)]
    pub favorite_fonts: Vec<String>,
    /// Engine preferences (Edit → Preferences), persisted alongside the UI state.
    #[serde(default)]
    pub engine_prefs: Value,
    /// Color Guide panel: variation kind, steps and amount (Color Guide Options).
    #[serde(default)]
    pub color_guide: terrazon_color::harmony::GuideOptions,
    /// The library open in the library panel (Window → Swatch Libraries).
    #[serde(default)]
    pub library_panel: Option<crate::panels::library_panel::OpenLibrary>,
    /// Flattener Preview panel: highlight, overprints, preset and options (`ui.flattenerPreview`).
    #[serde(default)]
    pub flattener_preview: crate::panels::flattener_preview::Settings,
    /// Color Guide panel: Limit to Library, a swatch library id, "document" (the document's
    /// swatches) or "" (none) (`ui.colorGuideLimit`).
    #[serde(default)]
    pub color_guide_limit: String,
    /// The SVG Options chosen last (`svg` object of the export/save commands; null: never used).
    #[serde(default)]
    pub svg_options: Value,
    /// File → Place: Link is on (the Place dialog remembers it).
    #[serde(default = "yes")]
    pub place_link: bool,
    /// The file the open dialog reads, kept out of its JSON fields (Import PDF); dropped when no
    /// dialog is open.
    #[serde(skip)]
    pub dialog_file: Option<std::sync::Arc<crate::dialogs::import_pdf::DialogFile>>,
    /// The DXF Options chosen last (`document.exportDxf` options; null: never used).
    #[serde(default)]
    pub dxf_options: Value,
    /// The EPS Options chosen last (`document.exportEps` options; null: never used).
    #[serde(default)]
    pub eps_options: Value,
    /// The DXF Import Options chosen last (fit, scaleLineweights, center, mergeLayers; null:
    /// never used).
    #[serde(default)]
    pub dxf_import: Value,
    /// The Home screen is shown over the open documents (`app.home`): the active document's uid
    /// and the document count when it opened. Choosing a tab, or a document opening, closing or
    /// becoming active, leaves it.
    #[serde(skip)]
    pub home: Option<(Option<u64>, usize)>,
    /// The Tiles panel's Add Connection form is open (not saved: a form, not a setting).
    #[serde(skip)]
    pub tiles_form: bool,
    /// The Add Connection form's draft, kept while the form opens and closes.
    #[serde(skip)]
    pub tiles_draft: crate::panels::tiles_panel::TilesDraft,
    /// The Tiles panel's open tab (the source list, Style or Edit), as it was last left (transient).
    #[serde(skip)]
    pub tiles_tab: crate::panels::tiles_panel::TilesPanelTab,
    /// What the Tiles panel's Edit tab shows: a tile source or a pinned tile layer, by id (the
    /// row pencil opens it; transient).
    #[serde(skip)]
    pub tile_edit: Option<crate::panels::tiles_panel::TileEdit>,
    /// The pinned tile layer whose data editor is expanded in the Tiles panel (transient).
    #[serde(skip)]
    pub tile_layer_open: Option<String>,
    /// The pinned tile layers picked in the Tiles and Layers panels (transient; Delete unpins
    /// them, like the art selection's Delete).
    #[serde(skip)]
    pub tile_layers_sel: Vec<String>,
    /// The tile disk cache's last reading — tiles, bytes, when — shown in the Tiles panel; the
    /// walk is re-run when the line has been stale for a while or after a Clear (transient).
    #[serde(skip)]
    pub tile_cache_info: Option<(u64, u64, std::time::Instant)>,
    /// The Fleet panel's selected feed (its editor is expanded; transient).
    #[serde(skip)]
    pub fleet_sel: Option<String>,
    /// The Fleet panel's sign-in drafts (what the fields hold between frames; transient).
    #[serde(skip)]
    pub fleet_draft: FleetDraft,
    /// The credentials vault — Bolt's session and the system cookies live in the app's
    /// settings, outliving every document (a fresh document re-attaches the saved session).
    #[serde(default)]
    pub fleet_vault: FleetVault,
    /// The deployment's auto-trust was tried once this run (the environment's Bolt pair
    /// seeds the vault on the first tick; a deliberate sign-out stays out until relaunch).
    #[serde(skip)]
    pub fleet_trust_tried: bool,
    /// Layers panel › Panel Options… (row size, thumbnails, Show Layers Only).
    #[serde(default)]
    pub layers_panel: crate::panels::layers::PanelOptions,
    /// The Image Trace panel's Advanced section is open (as it was last left).
    #[serde(default = "yes")]
    pub image_trace_advanced: bool,
    /// The desktop window's size, position and maximized state, saved when the app quits and
    /// restored at the next launch (the desktop host reads and writes it; none on the web).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowGeometry>,
    /// The 3D environment's terrain build settings (the `scene3d.set` command writes them; the
    /// 3D Terrain panel edits them). Saved with the UI.
    #[serde(default)]
    pub terrain3d: terrazon_scene3d::TerrainSettings,
    /// The 3D environment's orbit camera ([`terrazon_scene3d::OrbitCamera`]; not serializable, so
    /// it starts at the default framing every launch and is shared by the documents).
    #[serde(skip)]
    pub cam3d: terrazon_scene3d::OrbitCamera,
    /// The 3D environment's camera mode — the EffectCraft viewer's pair: the orbit arm or the
    /// first-person walk rig. Both cameras live here ([`Self::cam3d`] above, [`Self::walk3d`]
    /// below) so switching modes hands the view over instead of resetting it.
    #[serde(skip)]
    pub cam_mode: terrazon_scene3d::CamMode,
    /// The walk rig ([`terrazon_scene3d::WalkCam`], the EffectCraft camera): WASD moves it,
    /// drag looks, Q/E or the wheel lifts. Starts at the scene centre eye-high.
    #[serde(skip)]
    pub walk3d: terrazon_scene3d::WalkCam,
}

/// The desktop window's geometry, kept across launches.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    /// Top-left corner of the window frame, in physical pixels on the desktop (none where the
    /// system doesn't tell windows where they are).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[i32; 2]>,
    /// Size of the window's contents in logical pixels (points at 100% UI scaling).
    pub size: [f32; 2],
    /// The window was maximized; `pos` and `size` are where un-maximizing puts it.
    #[serde(default)]
    pub maximized: bool,
}

/// A tool group's flyout torn off the toolbar: it floats as its own panel until its × puts it back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FloatingFlyout {
    /// The group's tools in flyout order; the first one names the toolbar slot.
    pub tools: Vec<String>,
    /// Top-left corner in screen points.
    pub pos: [f32; 2],
}

/// Where the Contextual Task Bar sits once its handle has moved it (`window.taskBar.pin`,
/// `window.taskBar.reset`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TaskBarPlace {
    /// More Options › Pin Bar Position: the bar stays where it is instead of following the selection.
    pub pinned: bool,
    /// Where a pinned bar's top-left corner sits from the canvas's top-left (none when it was
    /// pinned before it ever showed, until it is drawn). Unpinning leaves it for the next frame
    /// to turn into `offset`.
    pub pin_at: Option<egui::Vec2>,
    /// Where the bar was last drawn, from the canvas's top-left: where pinning holds it.
    pub shown_at: Option<egui::Vec2>,
    /// How far an unpinned bar was dragged from its place under the selection, which it keeps while
    /// it follows the selection, and the document (`DocState::uid`) it was moved in: in another
    /// document the bar starts under the selection again.
    pub offset: Option<(u64, egui::Vec2)>,
}

/// A group of panels dragged out of the dock: it floats as a stack of tabs, one panel shown.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FloatingPanels {
    /// Panel ids (`window.panel`), in tab order.
    pub panels: Vec<String>,
    /// The tab shown.
    #[serde(default)]
    pub active: usize,
    /// Top-left corner in screen points.
    pub pos: [f32; 2],
    /// A width the group was given (the Tabs panel sized over its text), else its panels' own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
}

impl FloatingPanels {
    /// Keep known panels, each in one group, drop the groups left empty and the toolbar position
    /// that isn't a number (a hand-edited preferences file or workspace).
    pub fn sanitize(groups: &mut Vec<FloatingPanels>, toolbar_pos: &mut Option<[f32; 2]>) {
        let mut seen = std::collections::BTreeSet::new();
        groups.retain_mut(|g| {
            g.panels.retain(|id| all_panels().any(|(p, _)| p == id) && seen.insert(id.clone()));
            g.active = g.active.min(g.panels.len().saturating_sub(1));
            !g.panels.is_empty()
        });
        if toolbar_pos.is_some_and(|p| !p.iter().all(|v| v.is_finite())) {
            *toolbar_pos = None;
        }
    }
}

impl UiState {
    /// Clear transient state after loading saved preferences.
    pub fn sanitized(mut self) -> Self {
        self.dialog = None;
        self.flyout = None;
        self.palette_open = false;
        self.status.clear();
        self.about = false;
        self.open_panel = None;
        if self.group_tool.len() != terrazon_tools::TOOL_GROUPS.len() {
            self.group_tool = UiState::default().group_tool;
        }
        // One strip per group, of known tools (a hand-edited preferences file).
        let mut seen = std::collections::BTreeSet::new();
        self.floating_flyouts.retain_mut(|f| {
            f.tools.retain(|id| terrazon_tools::tool_info(id).is_some());
            f.tools.first().is_some_and(|k| seen.insert(k.clone()))
        });
        FloatingPanels::sanitize(&mut self.floating_panels, &mut self.toolbar_pos);
        // Overrides that can't fire (modifier-only chords recorded by older versions, #487) give
        // the default back.
        self.shortcut_overrides.retain(|_, c| c.is_empty() || crate::shortcut_editor::normalize(c).is_some());
        self
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            legacy_language: None,
            brightness: Brightness::MediumDark,
            dock_tab: DockTab::Properties,
            dock_collapsed: false,
            open_panel: None,
            control_bar: false,
            toolbar: true,
            toolbar_double: false,
            toolbar_advanced: false,
            task_bar: true,
            task_bar_place: TaskBarPlace::default(),
            slot_tool: Default::default(),
            floating_flyouts: vec![],
            floating_panels: vec![],
            toolbar_pos: None,
            status_bar: true,
            dock: true,
            view: ViewFlags::default(),
            dialog: None,
            flyout: None,
            group_tool: terrazon_tools::TOOL_GROUPS.iter().map(|g| g[0].id.to_string()).collect(),
            status: String::new(),
            palette_open: false,
            palette_query: String::new(),
            screen_mode: 0,
            draw_mode: 0,
            about: false,
            action_sets: crate::panels::actions::default_sets(),
            recording: None,
            shortcut_overrides: Default::default(),
            shortcut_set: default_shortcut_set(),
            workspace: default_workspace(),
            custom_workspaces: vec![],
            recent_files: vec![],
            recent_fonts: vec![],
            favorite_fonts: vec![],
            engine_prefs: Value::Null,
            color_guide: Default::default(),
            library_panel: None,
            flattener_preview: Default::default(),
            color_guide_limit: String::new(),
            svg_options: Value::Null,
            place_link: true,
            dialog_file: None,
            dxf_options: Value::Null,
            eps_options: Value::Null,
            dxf_import: Value::Null,
            home: None,
            tiles_form: false,
            tiles_draft: Default::default(),
            tiles_tab: Default::default(),
            tile_edit: None,
            tile_layer_open: None,
            tile_layers_sel: vec![],
            tile_cache_info: None,
            fleet_sel: None,
            fleet_vault: FleetVault::default(),
            fleet_trust_tried: false,
            fleet_draft: Default::default(),
            layers_panel: Default::default(),
            image_trace_advanced: true,
            window: None,
            terrain3d: Default::default(),

            cam3d: Default::default(),
            cam_mode: Default::default(),
            walk3d: Default::default(),
        }
    }
}

fn yes() -> bool {
    true
}

fn default_shortcut_set() -> String {
    crate::shortcut_editor::PRESETS[0].to_string()
}

fn default_workspace() -> String {
    crate::workspaces::ESSENTIALS.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps() {
        assert_eq!(next_zoom(1.0, true), 1.5);
        assert!((next_zoom(1.0, false) - 0.6667).abs() < 1e-9);
        // The stops go on past 640x (street level for map documents); non-map documents clamp
        // at 640x in the view (View::of), not here. At either end the ladder stays.
        assert_eq!(next_zoom(640.0, true), 1280.0);
        assert_eq!(next_zoom(64000.0, true), 64000.0);
        // 640% (a non-map document's ceiling) still steps up the same way.
        assert_eq!(next_zoom(6.4, true), 8.0);
        // The ladder now continues below 3.13% down to a map's whole-world 0.2% (0.002×): a
        // non-map document still clamps at 3.13% in the view, so these stops only serve maps.
        assert_eq!(next_zoom(0.0313, false), 0.0208);
        assert_eq!(next_zoom(0.0208, false), 0.0139);
        // 0.2% is the ladder's floor: stepping down from the whole world stays there.
        assert_eq!(next_zoom(0.002, false), 0.002);
        assert_eq!(zoom_label(0.6667), "66.67%");
        assert_eq!(zoom_label(1.0), "100%");
        assert_eq!(zoom_label(0.125), "12.5%");
    }

    #[test]
    fn dialog_fields() {
        let d = Dialog::new("rectangle", serde_json::json!({"width": "1in", "height": 50}));
        assert_eq!(d.f64("width", 0.0), 72.0);
        assert_eq!(d.f64("height", 0.0), 50.0);
    }
}
// PERSIST-TEST-1723
