//! TerraZone Fleet: the live fleet layer — ride-hailing (Bolt), marine radar and flight radar
//! feeds tiled by **H3** (Uber's hexagonal indexing, via `h3o`), the same layer engine as the
//! map: a "Fleet" group sits on top of the "Map" group in the document, one locked sub-layer per
//! configured provider, and every marker draws from that one source of truth on the 2D map and in
//! the 3D environment alike.
//!
//! Data-driven, like the tiles: a provider only shows when its status says it has data — a feed
//! nobody signed into stays a dark row, never fake markers. The crate is network-free (like
//! `terrazon-map`): it builds requests, parses answers and gates statuses; the HTTP lives in the
//! app crate behind [`FleetFetch`].

use serde::{Deserialize, Serialize};

pub mod auth;
pub mod feed;
pub mod h3grid;
pub mod icons;
pub mod rail;
pub mod sizing;

pub use auth::{
    AuthFlow, AuthMethod, AuthRequest, BoltAuth, BoltSession, BoltTrust, Credential, Device, FlightAuth, MarineAuth, PendingSession,
    bolt_confirm_answer, bolt_group_kind, bolt_start_answer, bolt_type_name, flow_for, phone_country, timezone_for_country,
};
pub use rail::{RailAtlas, RailStop, SgrAuth, SgrStop, SgrTimetable, SgrTrain, SweptTrip};

/// A fleet provider: one vendor feed, one locked sub-layer under the Fleet group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Bolt ride-hailing (cars, vans, motorcycles) — signed in with a session token.
    #[default]
    Bolt,
    /// Marine vessel radar — cookie authenticated.
    Marine,
    /// Flight radar (aircraft) — cookie authenticated (optional; the public feed works bare).
    Flight,
    /// Tanzania SGR trains — the TRC TICIDIS public schedule service. No credentials, no live
    /// positions: stations and timetables, with the trains themselves placed algorithmically
    /// along the OSM rail network by the schedule times (see [`rail`]).
    Sgr,
}

impl Provider {
    pub const ALL: [Provider; 4] = [Provider::Bolt, Provider::Marine, Provider::Flight, Provider::Sgr];

    /// The name the layer panel shows (and the name of the locked sub-layer node).
    pub fn label(self) -> &'static str {
        match self {
            Provider::Bolt => "Bolt",
            Provider::Marine => "Marine Radar",
            Provider::Flight => "Flight Radar",
            Provider::Sgr => "SGR Trains",
        }
    }

    /// Parse from a command param / JSON (label or lowercase id, case-insensitive).
    pub fn parse(s: &str) -> Option<Provider> {
        let s = s.trim();
        Provider::ALL.into_iter().find(|p| p.label().eq_ignore_ascii_case(s) || format!("{p:?}").eq_ignore_ascii_case(s))
    }

    /// The marker tint, sRGB (Bolt's green, a deep marine navy, radar amber, SGR rail brick).
    pub fn color(self) -> [u8; 3] {
        match self {
            Provider::Bolt => [0x2f, 0xc6, 0x6f],
            Provider::Marine => [0x17, 0x4e, 0x63],
            Provider::Flight => [0xd9, 0x8e, 0x1f],
            Provider::Sgr => [0xb0, 0x43, 0x2a],
        }
    }

    /// The default request endpoint template, overridable per document. Placeholders:
    /// `{res}` the H3 resolution, `{cells}` the comma-separated covering cells (hex), `{z}` the
    /// map zoom level, `{bbox}` `west,south,east,north` in degrees.
    pub fn default_endpoint(self) -> &'static str {
        match self {
            Provider::Bolt => "https://fleet.bolt.eu/api/h3/{res}/{cells}",
            Provider::Marine => "https://services.marinetraffic.com/api/h3/{res}/{cells}",
            Provider::Flight => "https://data-live.flightradar24.com/zones/fcgi/feed.js?bbox={bbox}",
            // Template mode is never SGR's path (its flow builds the sweep); this names the
            // service the same way the other rows do.
            Provider::Sgr => "https://sgrticket-api.trc.co.tz/TICIDIS/api/v1/Public/SearchTrip",
        }
    }

    /// The URL the system visits to obtain this feed's session cookie by itself (a plain page
    /// load the vendor answers with `Set-Cookie`). `None` for Bolt — Bolt's credential is the
    /// user's session token, kept by the user, never generated here.
    pub fn bootstrap_url(self) -> Option<&'static str> {
        match self {
            Provider::Bolt | Provider::Sgr => None,
            Provider::Marine => Some("https://www.marinetraffic.com/en/ais/home"),
            Provider::Flight => Some("https://www.flightradar24.com/data/aircraft"),
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// How a provider authenticates. Bolt carries a session token (`Authorization: Bearer …`) that
/// the **user** keeps; the radars carry a cookie the **system** generates by itself ([`Auth::System`]
/// — TerraZone visits the vendor once, takes the `Set-Cookie` answer and rotates it when it
/// expires; nothing of this is ever typed into the UI).
/// Adjacent tagging (`{kind, value}`) — internally tagged enums can't carry a plain string.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum Auth {
    #[default]
    None,
    /// Bolt: the session token from signing in (phone + password on the vendor's side).
    Token(String),
    /// Marine / Flight: a cookie the user pasted from a signed-in browser session (still
    /// accepted — the command surface keeps it for scripts — but the UI never asks for one).
    Cookie(String),
    /// Marine / Flight: a cookie the system obtained itself from the vendor's bootstrap page.
    /// An empty string means the handshake hasn't answered yet — the row reads *Connecting*.
    System(String),
}

