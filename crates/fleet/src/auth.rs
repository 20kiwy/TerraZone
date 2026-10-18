//! The auth flows: one trait controls every provider's credential — what it needs, how the
//! user signs in (Bolt: phone number → a 4-digit code the vendor sends), how the system mints
//! its own sessions (the radars' cookies, traced from the vendors' own clients), and how a
//! data poll rides. The crate stays network-free: requests are *described* here (URL, method,
//! headers, body), the host's [`crate::FleetFetch`] executes them.
//!
//! The shape follows the reference gateway's `Source` contract: a caller asks a flow three
//! questions — *what credential do you need, and have you got it?*, *how do I sign in / mint
//! one?*, *what does a poll ride with?* — without learning any vendor's name.

use serde::{Deserialize, Serialize};

use crate::feed::FleetRequest;
use crate::rail::SgrAuth;
use crate::{Provider, VehicleKind};

/// What a flow needs before it can fetch anything, and where it is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Credential {
    /// Public data — the flight feed needs no account at all.
    None,
    /// A cookie the *system* obtains by itself (TerraZone visits the vendor, the
    /// `Set-Cookie` answer becomes the session, and it rotates when it expires).
    /// Nothing of this is ever typed into the UI.
    Cookie { service: &'static str, name: &'static str },
    /// A session the *user* establishes once and then keeps — Bolt's phone OTP mints
    /// one, and it is refreshed rather than re-typed. Stored in the app's settings vault.
    Session { service: &'static str, name: &'static str },
}

impl Credential {
    /// Where this credential lives in the settings vault, if it is kept at all.
    pub fn vault_key(self) -> Option<(&'static str, &'static str)> {
        match self {
            Credential::None => None,
            Credential::Cookie { service, name } | Credential::Session { service, name } => Some((service, name)),
        }
    }

    /// Whether an operator would supply this by hand. In TerraZone nothing is pasted:
    /// a session is negotiated (the 4-digit code flow) and a cookie is minted by the
    /// system — so this is false for every flow that ships.
    pub fn is_pasted(self) -> bool {
        false
    }
}

/// Who and where we claim to be, for one sign-in. Bolt ties the pending session to the
/// device, so the same values must ride `otp_start` and `otp_confirm`, and the device id
/// should live as long as the session does (the vault keeps it under `bolt/device`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// Stable per-installation identifier (random once, kept in the settings vault).
    pub uuid: String,
    pub lat: f64,
    pub lng: f64,
    /// ISO-3166-1 alpha-2, lowercased before it reaches the API (inferred from the map view).
    pub country: String,
    /// IANA name, e.g. `Africa/Dar_es_Salaam`.
    pub timezone: String,
}

/// A sign-in in flight: the code has been sent to the phone, the session is not minted yet.
/// Kept by the UI draft — it is editing state, never saved with a document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingSession {
    pub session_id: String,
    pub device_id: String,
    pub phone: String,
    /// Bolt JSON `type` for start/confirm: `sms`, `phone` (voice) or `whatsapp`.
    pub channel: String,
    /// When the code was requested (unix ms) — the draft ages out stale attempts.
    pub created_ms: i64,
}

/// A signed-in Bolt session — the secret the settings vault keeps under `bolt/session`
/// (serialized). The poll rides `Authorization: Basic base64(auth_username:device_id)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoltSession {
    pub session_id: String,
    pub device_id: String,
    pub auth_username: String,
    pub phone: String,
    /// Unix ms the code was confirmed.
    pub created_ms: i64,
}

impl BoltSession {
    /// The stored session from a layer's auth secret (the vault's `bolt/session` JSON).
    pub fn parse(secret: &str) -> Option<BoltSession> {
        serde_json::from_str(secret.trim()).ok()
    }

    /// The serialized form the vault and `fleet.setAuth` carry.
    pub fn to_secret(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The Basic auth value the poll rides: `base64(auth_username:device_id)`.
    pub fn basic_auth(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", self.auth_username, self.device_id))
    }
}

/// The trust TerraZone builds itself: an **auto-generated** Basic-auth code for the Bolt
/// feed, minted from a username + device pair the deployment provides (the
/// `BOLT_AUTH_USERNAME` and `BOLT_DEVICE_UUID` environment values, or a ready
/// `BOLT_AUTH_TOKEN` Basic header). The frame-one heal seeds it into the settings vault and
/// signs the feed in, so the fleet is live before anyone types anything — the code is
/// generated, not requested from a phone, and the pair survives in the vault across runs.
#[derive(Clone, Debug, PartialEq)]
pub struct BoltTrust {
    /// The `uid_…` username the Basic code carries.
    pub auth_username: String,
    /// The device uuid the Basic code carries (and the poll's device identity).
    pub device_uuid: String,
    /// The label the session keeps as its phone (a deployment may name a real number).
    pub phone: String,
}

impl BoltTrust {
    /// The deployment's own trust pair, **built in**: when the environment carries nothing of
    /// its own, the app signs Bolt in from these — the value lives in the field ("hardcode
    /// the Bolt value … so to show Bolt automatically"), so the feed is live on the first
    /// launch with zero setup. An env pair (or a ready Basic header) outranks it; a session
    /// the vendor itself minted (a real phone sign-in kept in the vault) outranks both — the
    /// tick only re-mints when the stored session is a trust mint.
    const DEPLOY_USERNAME: &str = "uid_247899280";
    const DEPLOY_DEVICE: &str = "F4669DCE-9F61-480A-9C02-345F12A78C32";

    /// The trust this run signs Bolt in with: the environment's pair when the deployment
    /// provides one, else the built-in deployment pair above (never `None` now — the feed
    /// shows automatically).
    pub fn from_env() -> Option<BoltTrust> {
        let read = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let env = Self::from_parts(
            read("BOLT_AUTH_TOKEN").as_deref(),
            read("BOLT_AUTH_USERNAME").as_deref(),
            read("BOLT_DEVICE_UUID").as_deref(),
            read("BOLT_AUTH_PHONE").as_deref(),
        );
        env.or_else(|| Self::from_parts(None, Some(Self::DEPLOY_USERNAME), Some(Self::DEPLOY_DEVICE), None))
    }

    /// The pure core [`Self::from_env`] reads: a ready Basic header (`BOLT_AUTH_TOKEN`), or
    /// the username + device pair, plus an optional phone label. `None` without a usable
    /// pair — the ordinary phone sign-in is then the only path, as before.
    pub fn from_parts(token: Option<&str>, username: Option<&str>, device_uuid: Option<&str>, phone: Option<&str>) -> Option<BoltTrust> {
        let (user, device) = match token {
            Some(t) => decode_basic(t)?,
            None => (username?.trim().to_string(), device_uuid?.trim().to_string()),
        };
        if user.is_empty() || device.is_empty() {
            return None;
        }
        Some(BoltTrust { auth_username: user, device_uuid: device, phone: phone.map(str::to_string).unwrap_or_else(|| "auto sign-in".into()) })
    }

