//! The Fleet commands: sign a feed in or out, store the system's own session for the managed
//! radars, toggle a row's eye, run the demo feed, set the poll cadence, ask for a poll now, and
//! read the feeds' status. The commands keep the layer tree in step — the "Fleet" group is part
//! of every document like the "Map" layer: the system-managed radars (Marine, Flight) hold
//! locked sub-layers from the start, Bolt joins when its user signs in, the demo when it runs.

use std::sync::Arc;

use serde_json::{Value, json};

use terrazon_doc::{Document, LayerColor, Node, NodeId, NodeKind};
use terrazon_fleet::{Auth, FeedStatus, FleetState, Provider};

use super::*;

fn fleet_of(d: &mut Document) -> &mut FleetState {
    &mut d.fleet
}

fn provider_param(cmd: &str, p: &Value) -> std::result::Result<Provider, EngineError> {
    let s = str_param(p, "provider").ok_or_else(|| bad(cmd, "give the provider: \"bolt\", \"marine\", \"flight\" or \"sgr\""))?;
    Provider::parse(s).ok_or_else(|| bad(cmd, format!("no fleet provider called `{s}` (bolt, marine, flight, sgr)")))
}

/// The Fleet group node's index among the top-level layers.
fn fleet_group_index(d: &Document) -> Option<usize> {
    d.layers.iter().position(|n| n.name.as_deref() == Some("Fleet"))
}

/// What each feed draws, as the layer registry's rows under its feed row: everything the
/// canvas shows for a feed is registered where the feed is — the vehicles, the H3 cells
/// they ride, the SGR's rail atlas, stations and trains — each with its own eye.
pub(crate) fn parts_for(provider: Option<Provider>) -> &'static [&'static str] {
    match provider {
        Some(Provider::Bolt) => &["Vehicles", "H3 Cells"],
        Some(Provider::Marine) => &["Vessels", "H3 Cells"],
        Some(Provider::Flight) => &["Aircraft", "H3 Cells"],
        Some(Provider::Sgr) => &["Rail Atlas", "Stations", "Trains"],
        None => &["Markers", "H3 Cells"], // the demo feed
    }
}