impl Auth {
    pub fn is_set(&self) -> bool {
        match self {
            Auth::None => false,
            Auth::Token(t) | Auth::Cookie(t) | Auth::System(t) => !t.trim().is_empty(),
        }
    }

    /// Is this a credential the system owns? True for [`Auth::System`] whether or not the
    /// handshake has landed yet.
    pub fn is_system(&self) -> bool {
        matches!(self, Auth::System(_))
    }

    /// The secret itself (empty when unset).
    pub fn secret(&self) -> &str {
        match self {
            Auth::Token(t) | Auth::Cookie(t) | Auth::System(t) => t,
            Auth::None => "",
        }
    }

    /// The request header this auth rides in: `(name, value)`.
    pub fn header(&self) -> Option<(String, String)> {
        match self {
            Auth::None => None,
            Auth::Token(t) => Some(("Authorization".into(), format!("Bearer {}", t.trim()))),
            Auth::Cookie(t) | Auth::System(t) => Some(("Cookie".into(), t.trim().to_string())),
        }
    }
}

/// A provider's data state — the gate everything shows behind. `Ready` (or a fresh error with
/// markers left over from the last good poll) is the only state that draws markers; a feed that
/// never answered draws nothing at all, the way a keyed basemap with no key stays dark.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum FeedStatus {
    /// Not configured (no auth) — the row shows what signing in would enable.
    #[default]
    Off,
    /// Auth set, the feed hasn't answered its first poll yet.
    Connecting,
    /// The last poll answered with data.
    Ready,
    /// The last poll failed (stale markers keep drawing until the next good poll — a lost
    /// packet shouldn't blink the fleet away).
    Error(String),
}

impl FeedStatus {
    /// Does this status carry data worth drawing?
    pub fn has_data(&self) -> bool {
        matches!(self, FeedStatus::Ready | FeedStatus::Error(_))
    }
}

/// One thing a feed tracks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vehicle {
    /// The vendor's own id (stable across polls, so markers move instead of respawn).
    pub id: String,
    pub kind: VehicleKind,
    pub lon: f64,
    pub lat: f64,
    /// Degrees clockwise from north; −1 = unknown (the marker then sits still).
    pub heading_deg: f32,
    /// The vendor's own length figure in metres, when it gives one — the marine sizing ladder
    /// reads this; a missing length falls back to the kind's base class.
    pub length_m: Option<f32>,
    /// A short label when the vendor gives one (callsign, registration, MMSI).
    pub name: Option<String>,
    /// The vendor's own category name for this vehicle when it gives one (Bolt's
    /// `category_details` — "Boda", "XL", "Comfort"; the SGR train's service name). Drawn
    /// under the marker at higher zooms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The vendor's artwork URL for this vehicle's class (Bolt's `icons` block): the runtime
    /// downloads it once per id, caches the PNG and draws the real pin instead of the glyph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    /// The altitude above the datum in metres, when the vendor reports one (the flight
    /// feed's alt field, feet → metres) — the 3D environment lifts the marker by it and
    /// drops a leader line to the ground, so aircraft read at height, not as road traffic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f32>,
}

