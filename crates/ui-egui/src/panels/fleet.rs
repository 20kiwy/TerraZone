//! The Fleet panel — the live feeds in one place: each provider's status dot, marker count and
//! eye (the same eyes the layer panel's Fleet group holds), Bolt's sign-in editor, the endpoint
//! override, the demo feed and the poll cadence. Marine and Flight are **system-managed**: their
//! sessions are obtained and rotated by TerraZone itself, so no cookie is ever typed here —
//! their rows just show the handshake's progress. Everything mutates through the `fleet.*`
//! engine commands — the document's fleet state and its "Fleet" layer group are the one truth;
//! this panel edits it.

use egui::{Sense, Stroke, Ui, vec2};
use serde_json::json;

use terrazon_fleet::{FeedStatus, Provider};

use crate::theme::Tokens;
use crate::{TerrazoneApp, icons, widgets};

/// The panel id (the icon column and the Window menu key it by).
pub const ID: &str = "fleet";

/// What one click on a feed row asked for.
enum Action {
    None,
    Select,
    Toggle,
}

pub fn show(app: &mut TerrazoneApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    widgets::subheader(ui, tl!("Fleet"));
    let Some(_) = app.view() else {
        super::empty_state(ui, "globe", tl!("No document"), tl!("Open a map document to see its fleet."));
        return;
    };
    let Some(_st) = app.session.active() else { return };

    // One row per feed: the status dot, the name, the markers, the eye.
    for provider in Provider::ALL {
        let Some(st) = app.session.active() else { return };
        let l = st.doc.fleet.layer(provider);
        let managed = l.is_some_and(|l| l.auth.is_system());
        let pending = l.is_some_and(|l| l.needs_bootstrap());
        // The SGR sweep needs no credential — its truth is the cache's status, the same gate
        // the drawing uses (a timetable the sweep answered reads live like the radars do).
        let keyless = provider == Provider::Sgr;
        let auth_set = keyless || l.is_some_and(|l| l.auth.is_set());
        let visible = l.is_none_or(|l| l.visible);
        let count = app.canvas.fleet.markers_of(provider).len();
        // A managed feed whose handshake hasn't landed reads *Connecting* — the system is on it
        // (the cache can't know; it only speaks once requests fly).
        let status = if pending { FeedStatus::Connecting } else { app.canvas.fleet.status_of(provider) };
        let (dot, status_label) = match (auth_set, pending, &status) {
            (_, true, _) => (t.warning, tl!("Connecting")),
            (false, _, _) => (t.text_disabled, tl!("Off")),
            (true, _, FeedStatus::Connecting) => (t.warning, tl!("Connecting")),
            (true, _, FeedStatus::Ready) => (t.accent_strong, tl!("Live")),
            (true, _, FeedStatus::Error(_)) => (t.error, tl!("Error")),
            (true, _, FeedStatus::Off) => (t.text_disabled, tl!("Off")),
        };
        let selected = app.ui.fleet_sel.as_deref() == Some(provider.label());
        match feed_row(
            ui,
            &t,
            ("fleet", provider.label()),
            provider.label(),
            dot,
            status_label,
            if status.has_data() { Some(count) } else { None },
            visible,
            managed,
            selected,
        ) {
            Action::Toggle => {
                let _ = app.run("fleet.toggle", json!({ "provider": provider.label(), "visible": !visible }));
            }
            Action::Select => {
                app.ui.fleet_sel = if selected { None } else { Some(provider.label().to_string()) };
            }
            Action::None => {}
        }
    }

    // The demo row: the sample feed that shows the whole layer system before a vendor signs in.
    let demo = app.session.active().is_some_and(|st| st.doc.fleet.demo);
    let demo_visible = app.session.active().is_some_and(|st| st.doc.fleet.demo_visible);
    let demo_count = app.canvas.fleet.demo_markers.len();
    match feed_row(
        ui,
        &t,
        ("fleet-demo", "demo"),
        tl!("Demo feed"),
        if demo { t.text_dim } else { t.text_disabled },
        if demo { tl!("On") } else { tl!("Off") },
        demo.then_some(demo_count),
        demo_visible,
        false,
        false,
    ) {
        Action::Toggle => {
            let _ = app.run("fleet.toggle", json!({ "provider": "demo", "visible": !demo_visible }));
        }
        Action::Select => {
            let _ = app.run("fleet.demo", json!({ "on": !demo }));
        }
        Action::None => {}
    }

    ui.add_space(8.0);

    // The selected feed's editor: Bolt signs in; the radars explain their managed session.
    if let Some(name) = app.ui.fleet_sel.clone()
        && let Some(provider) = Provider::parse(&name)
    {
        let (pending, auth_set, endpoint) = {
            let Some(st) = app.session.active() else { return };
            let l = st.doc.fleet.layer(provider);
            (l.is_some_and(|l| l.needs_bootstrap()), l.is_some_and(|l| l.auth.is_set()), l.and_then(|l| l.endpoint.clone()).unwrap_or_default())
        };
        widgets::subheader(ui, provider.label());
        if provider == Provider::Bolt {
            bolt_editor(app, ui, auth_set);
        } else if provider == Provider::Sgr {
            sgr_editor(app, ui);
        } else {
            // The system-managed radars: nothing to type, ever.
            ui.add_space(2.0);
            widgets::dim_label(ui, tl!("Managed automatically — TerraZone obtains and rotates this feed's session itself; no cookie to paste."));
            if pending {
                widgets::dim_label(ui, tl!("Requesting a session…"));
            } else if auth_set {
                widgets::dim_label(ui, tl!("Session active."));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, tl!("Reset session")).clicked() {
                    let _ = app.run("fleet.clearAuth", json!({ "provider": provider.label() }));
                }
            });
        }
        // The endpoint override is for every feed (advanced; not a credential).
        let mut endpoint = app.ui.fleet_draft.endpoint.clone().unwrap_or(endpoint);
        let ep = ui
            .add(
                egui::TextEdit::singleline(&mut endpoint)
                    .hint_text(tl!("Endpoint ({res}, {cells}, {z}, {bbox})"))
                    .desired_width(ui.available_width()),
            )
            .on_hover_text(tl!("The request URL template; empty keeps the provider's default."));
        if ep.changed() {
            app.ui.fleet_draft.endpoint = Some(endpoint.clone());
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, tl!("Save endpoint")).clicked() {
                let mut params = json!({ "provider": provider.label() });
                if !endpoint.trim().is_empty() {
                    params["endpoint"] = json!(endpoint.trim());
                }
                if provider == Provider::Bolt {
                    // setAuth is the sign-in; an endpoint-only save re-signs with what's stored.
                    if let Some(t) = app.session.active().and_then(|st| st.doc.fleet.layer(provider)).map(|l| l.auth.secret().to_string())
                        && !t.is_empty()
                    {
                        params["token"] = json!(t);
                    }
                } else if let Some(c) = app.session.active().and_then(|st| st.doc.fleet.layer(provider)).map(|l| l.auth.secret().to_string())
                    && !c.trim().is_empty()
                {
                    // The radars keep their (system-minted) session across an endpoint save.
                    params["cookie"] = json!(c);
                }
                let _ = app.run("fleet.setAuth", params);
                app.ui.fleet_draft.endpoint = None;
            }
        });
        if let Some(st) = app.session.active()
            && let Some(l) = st.doc.fleet.layer(provider)
            && !l.needs_bootstrap()
            && let FeedStatus::Error(e) = app.canvas.fleet.status_of(l.provider)
        {
            widgets::dim_label(ui, &e);
        }
        ui.add_space(6.0);
    }

    // The poll cadence + a fetch-now button.
    let poll = app.session.active().map(|st| st.doc.fleet.poll_secs).unwrap_or(15);
    ui.horizontal(|ui| {
        let mut secs = poll;
        if ui.add(egui::Slider::new(&mut secs, 5..=120).text(tl!("Poll"))).changed() {
            let _ = app.run("fleet.setPoll", json!({ "secs": secs }));
        }
        ui.label(crate::i18n::fmt(tl!("Poll every {n} s"), &[("n", &secs.to_string())]));
    });
    ui.horizontal(|ui| {
        if widgets::secondary_button(ui, tl!("Fetch now")).clicked() {
            let _ = app.run("fleet.poll", json!({}));
        }
        let pending = app.canvas.fleet.pending_count();
        if pending > 0 {
            widgets::dim_label(ui, &crate::i18n::fmt(tl!("Asking {n} feeds…"), &[("n", &pending.to_string())]));
        }
    });
    ui.add_space(4.0);
    widgets::dim_label(
        ui,
        tl!(
            "A feed draws its markers only while its session is live and answering. Bolt keeps your sign-in; the radars' sessions manage themselves."
        ),
    );
}