    /// The Basic header code this trust generates: `base64(auth_username:device_uuid)` —
    /// the exact value `BoltSession::basic_auth` will ride on every poll.
    pub fn basic_code(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", self.auth_username, self.device_uuid))
    }

    /// The session secret the trust mints (the vault's `bolt/session` JSON) — the same
    /// shape a phone sign-in produces, so every consumer reads one form. The session id is
    /// stable per deployment: the vendor accepts a session it has seen (verified live) and
    /// a fresh id every launch reads as a minting farm to its new throttle.
    pub fn session_secret(&self) -> String {
        let short: String = self.device_uuid.chars().take(8).collect();
        let created_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        // The session id is STABLE per deployment (the device pair alone names it): the
        // vendor accepts a session it saw before (verified live — the same sid polls round
        // after round), and a fresh id every launch reads as a minting farm to the vendor's
        // new throttle. A real phone sign-in in the vault outranks the trust and stays.
        BoltSession {
            session_id: format!("trust-{short}"),
            device_id: self.device_uuid.clone(),
            auth_username: self.auth_username.clone(),
            phone: self.phone.clone(),
            created_ms,
        }
        .to_secret()
    }

    /// The device uuid as a vault `bolt/device` row (the sign-in flow's own storage).
    pub fn device_secret(&self) -> String {
        self.device_uuid.clone()
    }
}

/// Decode a Basic header (the `Basic ` prefix optional) into its `user:secret` pair.
fn decode_basic(header: &str) -> Option<(String, String)> {
    let raw = header.trim().strip_prefix("Basic ").unwrap_or(header.trim());
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD.decode(raw.trim()).ok()?;
    let joined = String::from_utf8(decoded).ok()?;
    let (user, secret) = joined.split_once(':')?;
    Some((user.to_string(), secret.to_string()))
}

/// How a described request goes out. GET is the fleet's everyday transport; the Bolt
/// sign-in and vehicle poll are POSTs with a JSON body.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthMethod {
    Get,
    Post { body: String, content_type: String },
}

/// One ask the host executes: a poll, a sign-in step or the system's own cookie handshake.
/// The same shape as [`FleetRequest`] plus the method — the runtime dispatches on `key`.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthRequest {
    pub key: String,
    pub url: String,
    pub method: AuthMethod,
    pub headers: Vec<(String, String)>,
}

/// One provider's credential lifecycle, behind one trait. Implement it and the runtime can
/// sign the user in, mint the system's own session and fetch data without learning the
/// vendor's name — the way a new source appears in the gateway's poller as a registration.
pub trait AuthFlow: Send + Sync {
    /// Stable key — names the vault rows it owns (`bolt/session`, `bolt/device`, …).
    fn id(&self) -> &'static str;

    /// What credential this source wants and where it is stored.
    fn credential(&self) -> Credential;

    /// Whether a credential must exist before any data poll can go out. The flight feed
    /// is public — it polls with browser headers and no account at all.
    fn needs_credential(&self) -> bool {
        true
    }

    /// Sign-in step one (Bolt): send the 4-digit code to `phone`. The answer is the
    /// vendor's OK — the pending session the caller keeps is built locally. Flows without
    /// a user sign-in (the public flight feed) refuse.
    fn otp_start(&self, _phone: &str, _device: &Device, _poll: u64) -> Result<AuthRequest, String> {
        Err("this feed has no user sign-in".into())
    }

    /// Sign-in step two (Bolt): check the 4-digit code. Interpret the answer with
    /// [`bolt_confirm_answer`] — on success it mints the [`BoltSession`] to store.
    fn otp_confirm(&self, _pending: &PendingSession, _code: &str, _device: &Device, _poll: u64) -> Result<AuthRequest, String> {
        Err("this feed has no user sign-in".into())
    }

    /// The system's own handshake: the request whose `Set-Cookie` answer becomes the feed's
    /// session. `None` — the flow mints nothing (Bolt's credential is the user's; flights
    /// need none at all).
    fn bootstrap(&self, poll: u64) -> Option<AuthRequest>;

    /// The feed's data poll. `Err(reason)` skips this round — an unreadable session asks
    /// to be signed in again, a pending handshake is simply not ready. The `endpoint`
    /// override is handled by the caller (template mode) before this is reached.
    fn poll(&self, secret: &str, view: crate::feed::ViewBox, zoom: f64, poll_no: u64) -> Result<FleetRequest, String>;
}

/// The flow for a provider — the one lookup the runtime needs.
pub fn flow_for(provider: Provider) -> &'static dyn AuthFlow {
    match provider {
        Provider::Bolt => &BoltAuth,
        Provider::Marine => &MarineAuth,
        Provider::Flight => &FlightAuth,
        Provider::Sgr => &SgrAuth,
    }
}

/// Bolt ride-hailing — the rider API, traced from the vendor's own client: the session is
/// minted by a phone OTP (start → the 4-digit code → confirm), the vehicle poll is a POST
/// under `Authorization: Basic`, and the vehicle's icon id is zone-specific (the table
/// below covers the ids seen so far; unknown ids fall back to the category name).
pub struct BoltAuth;

/// MarineTraffic — the mobile tile API. Its fixed constants (the `vessel-image` token, the
/// iOS app headers) are what the endpoint demands; the Cloudflare `__cf_bm` cookie is the
/// session, warmed by the system's own visit to the homepage — never typed by a user.
pub struct MarineAuth;

/// FlightRadar24 — keyless. The public `feed.js` answers real data when the request
/// carries the web client's headers (origin/referer and a browser user-agent).
pub struct FlightAuth;

const BOLT_BASE: &str = "https://user.live.boltsvc.net";
const BOLT_UA: &str = "Bolt/13113543 CFNetwork/1568.300.101 Darwin/24.2.0";
const BOLT_VERSION: &str = "CI.180.0";
const BOLT_LANGUAGE: &str = "en-GB";
const BOLT_DEVICE_TYPE: &str = "iphone";
const BOLT_DEVICE_OS: &str = "18.2.0";
const BOLT_DEVICE_NAME: &str = "iPhone";
const BOLT_GPS_ACCURACY: &str = "5.0";

const MARINE_TILE_BASE: &str = "https://www.marinetraffic.com/getData/get_data_json_4_mob";
const MARINE_UA: &str = "MarineTraffic/5.1.10 (iOS)";
/// A fixed constant the MarineTraffic mobile client sends on every tile request; the
/// endpoint rejects calls without it. Not a credential and not tied to any account.
const MARINE_VESSEL_IMAGE: &str = "339f89c84a559f573636a47ff8daed0d3308";
/// MarineTraffic tile zoom: 7 covers a country-sized area in one request.
const MARINE_Z: u32 = 7;

const FLIGHT_FEED: &str = "https://data-cloud.flightradar24.com/zones/fcgi/feed.js";
const FLIGHT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