impl Vehicle {
    /// A marker with only the required fields (the rest is the feed's own data).
    pub fn at(id: impl Into<String>, kind: VehicleKind, lon: f64, lat: f64, heading_deg: f32) -> Self {
        Self { id: id.into(), kind, lon, lat, heading_deg, length_m: None, name: None, category: None, icon_url: None, alt_m: None }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VehicleKind {
    #[default]
    Car,
    Motorcycle,
    Van,
    Boat,
    Ship,
    /// A narrow-body / medium airliner (the A320/B737 class) — the default plane.
    Plane,
    /// A heavy / wide-body aircraft — the B777/A330 class, freighters included.
    PlaneHeavy,
    /// A light aircraft — general aviation and small turboprops.
    PlaneLight,
    /// A rotorcraft — the type code says helicopter.
    Helicopter,
    /// A scheduled train (SGR) riding the rail network.
    Train,
}

impl VehicleKind {
    /// Whether this kind flies (the 3D environment lifts flyers by their altitude).
    pub fn flies(self) -> bool {
        matches!(self, VehicleKind::Plane | VehicleKind::PlaneHeavy | VehicleKind::PlaneLight | VehicleKind::Helicopter)
    }

    /// The marker icon outline (unit space, see [`icons`]).
    pub fn icon(self) -> &'static [(f32, f32)] {
        match self {
            VehicleKind::Car => icons::CAR,
            VehicleKind::Van => icons::VAN,
            VehicleKind::Motorcycle => icons::BIKE,
            VehicleKind::Boat | VehicleKind::Ship => icons::SHIP,
            VehicleKind::Plane => icons::PLANE,
            VehicleKind::PlaneHeavy => icons::PLANE_HEAVY,
            VehicleKind::PlaneLight => icons::PLANE_LIGHT,
            VehicleKind::Helicopter => icons::HELI,
            VehicleKind::Train => icons::TRAIN,
        }
    }
}

/// One provider's side data on the document: its auth, endpoint, live status and the markers the
/// last good poll left. The provider's locked sub-layer node lives in the layer tree; this is the
/// data that node represents (the tiles' `TileLayer` pattern).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetLayer {
    pub provider: Provider,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default)]
    pub auth: Auth,
    /// `None` = the provider's default endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub status: FeedStatus,
    #[serde(default)]
    pub markers: Vec<Vehicle>,
    /// The schedule feed's data (SGR): the merged timetable the sweep built. The trains draw
    /// by projecting it along the rail atlas at the current Dar es Salaam time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<rail::SgrTimetable>,
}

impl Default for FleetLayer {
    fn default() -> Self {
        // A new provider row is visible (the eye starts on) — hiding it is the user's move.
        Self {
            provider: Provider::default(),
            visible: true,
            auth: Auth::default(),
            endpoint: None,
            status: FeedStatus::default(),
            markers: Vec::new(),
            schedule: None,
        }
    }
}

fn yes() -> bool {
    true
}

impl FleetLayer {
    /// Status the row shows: `Off` only when there is no credential and the flow needs one
    /// (the public flight feed never reads Off); a system feed whose handshake hasn't landed
    /// yet is `Connecting` — the system is working on it.
    pub fn status(&self) -> FeedStatus {
        if matches!(self.auth, Auth::None) && auth::flow_for(self.provider).needs_credential() {
            return FeedStatus::Off;
        }
        if self.needs_bootstrap() {
            return FeedStatus::Connecting;
        }
        self.status.clone()
    }

    /// Ready to ask the network for data — a credential on file, or a feed that needs none
    /// (the public flight feed). The demo feed never asks.
    pub fn wants_fetch(&self) -> bool {
        self.auth.is_set() || !auth::flow_for(self.provider).needs_credential()
    }

    /// The system owns this feed and its handshake hasn't answered yet: the runtime sends
    /// the flow's bootstrap request, the vendor's `Set-Cookie` fills the credential, polling
    /// starts. Flows that mint nothing (Bolt, the public flight feed) never wait.
    pub fn needs_bootstrap(&self) -> bool {
        matches!(&self.auth, Auth::System(s) if s.trim().is_empty()) && auth::flow_for(self.provider).bootstrap(0).is_some()
    }
}