/// The SGR editor: the sweep's progress and the timetable it built — the trains running
/// right now with their ETA at the next stop and at the terminal, computed from the vendor's
/// own schedule times (the trains ride those times along the OSM rail network) — then the
/// day's trains with the seat selection: each coach's free seats, a sold-out class reading
/// "No seat available", and the flow ending at the picked seat (booking needs the vendor's
/// own login, so TerraZone stops there honestly).
fn sgr_editor(app: &mut TerrazoneApp, ui: &mut Ui) {
    ui.add_space(2.0);
    let schedule = app.session.active().and_then(|st| st.doc.fleet.layer(Provider::Sgr)).and_then(|l| l.schedule.clone());
    let Some(tt) = schedule else {
        widgets::dim_label(ui, tl!("Mapping the SGR line — one station pair per poll; trains appear as the timetable fills in."));
        return;
    };
    widgets::dim_label(ui, &crate::i18n::fmt(tl!("Timetable for {date}"), &[("date", &tt.date)]));
    widgets::dim_label(
        ui,
        &crate::i18n::fmt(tl!("{n} stations · {m} trains"), &[("n", &tt.stations.len().to_string()), ("m", &tt.trains.len().to_string())]),
    );
    ui.add_space(4.0);
    let now = terrazon_fleet::rail::now_minutes_eat(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
    );
    let running: Vec<&terrazon_fleet::rail::SgrTrain> = tt.running(now);
    if running.is_empty() {
        widgets::dim_label(ui, tl!("No train on the line right now — the sweep keeps the timetable current."));
    } else {
        widgets::subheader(ui, tl!("Running now"));
        for train in &running {
            let Some((next, terminal, to_next, to_terminal)) = tt.progress(train, now) else { continue };
            ui.horizontal(|ui| {
                widgets::dim_label(ui, &train.name.to_string());
            });
            widgets::dim_label(ui, &crate::i18n::fmt(tl!("Next: {name} in {m} min"), &[("name", &next.name), ("m", &to_next.to_string())]));
            widgets::dim_label(ui, &crate::i18n::fmt(tl!("Arrives {name} in {m} min"), &[("name", &terminal.name), ("m", &to_terminal.to_string())]));
        }
    }
    ui.add_space(4.0);

    // The day's trains with the seat selection: the vendor reports each set's coaches with
    // their free seats, so the pick is the vendor's own availability — a sold-out class is
    // not selectable, and a picked seat ends the flow (booking is the vendor's login's work).
    widgets::subheader(ui, tl!("Today's trains"));
    let mut trains: Vec<&terrazon_fleet::rail::SgrTrain> = tt.trains.iter().collect();
    trains.sort_by_key(|t| t.stops.first().map(|s| s.time_min).unwrap_or(u32::MAX));
    for train in &trains {
        let (dep, arr) = match (train.stops.first(), train.stops.last()) {
            (Some(a), Some(b)) => (format_clock(a.time_min), format_clock(b.time_min)),
            _ => ("—".into(), "—".into()),
        };
        let expanded = app.ui.fleet_draft.sgr_train.as_deref() == Some(train.trips_no.as_str());
        let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), egui::Sense::click());
        let t = Tokens::get(ui.ctx());
        if expanded || resp.hovered() {
            ui.painter_at(row).rect_filled(row, 3.0, if expanded { t.row_selected } else { t.hover });
        }
        ui.painter_at(row).text(
            row.left_center() + vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            format!("{}  {dep} → {arr}", train.name),
            egui::FontId::proportional(11.5),
            if expanded { t.text_strong } else { t.text },
        );
        ui.painter_at(row).text(
            row.right_center() - vec2(6.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            if train.cars.is_empty() { tl!("Seats…").to_string() } else { tl!("Seats").to_string() },
            egui::FontId::proportional(10.5),
            t.text_dim,
        );
        if resp.clicked() {
            // Toggling a train's list resets the landing (a new pick starts a new flow).
            app.ui.fleet_draft.sgr_train = if expanded { None } else { Some(train.trips_no.clone()) };
            app.ui.fleet_draft.sgr_coach = None;
        }
        if expanded {
            let picked = app.ui.fleet_draft.sgr_coach.as_ref().and_then(|(tn, cid)| (tn == &train.trips_no).then_some(*cid));
            if train.cars.is_empty() {
                widgets::dim_label(ui, tl!("The vendor hasn't reported this set's coaches yet — the sweep keeps asking."));
            }
            for car in &train.cars {
                let sold_out = car.empty <= 0;
                ui.horizontal(|ui| {
                    widgets::dim_label(
                        ui,
                        &crate::i18n::fmt(
                            tl!("{name} — {free} of {total} seats free"),
                            &[("name", &car.name), ("free", &car.empty.to_string()), ("total", &car.capacity.to_string())],
                        ),
                    );
                    if sold_out {
                        widgets::dim_label(ui, tl!("No seat available"));
                    } else if widgets::secondary_button(ui, tl!("Select")).clicked() {
                        app.ui.fleet_draft.sgr_coach = Some((train.trips_no.clone(), car.id));
                    }
                    if picked == Some(car.id) {
                        widgets::dim_label(ui, tl!("Selected"));
                    }
                });
                // The active train's seat map: when the sweep's TrainSetBy answer is this
                // train's, each coach draws its own grid — the vendor's arrangement, the
                // free seats green, the taken seats dim, the aisles gaps.
                if tt.seat_set_trip.as_deref() == Some(train.trips_no.as_str())
                    && let Some(set) = &tt.seat_set
                    && let Some(grid) = set.coach(car.id)
                {
                    draw_seat_grid(ui, &t, grid);
                }
            }
            if picked.is_some() {
                widgets::dim_label(ui, tl!("Seat picked — booking needs an SGR account, so the flow ends here."));
            }
        }
    }
    ui.add_space(4.0);
    widgets::dim_label(ui, tl!("The timetable rides the document — a saved map keeps it."));
}