fn bolt_common_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), BOLT_UA.into()),
        ("Accept".into(), "*/*".into()),
        ("Accept-Language".into(), "en-GB,en;q=0.9".into()),
        ("Content-Type".into(), "application/json".into()),
    ]
}

/// Bolt's identity query — every API call carries the device and GPS block the app sends.
fn bolt_query(device: &Device, session_id: &str) -> String {
    let lat = device.lat;
    let lng = device.lng;
    format!(
        "version={BOLT_VERSION}&language={BOLT_LANGUAGE}&brand=bolt&deviceType={BOLT_DEVICE_TYPE}\
&session_id={session_id}&deviceId={uuid}&lng={lng:.6}&device_os_version={BOLT_DEVICE_OS}\
&gps_lng={lng:.6}&device_name={BOLT_DEVICE_NAME}&country={cc}&gps_lat={lat:.6}\
&distinct_id=$device:{uuid}&gps_accuracy_m={BOLT_GPS_ACCURACY}&lat={lat:.6}&gps_age=1.0",
        uuid = device.uuid,
        cc = device.country
    )
}

fn bolt_last_known_state(device: &Device) -> String {
    format!(r#""last_known_state":{{"location":{{"lng":{:.6},"lat":{:.6}}},"opened_product":{{"product":"taxi"}}}}"#, device.lng, device.lat)
}

impl AuthFlow for BoltAuth {
    fn id(&self) -> &'static str {
        "bolt"
    }

    fn credential(&self) -> Credential {
        Credential::Session { service: "bolt", name: "session" }
    }

    fn otp_start(&self, phone: &str, device: &Device, poll: u64) -> Result<AuthRequest, String> {
        if phone.trim().is_empty() {
            return Err("enter the phone number the code should go to".into());
        }
        let url = format!("{BOLT_BASE}/profile/verification/start/v2?{}", bolt_query(device, &device.uuid));
        // `password` is the device id — the vendor treats the install as the credential
        // holder until the code confirms otherwise.
        let body = format!(
            r#"{{"type":"sms","timezone":"{}","password":"{}","phone_number":"{}","method":"sms",{}}}"#,
            device.timezone,
            device.uuid,
            phone.trim(),
            bolt_last_known_state(device)
        );
        Ok(AuthRequest {
            key: format!("bolt/otp-start{poll}"),
            url,
            method: AuthMethod::Post { body, content_type: "application/json".into() },
            headers: bolt_common_headers(),
        })
    }

    fn otp_confirm(&self, pending: &PendingSession, code: &str, device: &Device, poll: u64) -> Result<AuthRequest, String> {
        let trimmed = code.trim();
        if trimmed.len() != 4 || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
            return Err("the code is the 4 digits the SMS carries".into());
        }
        let url = format!(
            "{BOLT_BASE}/profile/verification/confirm/v3?lng={lng:.6}&rh_session_id={sid}&country={cc}\
&distinct_id=device:{sid}&deviceId={sid}&device_os_version={BOLT_DEVICE_OS}&deviceType={BOLT_DEVICE_TYPE}\
&brand=bolt&language={BOLT_LANGUAGE}&session_id={sid}&version={BOLT_VERSION}&gps_lat={lat:.6}\
&device_name={BOLT_DEVICE_NAME}&gps_accuracy_m={BOLT_GPS_ACCURACY}&gps_age=1.0&lat={lat:.6}&gps_lng={lng:.6}",
            sid = pending.session_id,
            cc = device.country,
            lat = device.lat,
            lng = device.lng
        );
        let body = format!(
            r#"{{"type":"{}","password":"{}","timezone":"{}","phone_number":"{}","code":"{}",{}}}"#,
            pending.channel,
            pending.device_id,
            device.timezone,
            pending.phone,
            trimmed,
            bolt_last_known_state(device)
        );
        Ok(AuthRequest {
            key: format!("bolt/otp-confirm{poll}"),
            url,
            method: AuthMethod::Post { body, content_type: "application/json".into() },
            headers: bolt_common_headers(),
        })
    }

    fn bootstrap(&self, _poll: u64) -> Option<AuthRequest> {
        // Bolt's credential is the user's session — the system never mints one.
        None
    }

    fn poll(&self, secret: &str, view: crate::feed::ViewBox, _zoom: f64, poll_no: u64) -> Result<FleetRequest, String> {
        let session = BoltSession::parse(secret).ok_or_else(|| "sign in again — the stored session is unreadable".to_string())?;
        let (lat, lng) = (view.1 + view.3, view.0 + view.2);
        let (lat, lng) = (lat / 2.0, lng / 2.0);
        // A viewport roughly 5 km across, centred on the view — Bolt widens it if needed.
        let d = 0.025_f64;
        let (mut sw_lat, mut sw_lng, mut ne_lat, mut ne_lng) = (lat - d, lng - d, lat + d, lng + d);
        if (ne_lat - sw_lat).abs() < 0.005 {
            sw_lat = lat - 0.0025;
            ne_lat = lat + 0.0025;
        }
        if (ne_lng - sw_lng).abs() < 0.005 {
            sw_lng = lng - 0.0025;
            ne_lng = lng + 0.0025;
        }
        let url = format!(
            "{BOLT_BASE}/mobility/search/poll?version={BOLT_VERSION}&rh_session_id={sid}&language={BOLT_LANGUAGE}\
&gps_lat={lat:.6}&device_name={BOLT_DEVICE_NAME}&distinct_id=client-001&device_os_version={BOLT_DEVICE_OS}\
&gps_accuracy_m={BOLT_GPS_ACCURACY}&deviceId={did}&deviceType={BOLT_DEVICE_TYPE}&gps_lng={lng:.6}&brand=bolt\
&lat={lat:.6}&user_id={user}&session_id={sid}&gps_age=0.1&lng={lng:.6}",
            sid = session.session_id,
            did = session.device_id,
            user = session.auth_username,
            lat = lat,
            lng = lng
        );
        // NOTE: the body is ONE LINE. The vendor's tightened parser (2026, from CI.180.0
        // on) answers a pretty-printed poll 702 INVALID_REQUEST — whitespace between the
        // fields reads as a malformed body now. The device block rides current values too:
        // the old `iOS13.2`/`iPhone16,1` pair reads as a stale install and is refused.
        let body = format!(
            r#"{{"order_handle":{{}},"stage":"category_selection","pickup_stop":{{"lat":{lat:.6},"lng":{lng:.6}}},"viewport":{{"south_west":{{"lat":{sw_lat:.6},"lng":{sw_lng:.6}}},"north_east":{{"lat":{ne_lat:.6},"lng":{ne_lng:.6}}}}},"destination_stops":[{{"lat":{dlat:.6},"lng":{dlng:.6}}}],"payment_method":{{"type":"default","id":"cash"}}}}"#,
            lat = lat,
            lng = lng,
            dlat = lat + 0.005,
            dlng = lng + 0.005
        );
        let mut headers = bolt_common_headers();
        headers.push(("Authorization".into(), format!("Basic {}", session.basic_auth())));
        Ok(FleetRequest {
            key: crate::feed::FleetRequest::key(Provider::Bolt, poll_no as usize),
            provider: Provider::Bolt,
            url,
            headers,
            cells: Vec::new(),
            body: Some((body, "application/json".into())),
        })
    }
}