/// Bring the layer tree in step with the fleet config: the "Fleet" group is part of every
/// document (the system-managed radars ride with it), each live feed holds one locked
/// sub-layer node (Bolt, Marine Radar, Flight Radar, Demo) like the tiles sit locked under
/// "Map", and every drawn artifact holds its own locked row under its feed ("Vehicles",
/// "H3 Cells", the SGR's "Rail Atlas", "Stations", "Trains"). An old file whose fleet block
/// lacks the system layers is healed here too.
///
/// Returns the ids of the rows this sync **created** (the group's first one included), so the
/// runtime can open them in the Layers panel — a row born folded reads as missing.
pub(crate) fn sync_fleet_tree(d: &mut Document) -> Vec<NodeId> {
    let mut created = Vec::new();
    // The system-managed radars exist as long as the document does (a layer with no credential
    // yet becomes a pending system session; an explicit cookie override is left alone). The
    // SGR sweep is public — it just rides with the document like they do.
    ensure_system_layer(d, Provider::Marine);
    ensure_system_layer(d, Provider::Flight);
    ensure_system_layer(d, Provider::Sgr);
    let gi = match fleet_group_index(d) {
        Some(i) => i,
        None => {
            let id = d.alloc_id();
            let mut group = Node::layer(id, "Fleet", LayerColor::Preset(1));
            // Locked: a system group. New art keeps landing on the drawing layers (Map and
            // friends) — `default_layer` skips locked layers — and the panel ops can't drag,
            // merge or delete the feeds' rows out from under the runtime.
            group.locked = true;
            d.layers.push(Arc::new(group));
            created.push(id);
            d.layers.len() - 1
        }
    };
    // Keep an existing group locked too (a file saved by an older build may have it unlocked).
    if !d.layers[gi].locked {
        let node = Arc::make_mut(&mut d.layers[gi]);
        node.locked = true;
    }
    // What the tree should hold. The system-managed radars are always on; Bolt joins with the
    // user's sign-in, the demo with the switch. Only feeds that are **off** drop their rows;
    // anything the user added under the group stays (the group is theirs to draw in too).
    let want: Vec<(&'static str, bool)> = vec![
        ("Bolt", d.fleet.layer(Provider::Bolt).is_some_and(|l| l.auth.is_set())),
        ("Marine Radar", true),
        ("Flight Radar", true),
        ("SGR Trains", true),
        ("Demo", d.fleet.demo),
    ];
    let drop: Vec<&'static str> = want.iter().filter(|(_, on)| !on).map(|(name, _)| *name).collect();
    let missing: Vec<&'static str> = {
        let node = Arc::make_mut(&mut d.layers[gi]);
        let NodeKind::Layer { children, .. } = &mut node.kind else { return created };
        children.retain(|n| !drop.contains(&n.name.as_deref().unwrap_or_default()));
        want.iter().filter(|(name, on)| *on && !children.iter().any(|n| n.name.as_deref() == Some(name))).map(|(name, _)| *name).collect()
    };
    // One id per missing child from the document's allocator (the node borrow ended above).
    if !missing.is_empty() {
        let new: Vec<(&'static str, _)> = missing.into_iter().map(|name| (name, d.alloc_id())).collect();
        let node = Arc::make_mut(&mut d.layers[gi]);
        let NodeKind::Layer { children, .. } = &mut node.kind else { return created };
        for (name, id) in new {
            let mut child = Node::layer(id, name, LayerColor::Preset(1));
            child.locked = true;
            child.visible = true;
            children.push(Arc::new(child));
            created.push(id);
        }
    }
    // The drawn artifacts: every feed row holds the rows of what it draws, each locked (the
    // runtime owns their existence) and visible (the eye is the user's control). A part row
    // missing after a delete comes back with the next sync — the registry always matches
    // what the canvas can draw. Planned in one pass (which parts are missing per feed), the
    // ids allocated apart from the tree (the allocator and the tree are both `d`), then one
    // pass pushes the rows.
    let planned: Vec<(NodeId, Vec<&'static str>)> = {
        let node = &d.layers[gi];
        let NodeKind::Layer { children, .. } = &node.kind else { return created };
        let mut planned: Vec<(NodeId, Vec<&'static str>)> = Vec::new();
        for feed in children {
            let name = feed.name.clone().unwrap_or_default();
            let parts = match name.as_str() {
                "Demo" => parts_for(None),
                other => match Provider::parse(other) {
                    Some(p) => parts_for(Some(p)),
                    None => continue, // a user row under the group: not the runtime's business
                },
            };
            let NodeKind::Layer { children: pc, .. } = &feed.kind else { continue };
            let missing: Vec<&'static str> = parts.iter().filter(|p| !pc.iter().any(|n| n.name.as_deref() == Some(**p))).copied().collect();
            if !missing.is_empty() {
                planned.push((feed.id, missing));
            }
        }
        planned
    };
    if !planned.is_empty() {
        // One id per missing part, before the tree borrow takes `d`.
        let mut rows: Vec<(NodeId, Vec<(&'static str, NodeId)>)> =
            planned.into_iter().map(|(feed_id, names)| (feed_id, names.into_iter().map(|p| (p, d.alloc_id())).collect())).collect();
        let mut born_parents: Vec<NodeId> = Vec::new();
        let node = Arc::make_mut(&mut d.layers[gi]);
        let NodeKind::Layer { children, .. } = &mut node.kind else { return created };
        for (feed_id, parts) in rows.drain(..) {
            let Some(feed) = children.iter_mut().find(|n| n.id == feed_id) else { continue };
            let feed = Arc::make_mut(feed);
            let NodeKind::Layer { children: pc, .. } = &mut feed.kind else { continue };
            for (part, pid) in parts {
                let mut child = Node::layer(pid, part, LayerColor::Preset(1));
                child.locked = true;
                child.visible = true;
                pc.push(Arc::new(child));
                created.push(pid);
            }
            // Parts appear under a feed that may be folded: open the feed once, when its
            // first part is born (a one-time upgrade; after that the user's folds rule).
            born_parents.push(feed_id);
        }
        created.extend(born_parents);
    }
    created
}

/// The rows the sync created open in the Layers panel (a row born folded reads as missing —
/// the drawn things must be seen on the layer registry the moment they exist).
pub(crate) fn open_fleet_rows(s: &mut Session, ids: &[NodeId]) {
    if ids.is_empty() {
        return;
    }
    if let Ok(st) = s.doc_mut() {
        for id in ids {
            st.layers_open.set(*id, true);
        }
    }
}

/// The marine/flight layer the system manages: create it when the document's fleet lacks it
/// (an old file), leave an explicit credential alone.
fn ensure_system_layer(d: &mut Document, provider: Provider) -> &mut terrazon_fleet::FleetLayer {
    let l = fleet_of(d).layer_mut(provider);
    if matches!(l.auth, Auth::None | Auth::Token(_)) {
        l.auth = Auth::System(String::new());
        l.status = FeedStatus::Connecting;
        l.visible = true;
    }
    l
}

fn set_auth(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "fleet.setAuth";
    let provider = provider_param(C, p)?;
    let token = str_param(p, "token");
    let cookie = str_param(p, "cookie");
    let user = str_param(p, "user");
    let pass = str_param(p, "pass");
    let endpoint = str_param(p, "endpoint").map(str::to_string);
    // Bolt signs in with a session token; the radars carry the browser cookie. A user + pass
    // pair stores the login it would send (the app's login exchange fills the token).
    let auth = match (token, cookie, (user, pass)) {
        (Some(t), _, _) if !t.trim().is_empty() => Auth::Token(t.trim().to_string()),
        (_, Some(c), _) if !c.trim().is_empty() => Auth::Cookie(c.trim().to_string()),
        (_, _, (Some(u), Some(pw))) if !u.trim().is_empty() && !pw.trim().is_empty() => Auth::Cookie(format!("session={u}")),
        _ => return Err(bad(C, "give token (bolt), cookie (marine/flight) or user + pass (bolt login)")),
    };
    let mut out = String::new();
    let mut created = Vec::new();
    s.edit("Fleet Sign-in", |d, _| {
        let l = fleet_of(d).layer_mut(provider);
        l.auth = auth;
        if endpoint.is_some() {
            l.endpoint = endpoint;
        }
        l.status = FeedStatus::Connecting;
        l.visible = true;
        out = format!("{:?}", FeedStatus::Connecting).to_lowercase();
        created = sync_fleet_tree(d);
        Ok(())
    })?;
    open_fleet_rows(s, &created);
    Ok(json!({"provider": provider.label(), "status": out}))
}

fn clear_auth(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "fleet.clearAuth";
    let provider = provider_param(C, p)?;
    let mut created = Vec::new();
    s.edit("Fleet Sign-out", |d, _| {
        if provider.bootstrap_url().is_some() || provider == Provider::Sgr {
            // A system-managed radar never leaves: the session resets and the system quietly
            // re-bootstraps it (the row stays; the user never typed a cookie to forget). The
            // SGR sweep is public — its reset drops the timetable and the sweep rebuilds it.
            let l = ensure_system_layer(d, provider);
            l.auth = Auth::System(String::new());
            l.status = FeedStatus::Connecting;
            l.markers.clear();
            l.schedule = None;
        } else {
            fleet_of(d).remove_layer(provider);
        }
        created = sync_fleet_tree(d);
        Ok(())
    })?;
    open_fleet_rows(s, &created);
    ok()
}

/// Store the session the system obtained itself (`FleetMsg::Done`'s `set-cookie` lands here
/// from the runtime). Marine/Flight only — Bolt's credential is the user's to keep, never the
/// system's to mint.
fn system_auth(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "fleet.systemAuth";
    let provider = provider_param(C, p)?;
    if provider.bootstrap_url().is_none() {
        return Err(bad(C, "only the system-managed feeds take a system session (marine, flight)"));
    }
    let cookie = str_param(p, "cookie").unwrap_or_default().trim().to_string();
    let mut created = Vec::new();
    s.edit("Fleet System Session", |d, _| {
        let l = ensure_system_layer(d, provider);
        l.auth = Auth::System(cookie);
        l.status = FeedStatus::Connecting;
        l.visible = true;
        created = sync_fleet_tree(d);
        Ok(())
    })?;
    open_fleet_rows(s, &created);
    Ok(json!({"provider": provider.label(), "status": "connecting", "managed": true}))
}

/// The SGR timetable the sweep built lands here from the runtime (the rail module parsed the
/// vendors' answers and merged them): the document keeps the timetable, so the trains ride
/// their own times on every canvas and a saved map file keeps its schedule.
fn sgr_schedule(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "fleet.sgrSchedule";
    let timetable: terrazon_fleet::SgrTimetable =
        serde_json::from_value(p.clone()).map_err(|e| bad(C, format!("the body is the rail module's timetable JSON: {e}")))?;
    let (date, stations, trains) = (timetable.date.clone(), timetable.stations.len(), timetable.trains.len());
    let mut created = Vec::new();
    s.edit_background(|d, _| {
        let l = fleet_of(d).layer_mut(Provider::Sgr);
        l.schedule = Some(timetable);
        l.status = FeedStatus::Ready;
        l.visible = true;
        created = sync_fleet_tree(d);
        Ok(())
    })?;
    open_fleet_rows(s, &created);
    Ok(json!({"date": date, "stations": stations, "trains": trains}))
}

fn toggle(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "fleet.toggle";
    let raw = str_param(p, "provider").ok_or_else(|| bad(C, "give the feed: bolt, marine, flight, sgr or demo"))?;
    let visible = p.get("visible").and_then(Value::as_bool);
    let is_demo = raw.eq_ignore_ascii_case("demo");
    let provider = Provider::parse(raw);
    if !is_demo && provider.is_none() {
        return Err(bad(C, format!("no fleet feed called `{raw}` (bolt, marine, flight, sgr, demo)")));
    }
    let node_name = if is_demo { "Demo".to_string() } else { provider.map(|prov| prov.label().to_string()).unwrap_or_default() };
    let mut out = false;
    let mut created = Vec::new();
    s.edit("Show Fleet Feed", |d, _| {
        // The layer data's eye and the tree node's eye move together (one truth, two views).
        if let Some(prov) = provider {
            let l = fleet_of(d).layer_mut(prov);
            l.visible = visible.unwrap_or(!l.visible);
            out = l.visible;
        } else {
            let st = fleet_of(d);
            st.demo_visible = visible.unwrap_or(!st.demo_visible);
            out = st.demo_visible;
        }
        created = sync_fleet_tree(d);
        let Some(gi) = fleet_group_index(d) else {
            return Err(bad(C, "the Fleet group is gone (sign a feed in first)"));
        };
        let node = Arc::make_mut(&mut d.layers[gi]);
        let NodeKind::Layer { children, .. } = &mut node.kind else {
            return Err(bad(C, "the Fleet group does not hold children"));
        };
        if let Some(pos) = children.iter().position(|n| n.name.as_deref() == Some(node_name.as_str())) {
            let child = Arc::make_mut(&mut children[pos]);
            child.visible = out;
        }
        Ok(())
    })?;
    open_fleet_rows(s, &created);
    Ok(json!({"feed": node_name, "visible": out}))
}

fn demo(s: &mut Session, p: &Value) -> Result<Value> {
    let on = p.get("on").and_then(Value::as_bool);
    let mut now = false;
    let mut created = Vec::new();
    s.edit("Fleet Demo Feed", |d, _| {
        let st = fleet_of(d);
        st.demo = on.unwrap_or(!st.demo);
        st.demo_visible = true;
        now = st.demo;
        created = sync_fleet_tree(d);
        Ok(())
    })?;
    open_fleet_rows(s, &created);
    Ok(json!({"demo": now}))
}

fn set_poll(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "fleet.setPoll";
    let secs = p.get("secs").and_then(Value::as_u64).ok_or_else(|| bad(C, "give secs: 5–120"))?;
    if !(5..=120).contains(&secs) {
        return Err(bad(C, "secs is 5–120"));
    }
    s.edit("Fleet Poll Interval", |d, _| {
        fleet_of(d).poll_secs = secs as u32;
        Ok(())
    })?;
    ok()
}

fn poll_now(s: &mut Session, p: &Value) -> Result<Value> {
    let only = str_param(p, "provider").and_then(Provider::parse);
    s.edit("Poll Fleet", |d, _| {
        for l in &mut fleet_of(d).layers {
            if only.is_none_or(|prov| l.provider == prov) && l.wants_fetch() {
                l.status = FeedStatus::Connecting;
            }
        }
        fleet_of(d).poll_due = true;
        Ok(())
    })?;
    ok()
}

/// Ensure the Fleet layer tree is in step (the runtime's background call — a document created
/// or opened by the engine picks its Fleet group up on the next frame, without an undo step).
fn sync_tree(s: &mut Session, _p: &Value) -> Result<Value> {
    let mut created = Vec::new();
    s.edit_background(|d, _| {
        created = sync_fleet_tree(d);
        Ok(())
    })?;
    open_fleet_rows(s, &created);
    ok()
}

fn sources_query(s: &mut Session, _p: &Value) -> Result<Value> {
    let st = s.doc_mut()?;
    let fleet = &st.doc.fleet;
    let status_name = |st: &FeedStatus| match st {
        FeedStatus::Off => "off",
        FeedStatus::Connecting => "connecting",
        FeedStatus::Ready => "ready",
        FeedStatus::Error(_) => "error",
    };
    Ok(json!({
        "configured": fleet.configured(),
        "demo": fleet.demo,
        "pollSecs": fleet.poll_secs,
        "markers": fleet.marker_count(),
        "layers": fleet.layers.iter().map(|l| json!({
            "provider": l.provider.label(),
            "visible": l.visible,
            "authSet": l.auth.is_set(),
            "managed": l.auth.is_system(),
            "endpoint": l.endpoint.clone().unwrap_or_else(|| l.provider.default_endpoint().to_string()),
            "status": status_name(&l.status()),
            "error": match l.status() { FeedStatus::Error(e) => Some(e), _ => None },
            "markers": l.markers.len(),
            "trains": l.schedule.as_ref().map(|t| t.trains.len()),
        })).collect::<Vec<_>>(),
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "fleet.setAuth",
            "Fleet Sign-in",
            ["Map"],
            None,
            "{provider: \"bolt\"|\"marine\"|\"flight\"|\"sgr\", token?: the Bolt session token, cookie?: a cookie override (the radars normally manage their own session — the UI never asks), user? + pass?: the Bolt login, endpoint?: override the request URL template ({res}, {cells}, {z}, {bbox} are filled)} sign a fleet feed in: its locked sub-layer appears under the Fleet group and its markers draw once the feed answers → {provider, status}",
            has_doc,
            set_auth
        ),
        cmd!(
            "fleet.systemAuth",
            "Fleet System Session",
            ["Map"],
            None,
            "{provider: \"marine\"|\"flight\", cookie} store the session the system obtained itself from the vendor's bootstrap page (the runtime calls this when the handshake answers; Bolt is rejected — its token is the user's) → {provider, status, managed}",
            has_doc,
            system_auth
        ),
        cmd!(
            "fleet.clearAuth",
            "Fleet Sign-out",
            ["Map"],
            None,
            "{provider} forget a feed's auth — Bolt's sub-layer leaves the Fleet group; the system-managed radars reset their session and quietly re-bootstrap (the row stays)",
            has_doc,
            clear_auth
        ),
        cmd!(
            "fleet.toggle",
            "Show Fleet Feed",
            ["Map"],
            None,
            "{provider: \"bolt\"|\"marine\"|\"flight\"|\"sgr\"|\"demo\", visible?: left out: toggle} show or hide a feed's markers (the eye on its row under Fleet)",
            has_doc,
            toggle
        ),
        cmd!(
            "fleet.demo",
            "Fleet Demo Feed",
            ["Map"],
            None,
            "{on?: left out: toggle} the demo feed: deterministic sample traffic around the view (cars, motorcycles, vans, ships sized by the marine ladder), clearly labelled, so the Fleet layer shows before any vendor signs in → {demo}",
            has_doc,
            demo
        ),
        cmd!(
            "fleet.setPoll",
            "Fleet Poll Interval",
            ["Map"],
            None,
            "{secs: 5–120} how often the signed-in feeds poll for fresh markers",
            has_doc,
            set_poll
        ),
        cmd!(
            "fleet.poll",
            "Poll Fleet Now",
            ["Map"],
            None,
            "{provider?} ask the signed-in feeds for fresh markers at once (they otherwise poll every fleet.setPoll seconds); the next frame sends the requests",
            has_doc,
            poll_now
        ),
        cmd!(
            "fleet.sgrSchedule",
            "SGR Timetable",
            ["Map"],
            None,
            "{date, stations: [[id, title]…], trains: [{tripsNo, name, direction, stops: [{stationId, name, timeMin}…]}…]} the SGR sweep's merged timetable lands here from the runtime (the rail module parsed the vendor's answers): the document keeps it and the trains ride their own times → {date, stations, trains}",
            has_doc,
            sgr_schedule
        ),
        cmd!(
            "fleet.syncTree",
            "Fleet Layer Sync",
            ["Map"],
            None,
            "{} bring the layer tree in step with the fleet config without an undo step (the runtime calls this on the frame a document appears: new, opened or reverted docs pick up their Fleet group here) → ok",
            has_doc,
            sync_tree
        ),
        cmd!(
            query
            "fleet.sources",
            "Fleet Status",
            ["Map"],
            None,
            "{} every feed's auth, endpoint, status and marker count → {configured, demo, pollSecs, markers, layers: [{provider, visible, authSet, managed, endpoint, status, error?, markers}]}",
            has_doc,
            sources_query
        ),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use crate::Session;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", &json!({ "preset": "World Map" })).unwrap();
        s
    }

    fn fleet_group(s: &Session) -> Option<usize> {
        s.active().unwrap().doc.layers.iter().position(|n| n.name.as_deref() == Some("Fleet"))
    }

    fn fleet_children(s: &Session) -> Vec<String> {
        let i = fleet_group(s).unwrap();
        match &s.active().unwrap().doc.layers[i].kind {
            terrazon_doc::NodeKind::Layer { children, .. } => children.iter().map(|n| n.name.clone().unwrap_or_default()).collect(),
            _ => vec![],
        }
    }

    #[test]
    fn every_document_opens_with_the_system_radars_in_the_tree() {
        let mut s = session();
        // The engine leaves a fresh document alone; the runtime's frame-one sync brings the
        // Fleet group in (not an undo step).
        assert!(fleet_group(&s).is_none(), "the engine does not touch a doc nobody drew on yet");
        let undo_before = s.active().unwrap().history.undo.len();
        s.execute("fleet.syncTree", &json!({})).unwrap();
        assert_eq!(s.active().unwrap().history.undo.len(), undo_before, "the sync is background work");
        let i = fleet_group(&s).expect("the Fleet group is part of every document");
        assert_eq!(fleet_children(&s), vec!["Marine Radar", "Flight Radar", "SGR Trains"]);
        let group = &s.active().unwrap().doc.layers[i];
        assert!(group.locked, "the Fleet group is a locked system group (new art never lands in it)");
        match &group.kind {
            terrazon_doc::NodeKind::Layer { children, .. } => {
                assert!(children.iter().all(|n| n.locked), "the feed sub-layers are locked like the tiles");
                assert!(children.iter().all(|n| n.visible));
            }
            k => panic!("the Fleet node is a layer, got {k:?}"),
        }
        let q = s.execute("fleet.sources", &json!({})).unwrap();
        assert_eq!(q["configured"], true);
        let marine = q["layers"].as_array().unwrap().iter().find(|l| l["provider"] == "Marine Radar").unwrap().clone();
        assert_eq!(marine["managed"], true, "the system owns the session");
        assert_eq!(marine["authSet"], false, "the handshake hasn't answered yet");
        assert_eq!(marine["status"], "connecting");
    }

    /// A feed row's part rows by name (empty when the feed row is gone — a deleted feed
    /// takes its parts with it).
    fn fleet_parts(s: &Session, feed: &str) -> Vec<String> {
        let i = fleet_group(s).unwrap();
        match &s.active().unwrap().doc.layers[i].kind {
            terrazon_doc::NodeKind::Layer { children, .. } => match children.iter().find(|n| n.name.as_deref() == Some(feed)) {
                Some(f) => match &f.kind {
                    terrazon_doc::NodeKind::Layer { children, .. } => children.iter().map(|n| n.name.clone().unwrap_or_default()).collect(),
                    k => panic!("`{feed}` is not a layer, got {k:?}"),
                },
                None => vec![],
            },
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn every_drawn_artifact_registers_as_a_row_under_its_feed() {
        let mut s = session();
        s.execute("fleet.syncTree", &json!({})).unwrap();
        s.execute("fleet.setAuth", &json!({ "provider": "bolt", "token": "tok123" })).unwrap();
        s.execute("fleet.demo", &json!({ "on": true })).unwrap();
        assert_eq!(fleet_parts(&s, "Bolt"), vec!["Vehicles", "H3 Cells"]);
        assert_eq!(fleet_parts(&s, "Marine Radar"), vec!["Vessels", "H3 Cells"]);
        assert_eq!(fleet_parts(&s, "Flight Radar"), vec!["Aircraft", "H3 Cells"]);
        assert_eq!(fleet_parts(&s, "SGR Trains"), vec!["Rail Atlas", "Stations", "Trains"]);
        assert_eq!(fleet_parts(&s, "Demo"), vec!["Markers", "H3 Cells"]);
        // Every part is a locked, visible row — the runtime owns its existence, the eye is
        // the user's control.
        let i = fleet_group(&s).unwrap();
        match &s.active().unwrap().doc.layers[i].kind {
            terrazon_doc::NodeKind::Layer { children, .. } => {
                for feed in children {
                    match &feed.kind {
                        terrazon_doc::NodeKind::Layer { children: parts, .. } => {
                            assert!(parts.iter().all(|p| p.locked && p.visible), "{}'s parts lock and show", feed.name.clone().unwrap_or_default());
                        }
                        k => panic!("{k:?}"),
                    }
                }
            }
            k => panic!("{k:?}"),
        }
        // A deleted part comes back with the next sync (the registry matches the canvas).
        let gi = fleet_group(&s).unwrap();
        let sgr_id = match &s.active().unwrap().doc.layers[gi].kind {
            terrazon_doc::NodeKind::Layer { children, .. } => children.iter().find(|n| n.name.as_deref() == Some("SGR Trains")).unwrap().id,
            k => panic!("{k:?}"),
        };
        s.execute("layer.delete", &json!({ "id": sgr_id.0 })).unwrap();
        assert!(fleet_parts(&s, "SGR Trains").is_empty(), "the row (and its parts) went with it");
        s.execute("fleet.syncTree", &json!({})).unwrap();
        assert_eq!(fleet_parts(&s, "SGR Trains"), vec!["Rail Atlas", "Stations", "Trains"]);
    }

    #[test]
    fn the_rows_the_sync_created_open_in_the_layers_panel() {
        let mut s = session();
        // A fresh document's OpenRows hold the top-level layers only: the Fleet group is born
        // AFTER that, so the sync opens what it created — group, feeds and parts — or the
        // drawn things would ride in a folded group nobody can see.
        s.execute("fleet.syncTree", &json!({})).unwrap();
        let st = s.active().unwrap();
        let gi = st.doc.layers.iter().position(|n| n.name.as_deref() == Some("Fleet")).unwrap();
        assert!(st.layers_open.contains(st.doc.layers[gi].id), "the group shows its feeds");
        match &st.doc.layers[gi].kind {
            terrazon_doc::NodeKind::Layer { children, .. } => {
                for feed in children {
                    assert!(st.layers_open.contains(feed.id), "{} shows its parts", feed.name.clone().unwrap_or_default());
                }
            }
            k => panic!("{k:?}"),
        }
        // The user can fold a row again; the next sync (which creates nothing) leaves it be.
        let feed_id = match &st.doc.layers[gi].kind {
            terrazon_doc::NodeKind::Layer { children, .. } => children[0].id,
            k => panic!("{k:?}"),
        };
        s.doc_mut().unwrap().layers_open.set(feed_id, false);
        s.execute("fleet.syncTree", &json!({})).unwrap();
        let st = s.active().unwrap();
        assert!(!st.layers_open.contains(feed_id), "the user's fold survives a no-op sync");
    }

    #[test]
    fn a_user_row_under_the_fleet_group_survives_the_sync() {
        let mut s = session();
        s.execute("fleet.syncTree", &json!({})).unwrap();
        let gi = s.active().unwrap().doc.layers.iter().position(|n| n.name.as_deref() == Some("Fleet")).unwrap();
        let group_id = s.active().unwrap().doc.layers[gi].id.0;
        s.execute("layer.setProps", &json!({ "ids": [group_id], "locked": false })).unwrap();
        s.execute("layer.newSublayer", &json!({ "parent": group_id, "name": "My sketch" })).unwrap();
        s.execute("fleet.demo", &json!({ "on": true })).unwrap();
        s.execute("fleet.syncTree", &json!({})).unwrap();
        assert!(fleet_children(&s).contains(&"My sketch".to_string()), "the group is the user's to draw in too");
    }

    #[test]
    fn signing_in_adds_bolt_and_undo_removes_only_the_user_feed() {
        let mut s = session();
        s.execute("fleet.syncTree", &json!({})).unwrap();
        assert_eq!(fleet_children(&s), vec!["Marine Radar", "Flight Radar", "SGR Trains"]);
        s.execute("fleet.setAuth", &json!({ "provider": "bolt", "token": "tok123" })).unwrap();
        assert_eq!(fleet_children(&s), vec!["Marine Radar", "Flight Radar", "SGR Trains", "Bolt"]);
        let q = s.execute("fleet.sources", &json!({})).unwrap();
        let bolt = q["layers"].as_array().unwrap().iter().find(|l| l["provider"] == "Bolt").unwrap().clone();
        assert_eq!(bolt["authSet"], true);
        assert_eq!(bolt["managed"], false);
        assert_eq!(bolt["status"], "connecting");
        // Undo the sign-in: the group stays (the system radars hold it), only Bolt leaves.
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(fleet_children(&s), vec!["Marine Radar", "Flight Radar", "SGR Trains"]);
    }

    #[test]
    fn the_system_session_lands_and_reset_rebootstraps() {
        let mut s = session();
        s.execute("fleet.syncTree", &json!({})).unwrap();
        // Bolt never takes a system session: its credential is the user's to keep.
        assert!(s.execute("fleet.systemAuth", &json!({ "provider": "bolt", "cookie": "x" })).is_err());
        s.execute("fleet.systemAuth", &json!({ "provider": "marine", "cookie": "mstc=1; route=5" })).unwrap();
        let m = s.active().unwrap().doc.fleet.layer(terrazon_fleet::Provider::Marine).unwrap();
        assert_eq!(m.auth, terrazon_fleet::Auth::System("mstc=1; route=5".into()));
        assert_eq!(m.status, terrazon_fleet::FeedStatus::Connecting);
        // Reset: the row stays, the credential empties, the system re-bootstraps.
        s.execute("fleet.clearAuth", &json!({ "provider": "marine" })).unwrap();
        let m = s.active().unwrap().doc.fleet.layer(terrazon_fleet::Provider::Marine).unwrap();
        assert!(m.auth.is_system() && m.needs_bootstrap());
        assert_eq!(fleet_children(&s), vec!["Marine Radar", "Flight Radar", "SGR Trains"], "the row never leaves");
        // A cookie override still lands through setAuth (scripts), and reset takes it back too.
        s.execute("fleet.setAuth", &json!({ "provider": "marine", "cookie": "k=v" })).unwrap();
        let m = s.active().unwrap().doc.fleet.layer(terrazon_fleet::Provider::Marine).unwrap();
        assert_eq!(m.auth, terrazon_fleet::Auth::Cookie("k=v".into()));
        s.execute("fleet.clearAuth", &json!({ "provider": "marine" })).unwrap();
        assert!(s.active().unwrap().doc.fleet.layer(terrazon_fleet::Provider::Marine).unwrap().needs_bootstrap());
        // Bolt's sign-out still removes its row entirely.
        s.execute("fleet.setAuth", &json!({ "provider": "bolt", "token": "t" })).unwrap();
        s.execute("fleet.clearAuth", &json!({ "provider": "bolt" })).unwrap();
        assert_eq!(fleet_children(&s), vec!["Marine Radar", "Flight Radar", "SGR Trains"]);
    }

    #[test]
    fn the_sgr_timetable_lands_and_the_row_stays_through_a_reset() {
        let mut s = session();
        s.execute("fleet.syncTree", &json!({})).unwrap();
        let timetable = json!({
            "date": "2026-10-17",
            "stations": [[1, "Dar Es Salaam"], [12, "Dodoma"]],
            "trains": [{
                "tripsNo": "0011904",
                "name": "DAR -- DOM | EXPRESS",
                "direction": 1,
                "stops": [
                    {"stationId": 1, "name": "Dar Es Salaam", "timeMin": 360},
                    {"stationId": 12, "name": "Dodoma", "timeMin": 590},
                ],
            }],
        });
        let r = s.execute("fleet.sgrSchedule", &timetable).unwrap();
        assert_eq!(r["trains"], 1);
        assert_eq!(r["stations"], 2);
        let l = s.active().unwrap().doc.fleet.layer(terrazon_fleet::Provider::Sgr).unwrap();
        assert_eq!(l.status, terrazon_fleet::FeedStatus::Ready, "the sweep's answers make the row live");
        assert!(l.schedule.is_some());
        // A background step: no undo entry.
        let undo = s.active().unwrap().history.undo.len();
        s.execute("fleet.sgrSchedule", &timetable).unwrap();
        assert_eq!(s.active().unwrap().history.undo.len(), undo);
        // The reset keeps the row (the sweep rebuilds the timetable).
        s.execute("fleet.clearAuth", &json!({ "provider": "sgr" })).unwrap();
        let l = s.active().unwrap().doc.fleet.layer(terrazon_fleet::Provider::Sgr).unwrap();
        assert!(l.schedule.is_none());
        assert_eq!(l.status, terrazon_fleet::FeedStatus::Connecting);
        assert!(fleet_children(&s).contains(&"SGR Trains".to_string()));
        // And the query reports the trains count per row.
        s.execute("fleet.sgrSchedule", &timetable).unwrap();
        let q = s.execute("fleet.sources", &json!({})).unwrap();
        let sgr = q["layers"].as_array().unwrap().iter().find(|l| l["provider"] == "SGR Trains").unwrap().clone();
        assert_eq!(sgr["trains"], 1);
    }

    #[test]
    fn the_demo_is_a_sublayer_and_the_eye_toggles_both_sides() {
        let mut s = session();
        s.execute("fleet.syncTree", &json!({})).unwrap();
        s.execute("fleet.demo", &json!({ "on": true })).unwrap();
        assert_eq!(fleet_children(&s), vec!["Marine Radar", "Flight Radar", "SGR Trains", "Demo"]);
        s.execute("fleet.setAuth", &json!({ "provider": "flight", "cookie": "c" })).unwrap();
        let children = fleet_children(&s);
        assert!(children.contains(&"Demo".to_string()) && children.contains(&"Flight Radar".to_string()));
        // The eye: the layer's visible and the node's visible move together.
        let r = s.execute("fleet.toggle", &json!({ "provider": "demo" })).unwrap();
        assert_eq!(r["visible"], false);
        let st = s.active().unwrap();
        assert!(!st.doc.fleet.demo_visible);
        let i = fleet_group(&s).unwrap();
        match &st.doc.layers[i].kind {
            terrazon_doc::NodeKind::Layer { children, .. } => {
                let demo = children.iter().find(|n| n.name.as_deref() == Some("Demo")).unwrap();
                assert!(!demo.visible, "the tree node's eye follows");
            }
            k => panic!("{k:?}"),
        }
        // The demo off removes its sub-layer but the group (and the radars) stay.
        s.execute("fleet.demo", &json!({ "on": false })).unwrap();
        assert_eq!(fleet_children(&s), vec!["Marine Radar", "Flight Radar", "SGR Trains"]);
    }

    #[test]
    fn poll_asks_and_the_cadence_clamps() {
        let mut s = session();
        s.execute("fleet.setAuth", &json!({ "provider": "bolt", "token": "t" })).unwrap();
        s.execute("fleet.poll", &json!({})).unwrap();
        assert!(s.active().unwrap().doc.fleet.poll_due, "the next frame's tick picks it up");
        assert!(s.execute("fleet.setPoll", &json!({ "secs": 3 })).is_err(), "below the floor");
        assert!(s.execute("fleet.setPoll", &json!({ "secs": 400 })).is_err(), "above the ceiling");
        s.execute("fleet.setPoll", &json!({ "secs": 30 })).unwrap();
        assert_eq!(s.active().unwrap().doc.fleet.poll_secs, 30);
    }

    #[test]
    fn unknown_providers_and_empty_auth_are_rejected() {
        let mut s = session();
        assert!(s.execute("fleet.setAuth", &json!({ "provider": "uber", "token": "t" })).is_err());
        assert!(s.execute("fleet.setAuth", &json!({ "provider": "bolt", "token": "  " })).is_err());
        assert!(s.execute("fleet.setAuth", &json!({ "provider": "bolt" })).is_err());
        assert!(s.execute("fleet.toggle", &json!({ "provider": "nope" })).is_err());
    }
}