/// `HH:MM` in the vendor's own clock style (minutes since midnight EAT).
fn format_clock(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

/// One coach's seat grid, drawn the way the vendor's own layout draws it: the column
/// letters over the rows, the row numbers on the left, free seats green, taken seats dim,
/// and the aisles/WCs/luggage gaps left as gaps. Long coaches scroll in place (the panel's
/// own rules: a long list scrolls, it does not push the panel out).
fn draw_seat_grid(ui: &mut Ui, t: &Tokens, coach: &terrazon_fleet::seats::SeatCoach) {
    use terrazon_fleet::seats::SeatType;
    if !coach.has_seat || coach.seats.is_empty() || coach.row_count == 0 || coach.column_count == 0 {
        return;
    }
    widgets::dim_label(
        ui,
        &crate::i18n::fmt(tl!("{arr} · {free} free"), &[("arr", &coach.arrangement()), ("free", &coach.free_count().to_string())]),
    );
    const CELL: f32 = 11.0;
    const GAP: f32 = 2.0;
    let letter_w = 14.0;
    let grid_w = letter_w + f32::from(coach.column_count) * (CELL + GAP);
    let grid_h = f32::from(coach.row_count) * (CELL + GAP);
    let free = egui::Color32::from_rgb(56, 142, 90);
    let taken = t.text_dim.gamma_multiply(0.55);
    let special = t.text_dim.gamma_multiply(0.28);
    egui::ScrollArea::vertical().max_height(grid_h.min(190.0)).show(ui, |ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(grid_w, grid_h), Sense::hover());
        let painter = ui.painter_at(rect);
        // The column letters over their seat columns (the aisle's gap carries no letter).
        for col in 1..=coach.column_count {
            let label = coach.seats.iter().find(|s| s.column == col).map(|s| s.column_label.as_str()).unwrap_or("");
            if label.is_empty() {
                continue;
            }
            painter.text(
                rect.left_top() + vec2(letter_w + f32::from(col - 1) * (CELL + GAP) + CELL * 0.5, 2.0),
                egui::Align2::CENTER_TOP,
                label,
                egui::FontId::proportional(8.0),
                t.text_dim,
            );
        }
        for seat in &coach.seats {
            let x = rect.left() + letter_w + f32::from(seat.column - 1) * (CELL + GAP);
            let y = rect.top() + f32::from(seat.row - 1) * (CELL + GAP);
            let cell = egui::Rect::from_min_size(egui::pos2(x, y), vec2(CELL, CELL));
            if seat.row == 1 {
                // The row number rides its first seat's column edge (one paint per row).
                painter.text(
                    egui::pos2(rect.left(), y + CELL * 0.5),
                    egui::Align2::LEFT_CENTER,
                    seat.row_label.as_str(),
                    egui::FontId::proportional(8.0),
                    t.text_dim,
                );
            }
            match seat.seat_type {
                SeatType::Seat | SeatType::Disabled => {
                    painter.rect_filled(cell, 2.5, if seat.available { free } else { taken });
                }
                SeatType::Luggage | SeatType::Wc => {
                    painter.rect_filled(cell, 2.5, special);
                }
                SeatType::Cancelled | SeatType::Other(_) => {}
            }
        }
    });
}