impl AuthFlow for MarineAuth {
    fn id(&self) -> &'static str {
        "marine"
    }

    fn credential(&self) -> Credential {
        Credential::Cookie { service: "marinetraffic", name: "cf_cookie" }
    }

    fn bootstrap(&self, poll: u64) -> Option<AuthRequest> {
        // The jar warm: one plain visit to the homepage makes Cloudflare hand back the
        // `__cf_bm` cookie the tile call replays. The host answers with `Set-Cookie`.
        Some(AuthRequest {
            key: format!("marine-bootstrap{poll}"),
            url: "https://www.marinetraffic.com/".into(),
            method: AuthMethod::Get,
            headers: vec![
                ("User-Agent".into(), MARINE_UA.into()),
                ("Accept".into(), "text/html,application/xhtml+xml".into()),
                ("Accept-Language".into(), "en-GB,en;q=0.9".into()),
            ],
        })
    }

    fn poll(&self, secret: &str, view: crate::feed::ViewBox, _zoom: f64, poll_no: u64) -> Result<FleetRequest, String> {
        let (lat, lng) = ((view.1 + view.3) / 2.0, (view.0 + view.2) / 2.0);
        // x/y are computed at z−1 to match the endpoint's tiling (the vendor client's quirk).
        let zxy = MARINE_Z.saturating_sub(1).max(1);
        let n = 2_f64.powi(zxy as i32);
        let x = (((lng + 180.0) / 360.0) * n).floor().clamp(0.0, n - 1.0) as u32;
        let lat_r = lat.to_radians();
        let y = ((1.0 - (lat_r.tan() + lat_r.cos().recip()).ln() / std::f64::consts::PI) / 2.0 * n).floor().clamp(0.0, n - 1.0) as u32;
        let cb = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let url = format!("{MARINE_TILE_BASE}/z:{MARINE_Z}/X:{x}/Y:{y}/station:0?cb={cb}");
        let mut headers = vec![
            ("Accept".into(), "application/json".into()),
            ("Origin".into(), "https://www.marinetraffic.com".into()),
            ("is_mobile_v2".into(), "true".into()),
            ("Referer".into(), "https://www.marinetraffic.com/".into()),
            ("is-mobile".into(), "true".into()),
            ("User-Agent".into(), MARINE_UA.into()),
            ("x-app-version".into(), "5.1.10".into()),
            ("x-requested-with".into(), "XMLHttpRequest".into()),
            ("vessel-image".into(), MARINE_VESSEL_IMAGE.into()),
        ];
        if !secret.trim().is_empty() {
            headers.push(("Cookie".into(), secret.trim().to_string()));
        }
        Ok(FleetRequest {
            key: crate::feed::FleetRequest::key(Provider::Marine, poll_no as usize),
            provider: Provider::Marine,
            url,
            headers,
            cells: Vec::new(),
            body: None,
        })
    }
}

impl AuthFlow for FlightAuth {
    fn id(&self) -> &'static str {
        "flight"
    }

    fn credential(&self) -> Credential {
        Credential::None
    }

    fn needs_credential(&self) -> bool {
        false
    }

    fn otp_start(&self, _phone: &str, _device: &Device, _poll: u64) -> Result<AuthRequest, String> {
        Err("the flight feed is public — nothing to sign in to".into())
    }

    fn otp_confirm(&self, _pending: &PendingSession, _code: &str, _device: &Device, _poll: u64) -> Result<AuthRequest, String> {
        Err("the flight feed is public — nothing to verify".into())
    }

    fn bootstrap(&self, _poll: u64) -> Option<AuthRequest> {
        None
    }

    fn poll(&self, secret: &str, view: crate::feed::ViewBox, _zoom: f64, poll_no: u64) -> Result<FleetRequest, String> {
        let (w, s, e, n) = view;
        // The vendor renamed the query (2026: `bounds=` answers stats only, `bbox=` answers
        // the aircraft object) and answers anonymous callers with its global capped cloud —
        // the runtime keeps the view's share (the drain filters by the box this ask covers).
        let url = format!(
            "{FLIGHT_FEED}?bbox={n:.3},{s:.3},{w:.3},{e:.3}\
&faa=1&satellite=1&mlat=1&flarm=1&adsb=1&gnd=1&air=1&vehicles=1&estimated=1&maxage=14400&gliders=1&stats=0"
        );
        let mut headers = vec![
            ("User-Agent".into(), FLIGHT_UA.into()),
            ("Accept".into(), "application/json".into()),
            ("Accept-Language".into(), "en-US,en;q=0.9".into()),
            ("Origin".into(), "https://www.flightradar24.com".into()),
            ("Referer".into(), "https://www.flightradar24.com/".into()),
        ];
        if !secret.trim().is_empty() {
            headers.push(("Cookie".into(), secret.trim().to_string()));
        }
        Ok(FleetRequest {
            key: crate::feed::FleetRequest::key(Provider::Flight, poll_no as usize),
            provider: Provider::Flight,
            url,
            headers,
            cells: Vec::new(),
            body: None,
        })
    }
}

/// Interpret a confirm answer: the vendor reports `{code, message, data}` — `code 0` with
/// an `OK` message means the code checked out, and the session's `auth_username` rides
/// somewhere under `data` (it has moved between releases, so the payload is searched, not
/// a pinned path). Keys of the error path are named, never values — the payload carries
/// session tokens.
pub fn bolt_confirm_answer(pending: &PendingSession, bytes: &[u8]) -> Result<BoltSession, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer is not UTF-8 text".to_string())?;
    let parsed: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the confirm answer is not JSON: {e}"))?;
    let code = parsed.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    let message = parsed.get("message").and_then(|v| v.as_str()).unwrap_or("(no message)");
    if code != 0 || !message.eq_ignore_ascii_case("OK") {
        // A nonzero code is the vendor refusing the code (wrong, expired, re-sent) —
        // the answer is always "ask again", never a retry of the same code.
        return Err(format!("the code was rejected — request a new one ({message}, code {code})"));
    }
    let data = parsed.get("data").unwrap_or(&parsed);
    let auth_username = find_string(data, "auth_username")
        .ok_or_else(|| format!("the confirm answer carried no session (keys received: {})", describe_shape(data, 3)))?;
    Ok(BoltSession {
        session_id: pending.session_id.clone(),
        device_id: pending.device_id.clone(),
        auth_username,
        phone: pending.phone.clone(),
        created_ms: pending.created_ms,
    })
}