/// The document's whole fleet: the per-provider layers, the demo feed switch and the poll
/// cadence. Saved with the document like the map settings; `poll_due` is editing state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetState {
    #[serde(default)]
    pub layers: Vec<FleetLayer>,
    /// The demo feed: deterministic sample vehicles around the view centre, clearly labelled —
    /// for seeing the layer, the icons and the sizing work before a vendor signs in.
    #[serde(default)]
    pub demo: bool,
    /// The demo row's eye (the demo markers draw while this is on).
    #[serde(default = "yes")]
    pub demo_visible: bool,
    /// Seconds between polls.
    #[serde(default = "default_poll")]
    pub poll_secs: u32,
    /// The demo feed's markers (kept on the state, generated per poll tick).
    #[serde(default)]
    pub demo_markers: Vec<Vehicle>,
    /// Set by `fleet.poll` to ask the next frame's tick to fetch at once (editing state).
    #[serde(skip)]
    pub poll_due: bool,
}

impl Default for FleetState {
    fn default() -> Self {
        // Marine and Flight ride with every document: the system owns their sessions, so their
        // locked sub-layers sit under the Fleet group from the start (the rows the Layers panel
        // shows next to Map), the same way every document carries the Map layer. Their markers
        // still draw only once the vendors actually answer — data-driven to the end. Bolt stays
        // out until the user signs in: their credential is theirs to keep.
        let system =
            |p: Provider| FleetLayer { provider: p, auth: Auth::System(String::new()), status: FeedStatus::Connecting, ..FleetLayer::default() };
        Self {
            // SGR is public (no handshake) but still a system row: the sweep starts on frame one
            // the same way the radars' bootstraps do.
            layers: vec![system(Provider::Marine), system(Provider::Flight), system(Provider::Sgr)],
            demo: false,
            demo_visible: true,
            poll_secs: default_poll(),
            demo_markers: Vec::new(),
            poll_due: false,
        }
    }
}

fn default_poll() -> u32 {
    15
}

impl FleetState {
    pub fn layer(&self, provider: Provider) -> Option<&FleetLayer> {
        self.layers.iter().find(|l| l.provider == provider)
    }

    pub fn layer_mut(&mut self, provider: Provider) -> &mut FleetLayer {
        let idx = match self.layers.iter().position(|l| l.provider == provider) {
            Some(i) => i,
            None => {
                self.layers.push(FleetLayer { provider, ..FleetLayer::default() });
                self.layers.len() - 1
            }
        };
        &mut self.layers[idx]
    }

    pub fn remove_layer(&mut self, provider: Provider) {
        self.layers.retain(|l| l.provider != provider);
    }

    /// Is anything configured at all (a signed-in feed, a system-managed one, or the demo on)?
    /// The Fleet layer group exists in the tree while this is true — with the system-managed
    /// radars seeded into every document, that is always the case, the same way the Map layer is.
    pub fn configured(&self) -> bool {
        self.demo || self.layers.iter().any(|l| !matches!(l.auth, Auth::None))
    }

    /// Nothing configured and nothing to remember: documents skip the fleet entirely when this
    /// is true, so a plain map file carries no fleet data at all. The seeded system feeds make
    /// this false in practice — the Fleet rows are part of the document now.
    pub fn is_unconfigured(&self) -> bool {
        !self.configured() && self.marker_count() == 0
    }

    /// Providers whose markers should draw right now (status carries data and the row is on).
    pub fn live(&self) -> Vec<Provider> {
        self.layers.iter().filter(|l| l.visible && l.status().has_data() && !l.markers.is_empty()).map(|l| l.provider).collect()
    }

    pub fn marker_count(&self) -> usize {
        self.layers.iter().map(|l| l.markers.len()).sum::<usize>() + self.demo_markers_len()
    }

    fn demo_markers_len(&self) -> usize {
        if self.demo { self.demo_markers.len() } else { 0 }
    }
}