/// Bolt's sign-in editor: the phone number → the 4-digit code the vendor sends → verified.
/// The session is kept in the settings vault, so this is a once-per-install ceremony — the
/// label shows the signed-in phone and the editor collapses to a sign-out until then.
fn bolt_editor(app: &mut TerrazoneApp, ui: &mut Ui, auth_set: bool) {
    ui.add_space(2.0);
    widgets::dim_label(ui, tl!("Vehicle icons download from Bolt automatically."));
    if let Some(phone) = crate::fleet::bolt_signed_phone(app) {
        // The session's own read: the phone names the rider when a number signed in; the
        // deployment's auto-trust names its uid and shows the code it generated (the same
        // Basic header the poll rides), so the auto sign-in is inspectable, not magic.
        widgets::dim_label(ui, &crate::i18n::fmt(tl!("Signed in as {phone}. The session is kept in the settings."), &[("phone", &phone)]));
        if let Some(secret) = app.ui.fleet_vault.get("bolt", "session").map(str::to_string)
            && let Some(session) = terrazon_fleet::BoltSession::parse(&secret)
        {
            let code = session.basic_auth();
            let shown = format!("{}…{}", &code[..8.min(code.len())], &code[code.len().saturating_sub(6).max(8)..]);
            widgets::dim_label(
                ui,
                &crate::i18n::fmt(
                    tl!("Auth code {code} (generated, Basic {uid})"),
                    &[("code", shown.as_str()), ("uid", session.auth_username.as_str())],
                ),
            );
            if !session.device_id.is_empty() {
                let device = format!("{}…", &session.device_id[..12.min(session.device_id.len())]);
                widgets::dim_label(ui, &crate::i18n::fmt(tl!("Device {device}"), &[("device", device.as_str())]));
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, tl!("Sign out")).clicked() {
                crate::fleet::sign_out_bolt(app);
            }
            if widgets::secondary_button(ui, tl!("Use another number")).clicked() {
                crate::fleet::sign_out_bolt(app);
            }
        });
        return;
    }
    let stage = app.ui.fleet_draft.stage;
    match stage {
        crate::state::OtpStage::Phone => {
            let mut phone = app.ui.fleet_draft.phone.clone();
            let resp = ui
                .add(
                    egui::TextEdit::singleline(&mut phone).hint_text(tl!("Phone number (e.g. +255 700 000 000)")).desired_width(ui.available_width()),
                )
                .on_hover_text(tl!("Bolt sends a 4-digit code to this number to verify it."));
            if resp.changed() {
                app.ui.fleet_draft.phone = phone;
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, tl!("Send code")).clicked() {
                    crate::fleet::otp_send_code(app);
                }
            });
        }
        crate::state::OtpStage::Code => {
            widgets::dim_label(ui, &crate::i18n::fmt(tl!("Type the 4-digit code sent to {phone}."), &[("phone", app.ui.fleet_draft.phone.trim())]));
            ui.add_space(2.0);
            let mut code = app.ui.fleet_draft.code.clone();
            let resp = ui
                .add(egui::TextEdit::singleline(&mut code).hint_text(tl!("4-digit code")).password(false).desired_width(ui.available_width()))
                .on_hover_text(tl!("The 4 digits from Bolt's SMS."));
            if resp.changed() {
                // Digits only, at most 4 — the code is exactly the SMS's four characters.
                let filtered: String = code.chars().filter(|c| c.is_ascii_digit()).take(4).collect();
                app.ui.fleet_draft.code = filtered;
            }
            let code_ready = app.ui.fleet_draft.code.chars().count() == 4;
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, tl!("Verify")).clicked() && code_ready {
                    crate::fleet::otp_verify(app);
                }
                if widgets::secondary_button(ui, tl!("Change number")).clicked() {
                    crate::fleet::otp_reset(app);
                }
            });
        }
    }
    if let Some(e) = &app.ui.fleet_draft.error {
        widgets::dim_label(ui, e);
    }
    if auth_set && stage == crate::state::OtpStage::Phone {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, tl!("Sign out")).clicked() {
                crate::fleet::sign_out_bolt(app);
            }
        });
    }
}