/// Interpret an otp-start answer: any vendor error object (`code != 0`) is the failure.
pub fn bolt_start_answer(bytes: &[u8]) -> Result<(), String> {
    let Ok(text) = std::str::from_utf8(bytes) else { return Ok(()) };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) else { return Ok(()) };
    let code = parsed.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
    if code != 0
        && let Some(message) = parsed.get("message").and_then(|v| v.as_str())
    {
        return Err(format!("the code could not be sent: {message} (code {code})"));
    }
    Ok(())
}

fn find_string(v: &serde_json::Value, key: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                if k == key
                    && let Some(s) = val.as_str()
                    && !s.is_empty()
                {
                    return Some(s.to_string());
                }
            }
            map.values().find_map(|child| find_string(child, key))
        }
        serde_json::Value::Array(items) => items.iter().find_map(|child| find_string(child, key)),
        _ => None,
    }
}

/// Key names only, never values — describing what arrived without leaking tokens.
fn describe_shape(v: &serde_json::Value, depth: usize) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let keys: Vec<&str> = map.keys().map(String::as_str).collect();
            if depth == 0 {
                format!("{keys:?}")
            } else {
                let inner: Vec<String> = map.values().map(|child| describe_shape(child, depth - 1)).take(6).collect();
                format!("{{{keys:?}: {inner:?}}}")
            }
        }
        serde_json::Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(|child| describe_shape(child, depth - 1)).take(4).collect();
            format!("[{inner:?}]")
        }
        other => match other {
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Bool(_) => "bool".to_string(),
            serde_json::Value::Number(_) => "number".to_string(),
            serde_json::Value::String(_) => "string".to_string(),
            _ => "value".to_string(),
        },
    }
}

/// One bounding box `(min_lat, max_lat, min_lng, max_lng)` and the ISO code it names.
type Region = ((f64, f64, f64, f64), &'static str);

/// Rough ISO-3166-1 alpha-2 from the map view's centre — Bolt's query carries the country
/// the session rides in, and the phone's market is where the map is looking. More specific
/// boxes first, broader ones later. The table is the vendor's own market list: Dar es Salaam
/// is the home view, and every Bolt market in Europe, Africa and beyond names itself so the
/// poll is data-driven wherever the map looks.
pub fn phone_country(lat: f64, lng: f64) -> &'static str {
    const REGIONS: &[Region] = &[
        // East Africa first — Dar es Salaam is the home view. Kenya and Uganda's boxes
        // overlap Tanzania's northern edge, so they name themselves before the broader
        // Tanzania box does.
        ((-5.0, 5.5, 33.0, 42.5), "ke"),
        ((-5.0, 5.5, 29.0, 41.0), "ug"),
        ((-12.0, -1.0, 29.0, 41.0), "tz"),
        // West / Central Africa
        ((4.0, 14.0, 2.5, 15.0), "ng"),
        ((1.0, 12.0, -4.0, 2.5), "gh"),
        ((4.0, 11.5, -9.0, -4.5), "ci"),
        ((6.0, 13.0, -3.5, 1.5), "bf"),
        ((0.5, 5.5, 5.5, 9.0), "bj"),
        ((1.0, 6.8, 8.0, 16.5), "cm"),
        ((-5.0, 5.5, 8.5, 19.0), "cd"),
        ((-13.0, -5.0, 11.0, 31.0), "zm"),
        ((-18.0, -8.0, 19.0, 34.0), "zw"),
        ((-27.0, -21.5, 19.0, 33.5), "bw"),
        ((-35.0, -22.0, 16.0, 33.5), "za"),
        ((-18.0, -8.0, 11.0, 25.0), "na"),
        ((11.5, 15.0, -18.0, -11.0), "mr"),
        ((18.5, 27.5, -18.0, -8.0), "ma"),
        // Europe (Bolt markets, approximate) — the vendor's own list, so a Europe view gets
        // Europe's vehicle ids and icons from the poll.
        ((49.5, 61.0, -11.0, 2.0), "gb"),
        ((59.0, 70.0, -10.0, 3.0), "ie"),
        ((49.0, 55.5, 3.0, 7.5), "nl"),
        ((50.5, 54.5, 2.0, 7.0), "be"),
        ((47.0, 55.5, 5.5, 15.5), "de"),
        ((49.5, 51.5, 14.0, 19.0), "cz"),
        ((49.0, 55.0, 16.5, 22.5), "sk"),
        ((45.5, 49.5, 9.5, 17.5), "at"),
        ((35.5, 48.0, 6.5, 19.0), "it"),
        ((45.8, 48.0, 8.5, 18.5), "ch"),
        ((41.5, 51.5, -5.5, 10.0), "fr"),
        ((36.0, 44.0, -10.0, 5.0), "es"),
        ((38.5, 42.0, -9.6, -6.0), "pt"),
        ((48.5, 55.0, 14.0, 24.2), "pl"),
        ((45.5, 49.5, 16.0, 22.2), "hu"),
        ((43.5, 48.5, 20.5, 29.8), "ro"),
        ((43.0, 47.5, 22.2, 28.8), "bg"),
        ((41.0, 44.5, 19.0, 23.5), "al"),
        ((33.5, 42.5, 19.5, 30.5), "gr"),
        ((35.5, 43.5, 25.5, 45.5), "tr"),
        ((45.0, 47.5, 13.2, 16.8), "si"),
        ((42.0, 46.6, 13.2, 19.5), "hr"),
        ((41.8, 44.0, 18.4, 20.9), "me"),
        ((41.8, 43.4, 20.0, 22.0), "xk"),
        ((41.8, 46.2, 18.8, 23.1), "rs"),
        ((55.5, 59.7, 20.5, 28.5), "ee"),
        ((55.5, 58.5, 20.5, 27.5), "lv"),
        ((53.5, 56.5, 20.5, 27.0), "lt"),
        ((57.5, 60.5, 10.5, 20.0), "se"),
        ((57.5, 62.5, 4.0, 12.0), "no"),
        ((54.5, 58.5, 8.0, 16.0), "dk"),
        ((59.0, 70.5, 15.0, 32.0), "fi"),
        ((44.0, 48.5, 26.5, 30.0), "md"),
        ((46.5, 52.4, 22.2, 40.2), "ua"),
        // Americas
        ((24.0, 50.0, -125.0, -66.0), "us"),
        ((13.0, 33.0, -119.0, -86.0), "mx"),
        ((-56.0, 13.0, -75.0, -34.0), "br"),
        // Middle East / Asia samples
        ((22.5, 32.0, 34.0, 39.5), "eg"),
        ((24.0, 32.0, 50.0, 57.0), "ae"),
    ];
    for ((min_lat, max_lat, min_lng, max_lng), code) in REGIONS {
        if lat >= *min_lat && lat <= *max_lat && lng >= *min_lng && lng <= *max_lng {
            return code;
        }
    }
    "us"
}