/// The host's fleet HTTP: one `get` (or `post`) per ask, answered on the channel (the
/// `DemFetcher` shape). Keyed by the request key `feed::FleetRequest::key` so the answer
/// finds its provider — `bolt/otp-start{n}` and `bolt/otp-confirm{n}` are the sign-in steps,
/// `marine-bootstrap{n}` the system's own cookie handshake, `{provider}/poll{n}` a data ask.
/// A bootstrap answer carries the vendor's `Set-Cookie` in `set_cookie`; data answers carry
/// it too when the vendor rotates sessions.
pub enum FleetMsg {
    Done { key: String, bytes: Vec<u8>, set_cookie: Option<String> },
    Failed { key: String, reason: String },
}

pub trait FleetFetch: Send + Sync {
    fn get(&self, key: String, url: String, headers: Vec<(String, String)>, tx: std::sync::mpsc::Sender<FleetMsg>);

    /// The sign-in and vehicle-poll transport (Bolt's API is POST-with-JSON). The default
    /// refuses, so a host that never implemented it fails the ask instead of hanging it.
    fn post(
        &self,
        key: String,
        _url: String,
        _headers: Vec<(String, String)>,
        _body: String,
        _content_type: String,
        tx: std::sync::mpsc::Sender<FleetMsg>,
    ) {
        let _ = tx.send(FleetMsg::Failed { key, reason: "this fetcher cannot POST".into() });
    }
}

/// Cell index as the wire format (hex string, the H3 convention).
pub fn cell_key(cell: h3o::CellIndex) -> String {
    format!("{:x}", u64::from(cell))
}