/// One feed row: status dot, name, marker count, status word, the eye (and the managed badge).
/// Returns the click.
#[allow(clippy::too_many_arguments)]
fn feed_row(
    ui: &mut Ui,
    t: &Tokens,
    id: (&str, &str),
    label: &str,
    dot: egui::Color32,
    status: &str,
    count: Option<usize>,
    visible: bool,
    managed: bool,
    selected: bool,
) -> Action {
    let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let painter = ui.painter_at(row);
    if selected || resp.hovered() {
        painter.rect_filled(row, 3.0, if selected { t.row_selected } else { t.hover });
    }
    painter.circle_filled(row.left_center() + vec2(9.0, 0.0), 4.0, dot);
    painter.text(
        row.left_center() + vec2(22.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.5),
        if selected { t.text_strong } else { t.text },
    );
    if managed {
        // The system owns this session — the small ring badge marks it, no text layout needed.
        let badge_c = row.left_center() + vec2(24.0, -8.0);
        painter.circle_stroke(badge_c, 3.5, Stroke::new(1.2, t.text_dim));
    }
    if let Some(count) = count {
        painter.text(
            row.right_center() - vec2(64.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            count.to_string(),
            egui::FontId::proportional(11.0),
            t.text_dim,
        );
    }
    painter.text(row.right_center() - vec2(34.0, 0.0), egui::Align2::RIGHT_CENTER, status, egui::FontId::proportional(10.5), t.text_dim);
    // The eye: the doc's own toggle (the layer tree moves with it).
    let eye = row.right_center() - vec2(12.0, 0.0);
    let eye_rect = egui::Rect::from_center_size(eye, vec2(20.0, 20.0));
    let eye_resp = ui.interact(eye_rect, egui::Id::new((id.0, id.1)), Sense::click());
    icons::paint(ui, if visible { "eye" } else { "eye-off" }, eye_rect.shrink(3.0), if visible { t.icon } else { t.text_disabled });
    if eye_resp.clicked() {
        return Action::Toggle;
    }
    if resp.clicked() {
        return Action::Select;
    }
    Action::None
}