/// The IANA timezone a sign-in reports, from the inferred country.
pub fn timezone_for_country(country: &str) -> &'static str {
    let b = country.as_bytes();
    if b.len() != 2 {
        return "UTC";
    }
    match (b[0].to_ascii_lowercase(), b[1].to_ascii_lowercase()) {
        (b't', b'z') => "Africa/Dar_es_Salaam",
        (b'n', b'g') => "Africa/Lagos",
        (b'k', b'e') => "Africa/Nairobi",
        (b'u', b'g') => "Africa/Kampala",
        (b'z', b'a') => "Africa/Johannesburg",
        (b'g', b'h') => "Africa/Accra",
        (b'c', b'i') => "Africa/Abidjan",
        (b'c', b'm') => "Africa/Douala",
        (b'c', b'd') => "Africa/Kinshasa",
        (b'z', b'm') => "Africa/Lusaka",
        (b'z', b'w') => "Africa/Harare",
        (b'b', b'w') => "Africa/Gaborone",
        (b'n', b'a') => "Africa/Windhoek",
        (b'm', b'a') => "Africa/Casablanca",
        (b'b', b'f') => "Africa/Ouagadougou",
        (b'b', b'j') => "Africa/Porto-Novo",
        (b'g', b'b') => "Europe/London",
        (b'i', b'e') => "Europe/Dublin",
        (b'd', b'e') => "Europe/Berlin",
        (b'f', b'r') => "Europe/Paris",
        (b'n', b'l') => "Europe/Amsterdam",
        (b'b', b'e') => "Europe/Brussels",
        (b'e', b's') => "Europe/Madrid",
        (b'p', b't') => "Europe/Lisbon",
        (b'i', b't') => "Europe/Rome",
        (b'a', b't') => "Europe/Vienna",
        (b'h', b'u') => "Europe/Budapest",
        (b'p', b'l') => "Europe/Warsaw",
        (b'c', b'z') => "Europe/Prague",
        (b's', b'k') => "Europe/Bratislava",
        (b's', b'e') => "Europe/Stockholm",
        (b'n', b'o') => "Europe/Oslo",
        (b'd', b'k') => "Europe/Copenhagen",
        (b'f', b'i') => "Europe/Helsinki",
        (b'c', b'h') => "Europe/Zurich",
        (b'r', b'o') => "Europe/Bucharest",
        (b'b', b'g') => "Europe/Sofia",
        (b'g', b'r') => "Europe/Athens",
        (b't', b'r') => "Europe/Istanbul",
        (b'm', b'd') => "Europe/Chisinau",
        (b'u', b'a') => "Europe/Kyiv",
        (b'e', b'e') => "Europe/Tallinn",
        (b'l', b'v') => "Europe/Riga",
        (b'l', b't') => "Europe/Vilnius",
        (b'r', b's') => "Europe/Belgrade",
        (b'h', b'r') => "Europe/Zagreb",
        (b's', b'i') => "Europe/Ljubljana",
        (b'm', b'e') => "Europe/Podgorica",
        (b'a', b'l') => "Europe/Tirane",
        (b'u', b's') => "America/New_York",
        (b'm', b'x') => "America/Mexico_City",
        (b'b', b'r') => "America/Sao_Paulo",
        (b'a', b'e') => "Asia/Dubai",
        (b'e', b'g') => "Africa/Cairo",
        _ => "UTC",
    }
}

/// A human name for one of Bolt's zone-specific icon ids, when the table knows it.
pub fn bolt_type_name(icon_id: &str) -> Option<&'static str> {
    Some(match icon_id {
        "178" => "Bajaji",
        "201" => "XL",
        "231" => "Basic",
        "278" => "Boda",
        "8749" => "Send Motorbike",
        "9172" => "Electric",
        "9195" => "Bolt Airport",
        _ => return None,
    })
}

/// The marker kind a Bolt category (or icon id) draws as — motorbikes (boda, bajaji, send)
/// point with the bike icon, XL vans with the van, everything else is a car.
pub fn bolt_kind_for(category: &str, icon_id: &str) -> VehicleKind {
    let hay = format!("{category} {icon_id}").to_ascii_lowercase();
    let named = bolt_type_name(icon_id).unwrap_or_default().to_ascii_lowercase();
    let hay = format!("{hay} {named}");
    if hay.contains("boda") || hay.contains("bajaji") || hay.contains("moto") || hay.contains("bike") {
        VehicleKind::Motorcycle
    } else if hay.contains("xl") || hay.contains("van") {
        VehicleKind::Van
    } else {
        VehicleKind::Car
    }
}