pub fn cell_from_key(key: &str) -> Option<h3o::CellIndex> {
    u64::from_str_radix(key.trim().trim_start_matches("0x"), 16).ok().and_then(|v| h3o::CellIndex::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_parse_labels_and_ids() {
        for p in Provider::ALL {
            assert_eq!(Provider::parse(p.label()), Some(p));
            assert_eq!(Provider::parse(&format!("{p:?}").to_lowercase()), Some(p));
        }
        assert_eq!(Provider::parse("BOLT"), Some(Provider::Bolt));
        assert_eq!(Provider::parse("nope"), None);
    }

    #[test]
    fn status_gates_the_draw() {
        let mut l = FleetLayer { provider: Provider::Bolt, ..FleetLayer::default() };
        assert_eq!(l.status(), FeedStatus::Off);
        assert!(!l.status().has_data(), "no auth: nothing draws");
        l.auth = Auth::Token("t".into());
        l.status = FeedStatus::Connecting;
        assert!(!l.status().has_data());
        l.status = FeedStatus::Ready;
        assert!(l.status().has_data());
        l.status = FeedStatus::Error("boom".into());
        assert!(l.status().has_data(), "stale markers keep drawing through an error");
        l.auth = Auth::None;
        assert_eq!(l.status(), FeedStatus::Off, "cleared auth forces Off");
    }

    #[test]
    fn system_auth_reads_as_connecting_until_the_handshake_lands() {
        let mut l = FleetLayer { provider: Provider::Marine, auth: Auth::System(String::new()), ..FleetLayer::default() };
        assert!(l.auth.is_system());
        assert!(l.needs_bootstrap(), "empty system credential: the handshake is pending");
        assert_eq!(l.status(), FeedStatus::Connecting, "never Off — the system is on it");
        assert!(!l.wants_fetch(), "no data poll before the cookie exists");
        l.auth = Auth::System("mstc=1".into());
        assert!(!l.needs_bootstrap());
        assert!(l.wants_fetch(), "the cookie landed: polling starts");
        // The commands set Connecting when the session lands (fleet.systemAuth); the field's
        // own default stays Off until then, exactly like a user-signed feed's.
        l.status = FeedStatus::Connecting;
        assert_eq!(l.status(), FeedStatus::Connecting, "still waiting for the first answer");
        l.status = FeedStatus::Ready;
        assert!(l.status().has_data());
    }

    #[test]
    fn every_document_carries_the_system_radars() {
        let st = FleetState::default();
        assert_eq!(st.layers.iter().map(|l| l.provider).collect::<Vec<_>>(), vec![Provider::Marine, Provider::Flight, Provider::Sgr]);
        for l in &st.layers {
            assert!(l.auth.is_system());
            assert!(l.visible, "the rows the Layers panel shows start with the eye on");
            assert_eq!(l.status(), FeedStatus::Connecting);
        }
        // Marine waits for its cookie handshake; the flight and SGR feeds are public — they
        // need no credential, so nothing is minted for them (the system just polls).
        assert!(st.layer(Provider::Marine).unwrap().needs_bootstrap());
        assert!(!st.layer(Provider::Flight).unwrap().needs_bootstrap());
        assert!(!st.layer(Provider::Sgr).unwrap().needs_bootstrap());
        assert!(st.layer(Provider::Sgr).unwrap().wants_fetch(), "the schedule sweep is public — it polls");
        assert!(st.configured(), "the system feeds make the Fleet group part of every document");
        assert!(!st.is_unconfigured(), "the rows persist with the file");
        assert!(st.live().is_empty(), "but nothing draws until a vendor answers");
    }

    #[test]
    fn auth_headers() {
        assert_eq!(Auth::None.header(), None);
        assert_eq!(Auth::Token("abc".into()).header(), Some(("Authorization".into(), "Bearer abc".into())));
        assert_eq!(Auth::Cookie("k=v; j=1".into()).header(), Some(("Cookie".into(), "k=v; j=1".into())));
        assert_eq!(Auth::System("s=9".into()).header(), Some(("Cookie".into(), "s=9".into())), "the system credential rides as a cookie too");
        assert!(!Auth::Token("   ".into()).is_set());
        assert!(Auth::System("".into()).is_system(), "system even before the handshake");
    }

    #[test]
    fn layer_mut_creates_and_reuses() {
        let mut st = FleetState::default();
        assert_eq!(st.layers.len(), 3, "marine, flight and the SGR sweep ride with the document");
        st.layer_mut(Provider::Bolt).visible = false;
        assert_eq!(st.layers.len(), 4);
        st.layer_mut(Provider::Bolt).auth = Auth::Cookie("c".into());
        assert_eq!(st.layers.len(), 4);
        assert_eq!(st.layer(Provider::Bolt).map(|l| l.auth.clone()), Some(Auth::Cookie("c".into())));
        assert!(st.configured(), "a signed-in feed is configured even while its row is hidden");
    }

    #[test]
    fn cell_keys_roundtrip() {
        let cell = h3o::LatLng::new(6.778, 39.223).unwrap().to_cell(h3o::Resolution::Seven);
        let k = cell_key(cell);
        assert_eq!(cell_from_key(&k), Some(cell));
        assert_eq!(cell_from_key("nope"), None);
    }

    #[test]
    fn configured_and_live() {
        let mut st = FleetState::default();
        assert!(st.configured(), "the system radars are always on board");
        st.demo = true;
        assert!(st.configured());
        st.demo = false;
        {
            let l = st.layer_mut(Provider::Flight);
            l.auth = Auth::Cookie("ck".into());
        }
        assert!(st.configured());
        assert!(st.live().is_empty(), "connecting with no markers: nothing draws");
        {
            let l = st.layer_mut(Provider::Flight);
            l.status = FeedStatus::Ready;
            l.markers.push(Vehicle {
                id: "a".into(),
                kind: VehicleKind::Plane,
                lon: 39.2,
                lat: -6.8,
                heading_deg: 90.0,
                length_m: None,
                name: None,
                category: None,
                icon_url: None,
                alt_m: None,
            });
        }
        assert_eq!(st.live(), vec![Provider::Flight]);
        st.layer_mut(Provider::Flight).visible = false;
        assert!(st.live().is_empty(), "hidden layer draws nothing");
    }

    #[test]
    fn fleet_state_roundtrips_through_json() {
        let st = FleetState::default();
        let json = serde_json::to_string(&st).unwrap();
        let back: FleetState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, st, "the seeded system feeds survive a save/load");
        assert!(back.layer(Provider::Marine).unwrap().auth.is_system());
        assert!(back.layer(Provider::Sgr).unwrap().schedule.is_none(), "an empty sweep saves nothing");
        // An old file whose fleet block only holds a signed-in bolt loads as-is (the engine sync
        // adds the system radars on top afterwards).
        let token_json = serde_json::to_string(&Auth::Token("x".into())).unwrap();
        let old = format!(r#"{{"layers":[{{"provider":"bolt","visible":true,"auth":{token_json}}}]}}"#);
        let parsed: FleetState = serde_json::from_str(&old).expect("an old fleet block loads");
        assert_eq!(parsed.layers.len(), 1);
        assert_eq!(parsed.layers[0].provider, Provider::Bolt);
    }
}