/// The marker kind from the vendor's own category **group** (the `category_details` block's
/// `group` field — the data-driven path, so a Europe market's "Comfort" or "Priority" class
/// reads right without anyone extending a table). Names the vendor uses:
/// `motorbike`, `delivery_motorbike`, `moped` → bike; `autorickshaw` (the bajaji) → bike;
/// `xl` → van; every other group is a car.
pub fn bolt_group_kind(group: &str) -> Option<VehicleKind> {
    let g = group.trim().to_ascii_lowercase();
    if g.is_empty() {
        return None;
    }
    Some(if g.contains("motorbike") || g.contains("moped") || g.contains("bike") || g.contains("autorickshaw") {
        VehicleKind::Motorcycle
    } else if g == "xl" || g.contains("van") {
        VehicleKind::Van
    } else {
        VehicleKind::Car
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::ViewBox;
    use base64::Engine as _;

    fn device() -> Device {
        Device { uuid: "tz-device-1".into(), lat: -6.778, lng: 39.223, country: "tz".into(), timezone: "Africa/Dar_es_Salaam".into() }
    }

    const SINZA: ViewBox = (39.18, -6.80, 39.27, -6.74);

    #[test]
    fn credentials_point_at_the_vault_rows_that_exist() {
        assert_eq!(BoltAuth.credential().vault_key(), Some(("bolt", "session")));
        assert_eq!(MarineAuth.credential().vault_key(), Some(("marinetraffic", "cf_cookie")));
        assert_eq!(FlightAuth.credential().vault_key(), None);
        assert!(!BoltAuth.credential().is_pasted(), "a session is negotiated, never typed");
        assert!(!MarineAuth.credential().is_pasted(), "the cookie is the system's own work");
        assert!(!FlightAuth.needs_credential(), "the flight feed is public");
        assert!(BoltAuth.needs_credential() && MarineAuth.needs_credential());
    }

    #[test]
    fn otp_start_describes_the_vendor_request() {
        let req = BoltAuth.otp_start("+255700000000", &device(), 3).unwrap();
        assert_eq!(req.key, "bolt/otp-start3");
        assert!(req.url.starts_with("https://user.live.boltsvc.net/profile/verification/start/v2?"), "{}", req.url);
        assert!(req.url.contains("country=tz") && req.url.contains("deviceId=tz-device-1"));
        match req.method {
            AuthMethod::Post { body, content_type } => {
                assert_eq!(content_type, "application/json");
                assert!(body.contains(r#""phone_number":"+255700000000""#), "{body}");
                assert!(body.contains(r#""type":"sms""#) && body.contains(r#""method":"sms""#));
                assert!(body.contains(r#""timezone":"Africa/Dar_es_Salaam""#));
            }
            other => panic!("expected a POST, got {other:?}"),
        }
        assert!(BoltAuth.otp_start("  ", &device(), 1).is_err(), "no phone, no code");
    }

    #[test]
    fn otp_confirm_wants_exactly_four_digits() {
        let pending = PendingSession {
            session_id: "tz-device-1".into(),
            device_id: "tz-device-1".into(),
            phone: "+255700000000".into(),
            channel: "sms".into(),
            created_ms: 0,
        };
        assert!(BoltAuth.otp_confirm(&pending, "12", &device(), 1).is_err());
        assert!(BoltAuth.otp_confirm(&pending, "abcd", &device(), 1).is_err());
        assert!(BoltAuth.otp_confirm(&pending, "00f0", &device(), 1).is_err());
        let req = BoltAuth.otp_confirm(&pending, "4321", &device(), 5).unwrap();
        assert_eq!(req.key, "bolt/otp-confirm5");
        assert!(req.url.contains("profile/verification/confirm/v3?"), "{}", req.url);
        match req.method {
            AuthMethod::Post { body, .. } => {
                assert!(body.contains(r#""code":"4321""#), "{body}");
                assert!(body.contains(r#""phone_number":"+255700000000""#));
            }
            other => panic!("expected a POST, got {other:?}"),
        }
    }

    #[test]
    fn the_confirm_answer_mints_a_session_wherever_the_username_hides() {
        let pending = PendingSession {
            session_id: "dev".into(),
            device_id: "dev".into(),
            phone: "+255700000000".into(),
            channel: "sms".into(),
            created_ms: 42,
        };
        // `data.auth.auth_username` — one of the shapes the vendor has shipped.
        let ok = br#"{"code":0,"message":"OK","data":{"auth":{"auth_username":"uid_123"}}}"#;
        let session = bolt_confirm_answer(&pending, ok).unwrap();
        assert_eq!(session.auth_username, "uid_123");
        assert_eq!(session.device_id, "dev");
        assert_eq!(session.created_ms, 42);
        // `data.auth_username` — the other shape.
        let flat = br#"{"code":0,"message":"ok","data":{"auth_username":"uid_9"}}"#;
        assert_eq!(bolt_confirm_answer(&pending, flat).unwrap().auth_username, "uid_9");
        let bad = br#"{"code":11,"message":"invalid code"}"#;
        let err = bolt_confirm_answer(&pending, bad).unwrap_err();
        assert!(err.contains("rejected") && err.contains("invalid code"), "{err}");
        // Success without a username is named by its shape, values never quoted.
        let hollow = br#"{"code":0,"message":"OK","data":{"other":1}}"#;
        let err = bolt_confirm_answer(&pending, hollow).unwrap_err();
        assert!(err.contains("keys received") && err.contains("other"), "{err}");
        assert!(!err.contains("uid_"), "session tokens never appear in errors");
    }

    #[test]
    fn the_bolt_poll_rides_basic_auth_over_the_viewport() {
        let session = BoltSession {
            session_id: "sess".into(),
            device_id: "dev".into(),
            auth_username: "uid_7".into(),
            phone: "+255700000000".into(),
            created_ms: 0,
        };
        let req = BoltAuth.poll(&session.to_secret(), SINZA, 16.0, 9).unwrap();
        assert_eq!(req.key, "bolt/poll9");
        assert!(req.url.contains("mobility/search/poll?"), "{}", req.url);
        assert!(req.url.contains("user_id=uid_7") && req.url.contains("deviceId=dev"));
        match &req.body {
            Some((body, ct)) => {
                assert_eq!(ct, "application/json");
                assert!(body.contains(r#""stage":"category_selection""#), "{body}");
                assert!(body.contains(r#""south_west""#), "the viewport rides the body");
                // The body must PARSE — a raw-string line continuation once rode as a
                // literal backslash and the vendor answered 702 INVALID_REQUEST forever.
                let parsed: serde_json::Value = serde_json::from_str(body).expect("the poll body is valid JSON");
                assert_eq!(parsed["stage"], "category_selection");
                assert!(parsed["viewport"]["south_west"]["lat"].as_f64().is_some());
                assert!(!body.contains('\\'), "no stray backslashes in the body: {body}");
            }
            None => panic!("the poll is a POST with a body"),
        }
        let basic = session.basic_auth();
        assert_eq!(basic, base64::engine::general_purpose::STANDARD.encode("uid_7:dev"), "Basic base64(auth_username:device_id)");
        assert!(req.headers.iter().any(|(k, v)| k == "Authorization" && v.as_str() == format!("Basic {basic}")));
        // An unreadable session refuses to poll rather than riding garbage.
        let err = BoltAuth.poll("not json", SINZA, 16.0, 1).unwrap_err();
        assert!(err.contains("sign in again"), "{err}");
    }

    #[test]
    fn the_marine_poll_rides_the_mobile_tile_api_and_its_cookie() {
        assert!(MarineAuth.bootstrap(1).is_some(), "the system mints its own cookie");
        assert_eq!(MarineAuth.bootstrap(2).unwrap().key, "marine-bootstrap2");
        let req = MarineAuth.poll("mstc=1; __cf_bm=9", SINZA, 16.0, 4).unwrap();
        assert_eq!(req.key, "marine/poll4");
        assert!(req.url.starts_with("https://www.marinetraffic.com/getData/get_data_json_4_mob/z:7/"), "{}", req.url);
        assert!(req.url.contains("X:39") || req.url.contains("X:38"), "the tile x comes from the view: {}", req.url);
        assert!(req.headers.iter().any(|(k, v)| k == "Cookie" && v.contains("__cf_bm=9")));
        assert!(req.headers.iter().any(|(k, _)| k == "vessel-image"));
        assert!(req.body.is_none(), "the tile ask is a plain GET");
    }

    #[test]
    fn the_flight_poll_is_public_but_reads_like_the_web_client() {
        assert!(FlightAuth.bootstrap(1).is_none(), "nothing to mint");
        assert!(FlightAuth.otp_start("+255700000000", &device(), 1).is_err());
        let bare = FlightAuth.poll("", SINZA, 16.0, 2).unwrap();
        assert!(bare.url.contains("bbox=-6.740,-6.800,39.180,39.270"), "{}", bare.url);
        assert!(bare.url.contains("adsb=1") && bare.url.contains("stats=0"));
        assert!(!bare.headers.iter().any(|(k, _)| k == "Cookie"), "no credential, no cookie");
        assert!(bare.headers.iter().any(|(k, v)| k == "Origin" && v.contains("flightradar24")));
        let cookied = FlightAuth.poll("a=1", SINZA, 16.0, 3).unwrap();
        assert!(cookied.headers.iter().any(|(k, _)| k == "Cookie"), "a stored cookie still rides");
    }

    #[test]
    fn country_and_timezone_infer_from_the_view() {
        assert_eq!(phone_country(-6.778, 39.223), "tz");
        assert_eq!(phone_country(-1.28, 36.82), "ke");
        assert_eq!(phone_country(51.5, -0.12), "gb");
        assert_eq!(phone_country(59.43, 24.75), "ee", "Tallinn is a Bolt market");
        assert_eq!(phone_country(52.52, 13.40), "de", "Berlin too");
        assert_eq!(phone_country(45.46, 9.19), "it");
        assert_eq!(phone_country(40.7, -74.0), "us");
        assert_eq!(timezone_for_country("tz"), "Africa/Dar_es_Salaam");
        assert_eq!(timezone_for_country("ke"), "Africa/Nairobi");
        assert_eq!(timezone_for_country("ee"), "Europe/Tallinn");
        assert_eq!(timezone_for_country("bg"), "Europe/Sofia");
        assert_eq!(timezone_for_country("zz"), "UTC");
    }

    #[test]
    fn bolt_categories_pick_the_marker_kind() {
        assert_eq!(bolt_kind_for("boda_boda", ""), VehicleKind::Motorcycle);
        assert_eq!(bolt_kind_for("", "278"), VehicleKind::Motorcycle, "the icon id table knows the boda");
        assert_eq!(bolt_kind_for("taxi", "178"), VehicleKind::Motorcycle, "bajaji rides the bike icon");
        assert_eq!(bolt_kind_for("", "201"), VehicleKind::Van);
        assert_eq!(bolt_kind_for("premium", "231"), VehicleKind::Car);
        assert_eq!(bolt_type_name("8749"), Some("Send Motorbike"));
        assert_eq!(bolt_type_name("99999"), None, "zone-specific ids stay unknown");
    }

    #[test]
    fn the_vendors_own_groups_drive_the_kind() {
        // The `category_details` groups the vendor sends — Dar es Salaam and Europe both.
        assert_eq!(bolt_group_kind("motorbike"), Some(VehicleKind::Motorcycle));
        assert_eq!(bolt_group_kind("delivery_motorbike"), Some(VehicleKind::Motorcycle));
        assert_eq!(bolt_group_kind("autorickshaw"), Some(VehicleKind::Motorcycle), "the bajaji");
        assert_eq!(bolt_group_kind("xl"), Some(VehicleKind::Van));
        assert_eq!(bolt_group_kind("economy"), Some(VehicleKind::Car));
        assert_eq!(bolt_group_kind("premium"), Some(VehicleKind::Car));
        assert_eq!(bolt_group_kind("comfort"), Some(VehicleKind::Car));
        assert_eq!(bolt_group_kind("luxury"), Some(VehicleKind::Car));
        assert_eq!(bolt_group_kind("priority"), Some(VehicleKind::Car));
        assert_eq!(bolt_group_kind("female"), Some(VehicleKind::Car), "Women for Women rides a car");
        assert_eq!(bolt_group_kind(""), None, "no group, no opinion");
    }

    #[test]
    fn sessions_roundtrip_through_the_secret_form() {
        let session = BoltSession { session_id: "s".into(), device_id: "d".into(), auth_username: "u".into(), phone: "+1".into(), created_ms: 7 };
        assert_eq!(BoltSession::parse(&session.to_secret()), Some(session));
        assert_eq!(BoltSession::parse("  "), None);
        assert_eq!(BoltSession::parse("{}"), None, "a hollow secret is not a session");
    }

    #[test]
    fn the_trust_generates_the_basic_code_and_a_real_session() {
        let trust = BoltTrust {
            auth_username: "uid_247899280".into(),
            device_uuid: "F4669DCE-9F61-480A-9C02-345F12A78C32".into(),
            phone: "auto sign-in".into(),
        };
        // The generated code IS the documented Basic header (the deployment's own pair).
        use base64::Engine as _;
        assert_eq!(
            trust.basic_code(),
            base64::engine::general_purpose::STANDARD.encode("uid_247899280:F4669DCE-9F61-480A-9C02-345F12A78C32"),
            "basic_code is base64(username:device_uuid)"
        );
        // The minted secret parses back to a session whose poll header is the same code.
        let secret = trust.session_secret();
        let session = BoltSession::parse(&secret).expect("the trust's secret parses");
        assert_eq!(session.auth_username, trust.auth_username);
        assert_eq!(session.device_id, trust.device_uuid);
        assert_eq!(session.basic_auth(), trust.basic_code(), "the poll rides the generated code");
        // The device row matches too (the vault keeps the pair together).
        assert_eq!(trust.device_secret(), trust.device_uuid);
    }

    #[test]
    fn a_basic_header_decodes_to_its_pair() {
        use base64::Engine as _;
        let enc = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        assert_eq!(decode_basic(&enc("uid_7:dev-uuid")), Some(("uid_7".into(), "dev-uuid".into())));
        assert_eq!(decode_basic(&format!("Basic {}", enc("uid_7:dev"))), Some(("uid_7".into(), "dev".into())));
        assert_eq!(decode_basic("not base64!"), None);
        assert_eq!(decode_basic(&enc("no-colon-here")), None, "no colon, no pair");
    }

    #[test]
    fn the_trust_reads_the_environment_pair() {
        // The pure core `from_env` reads (the env read itself is not unit-testable here —
        // the workspace denies unsafe code, which `env::set_var` needs in edition 2024).
        let trust = BoltTrust::from_parts(None, Some("uid_test"), Some("uuid-test"), None).expect("the pair signs in");
        assert_eq!(trust.auth_username, "uid_test");
        assert_eq!(trust.device_uuid, "uuid-test");
        assert_eq!(trust.phone, "auto sign-in", "no phone label, the placeholder rides");
        // A ready Basic header answers alone (the raw-token form).
        use base64::Engine as _;
        let header = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("uid_hdr:dev-hdr"));
        let trust = BoltTrust::from_parts(Some(&header), None, None, Some("+255700000000")).expect("the header signs in");
        assert_eq!(trust.auth_username, "uid_hdr");
        assert_eq!(trust.device_uuid, "dev-hdr");
        assert_eq!(trust.phone, "+255700000000");
        // Nothing given, nothing minted; and an empty part is as good as none.
        assert!(BoltTrust::from_parts(None, None, None, None).is_none());
        assert!(BoltTrust::from_parts(None, Some("  "), Some("uuid"), None).is_none(), "blank username, no trust");
        assert!(BoltTrust::from_parts(None, Some("uid"), Some(""), None).is_none(), "blank device, no trust");
    }
}
