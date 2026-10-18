//! The SGR seat map: the vendor's `TrainSetBy` answer read into real coaches — rows,
//! columns, the seat types, the vendor's own arrangement — with the seats numbered the way
//! the vendor numbers them, plus the booking chain's request builders (the TICIDIS
//! `Ticket/*` family; **code only** — booking itself stays the vendor's own login's work,
//! TerraZone's flow honestly ends at the picked seat).
//!
//! The vendor's answer shape (TRC TICIDIS, verified against the SGR2026 reference):
//! `{code, ok, data: {id, trainTypeId, trainType, name, no, trainSetRailwayCars: [
//!   {id, railwayCarType, name, orderNo, rowCount, columnCount, capacity, emptySeats,
//!    hasSeat, seats: [{row, rowDescription, column, columnDescription, seatTypeId,
//!    isAvailable, rotation, newSeatNo}]}]}}`.
//!
//! Seat types (the vendor's own table): 39 seat · 40 luggage · 41 WC · 42 cancelled ·
//! 43 disabled passenger. An aisle reads as a column no seat-type seat ever occupies —
//! that gap is what the arrangement read turns into "2+2".

use serde::{Deserialize, Serialize};

use crate::{Provider, feed::FleetRequest, sgr::rail::SGR_BASE};

/// The seat types the vendor's own table names (the layout legend reads these).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SeatType {
    /// 39 — a bookable passenger seat.
    Seat,
    /// 40 — the luggage rack.
    Luggage,
    /// 41 — the WC.
    Wc,
    /// 42 — a cancelled/removed position (draws as nothing, like the aisle).
    Cancelled,
    /// 43 — the disabled-passenger seat.
    Disabled,
    /// Anything else the vendor adds.
    Other(u16),
}

impl SeatType {
    /// From the vendor's `seatTypeId`.
    pub fn from_code(code: u16) -> Self {
        match code {
            39 => Self::Seat,
            40 => Self::Luggage,
            41 => Self::Wc,
            42 => Self::Cancelled,
            43 => Self::Disabled,
            other => Self::Other(other),
        }
    }

    /// Does this position count as a seat the arrangement names? (Aisles, luggage racks,
    /// WCs and cancelled positions break a column group; disabled seats are seats.)
    pub fn is_seat(self) -> bool {
        matches!(self, Self::Seat | Self::Disabled)
    }
}

/// One position on a coach's grid: where it sits, what it is, and whether it is free.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Seat {
    /// The vendor's row number (1-based).
    pub row: u16,
    /// The vendor's column number (1-based).
    pub column: u16,
    /// The row's own label ("1") — the vendor's `rowDescription`.
    #[serde(default)]
    pub row_label: String,
    /// The column's own label ("A") — the vendor's `columnDescription` (empty on aisles).
    #[serde(default)]
    pub column_label: String,
    pub seat_type: SeatType,
    /// The vendor's `isAvailable` — free to book right now.
    #[serde(default)]
    pub available: bool,
    /// The vendor's `rotation` (degrees; 180 = the seat faces backward).
    #[serde(default)]
    pub rotation: i32,
    /// The seat's number: the vendor's `newSeatNo` when it carried one, else generated in
    /// the vendor's own order (row by row, left to right, seats only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no: Option<String>,
}

/// One coach of the train set: its class, its grid and its availability.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatCoach {
    /// The vendor's car row id (matches the timetable's `SgrCoach::id` — the link the
    /// panel uses to show a set's grid under its timetable row).
    pub id: i64,
    /// The vendor's `railwayCarId`.
    #[serde(default)]
    pub railway_car_id: i64,
    /// The class name ("Royal Class", "Business", "Economy").
    pub name: String,
    /// The coach's position in the set (the vendor's `orderNo`).
    pub order: i32,
    pub row_count: u16,
    pub column_count: u16,
    pub capacity: i32,
    /// The seats still bookable (the vendor's `emptySeats`).
    pub empty_seats: i32,
    /// The vendor's `hasSeat` — false reads "no seat map for this coach".
    pub has_seat: bool,
    pub seats: Vec<Seat>,
}

impl SeatCoach {
    /// The seat at a grid position (None when the coach has no such position).
    pub fn seat_at(&self, row: u16, column: u16) -> Option<&Seat> {
        self.seats.iter().find(|s| s.row == row && s.column == column)
    }

    /// The free seats counted from the grid itself (the vendor's `emptySeats` when the
    /// grid is silent).
    pub fn free_count(&self) -> usize {
        let from_grid = self.seats.iter().filter(|s| s.seat_type.is_seat() && s.available).count();
        if from_grid > 0 { from_grid } else { self.empty_seats.max(0) as usize }
    }

    /// The arrangement read: the seat columns of the fullest row, split into groups by the
    /// gaps (aisles) — "2+2" for the Royal Class's A B | C D, "3+2" for Economy. The
    /// summary carries the rows and the capacity: `2+2 · 12 rows · 44 seats`.
    pub fn arrangement(&self) -> String {
        let mut groups = Vec::new();
        let mut run = 0usize;
        for column in 1..=self.column_count {
            let seated = self.seats.iter().any(|s| s.column == column && s.seat_type.is_seat());
            if seated {
                run += 1;
            } else if run > 0 {
                groups.push(run);
                run = 0;
            }
        }
        if run > 0 {
            groups.push(run);
        }
        if groups.is_empty() {
            return format!("{} rows · {} seats", self.row_count, self.capacity);
        }
        let layout = groups.iter().map(|g| g.to_string()).collect::<Vec<_>>().join("+");
        format!("{layout} · {} rows · {} seats", self.row_count, self.capacity)
    }
}

/// One train set's seat world: the vendor's `TrainSetBy` answer, normalized — the coaches
/// in their running order with their grids.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainSet {
    pub id: i64,
    /// The vendor's `trainType` ("High Speed Train").
    #[serde(default)]
    pub train_type: String,
    /// The vendor's `name` ("EXPRESS TRAIN").
    #[serde(default)]
    pub name: String,
    /// The vendor's set number (`no`, e.g. `1011`).
    #[serde(default)]
    pub no: i64,
    pub coaches: Vec<SeatCoach>,
}

impl TrainSet {
    /// The coach the timetable row names (by the vendor's car id — the link both answers
    /// share), else the set's own order.
    pub fn coach(&self, car_id: i64) -> Option<&SeatCoach> {
        self.coaches.iter().find(|c| c.id == car_id || c.railway_car_id == car_id)
    }
}

/// The `TrainSetBy` ask: one POST per active trip (the sweep fires it on its own gentle
/// cadence — see [`crate::sgr::rail::seat_request`]).
pub fn train_set_request(boarding_id: i64, landing_id: i64, trip_set_id: i64, poll_no: u64) -> FleetRequest {
    FleetRequest {
        key: format!("sgr/seats{poll_no}"),
        provider: Provider::Sgr,
        url: format!("{SGR_BASE}/Public/TrainSetBy"),
        headers: crate::sgr::rail::sweep_headers(),
        body: Some((
            format!(r#"{{"boardingStationId":{boarding_id},"landingStationId":{landing_id},"tripTimeTableTrainSetId":{trip_set_id}}}"#),
            "application/json".into(),
        )),
        cells: Vec::new(),
    }
}

/// Parse the vendor's `TrainSetBy` answer into a [`TrainSet`].
pub fn parse_train_set(bytes: &[u8]) -> Result<TrainSet, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer is not UTF-8 text".to_string())?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the answer is not JSON: {e}"))?;
    let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(-1);
    if code != 200 {
        let message = v.get("message").and_then(|x| x.as_str()).unwrap_or("(no message)");
        return Err(format!("the SGR feed answered: {message} (code {code})"));
    }
    let data = v.get("data").ok_or("no data block in the seat answer")?;
    let cars = data.get("trainSetRailwayCars").and_then(|x| x.as_array()).ok_or("no trainSetRailwayCars in the seat answer")?;
    let mut coaches = Vec::new();
    for c in cars {
        let Some(id) = c.get("id").and_then(|x| x.as_i64()) else { continue };
        let seats = c
            .get("seats")
            .and_then(|x| x.as_array())
            .map(|seats| {
                seats
                    .iter()
                    .filter_map(|s| {
                        Some(Seat {
                            row: s.get("row").and_then(|x| x.as_u64())? as u16,
                            column: s.get("column").and_then(|x| x.as_u64())? as u16,
                            row_label: s.get("rowDescription").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                            column_label: s.get("columnDescription").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                            seat_type: SeatType::from_code(s.get("seatTypeId").and_then(|x| x.as_u64()).unwrap_or(39) as u16),
                            available: s.get("isAvailable").and_then(|x| x.as_bool()).unwrap_or(false),
                            rotation: s.get("rotation").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
                            no: s.get("newSeatNo").and_then(crate::feed::as_text),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        coaches.push(SeatCoach {
            id,
            railway_car_id: c.get("railwayCarId").and_then(|x| x.as_i64()).unwrap_or(0),
            name: c.get("railwayCarType").and_then(|x| x.as_str()).or_else(|| c.get("name").and_then(|x| x.as_str())).unwrap_or("Coach").to_string(),
            order: c.get("orderNo").and_then(|x| x.as_i64()).unwrap_or(coaches.len() as i64 + 1) as i32,
            row_count: c.get("rowCount").and_then(|x| x.as_u64()).unwrap_or(0) as u16,
            column_count: c.get("columnCount").and_then(|x| x.as_u64()).unwrap_or(0) as u16,
            capacity: c.get("capacity").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
            empty_seats: c.get("emptySeats").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
            has_seat: c.get("hasSeat").and_then(|x| x.as_bool()).unwrap_or(!seats.is_empty()),
            seats,
        });
    }
    if coaches.is_empty() {
        return Err("the seat answer carried no coaches".into());
    }
    // The vendor's own numbering, where the answer left a seat unnamed.
    name_seats(&mut coaches);
    coaches.sort_by_key(|c| c.order);
    Ok(TrainSet {
        id: data.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
        train_type: data.get("trainType").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        name: data.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        no: data.get("no").and_then(|x| x.as_i64()).unwrap_or(0),
        coaches,
    })
}

/// The vendor's own numbering (the SGR2026 reference's rule): the seats sort row by row,
/// left to right, and every seat-type seat takes the next number. The vendor's own
/// `newSeatNo`s stay (they are the truth); the silent ones get generated.
pub fn name_seats(coaches: &mut [SeatCoach]) {
    for coach in coaches {
        let named = coach.seats.iter().filter_map(|s| s.no.as_deref()).filter(|s| !s.trim().is_empty()).count();
        if named > 0 {
            continue;
        }
        let mut numbered: Vec<&mut Seat> = coach.seats.iter_mut().filter(|s| s.seat_type.is_seat()).collect();
        numbered.sort_by_key(|s| (s.row, s.column));
        for (i, seat) in numbered.iter_mut().enumerate() {
            seat.no = Some((i + 1).to_string());
        }
    }
}

// --- The booking chain (code only — TerraZone never books; the flow ends at the seat) ---

/// The vendor's header block for the signed calls (`CustomHeaderParam` carries the
/// client's public IP, `Content-MD5` the vendor's app constant — both from the TICIDIS
/// reference client).
fn booking_headers(token: &str) -> Vec<(String, String)> {
    vec![
        ("Authorization".into(), format!("Bearer {token}")),
        ("CustomHeaderParam".into(), "197.250.198.0".into()),
        ("Content-MD5".into(), "c353ba9aa1ae612b6527bf8663016d8d".into()),
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "application/json".into()),
    ]
}

/// One passenger's booking draft — the fields the vendor's `Ticket/CreateTicket` rows
/// carry. Built by the UI flow that ends at a picked seat; **never sent by TerraZone**.
#[derive(Clone, Debug, PartialEq)]
pub struct BookingDraft {
    pub boarding_station_id: i64,
    pub landing_station_id: i64,
    /// The vendor's `tripTimeTableTrainSetId` (the timetable row's own).
    pub trip_set_id: i64,
    /// The picked coach (the vendor's `railwayCarId`).
    pub railway_car_id: i64,
    /// The picked seat's number.
    pub seat_no: String,
    pub passenger_name: String,
    pub phone: String,
}

impl BookingDraft {
    /// The vendor's `Ticket/CreateBookedSeat` body (the seat hold that precedes the
    /// ticket).
    pub fn booked_seat_body(&self) -> String {
        format!(
            r#"{{"boardingStationId":{},"landingStationId":{},"tripTimeTableTrainSetId":{},"railwayCarId":{},"seatNo":"{}"}}"#,
            self.boarding_station_id, self.landing_station_id, self.trip_set_id, self.railway_car_id, self.seat_no
        )
    }

    /// The vendor's `Ticket/CreateTicket` body (the row array the vendor's client sends).
    pub fn create_ticket_body(&self, access_key: &str) -> String {
        format!(
            r#"[{{"departure":{{"boardingStationId":{},"landingStationId":{},"tripsTimeTableTrainsetId":{},"accessKey":"{}","bookingType":0,"ticketTypeId":4,"passengerName":"{}","railwayCarId":{},"phone":"{}","isDisabledPassenger":false,"hasLuggage":false,"directionType":0}},"return":null}}]"#,
            self.boarding_station_id, self.landing_station_id, self.trip_set_id, access_key, self.passenger_name, self.railway_car_id, self.phone
        )
    }

    /// The hold request (`Ticket/CreateBookedSeat`). Not wired into any poll — the
    /// booking flow is the vendor's own login's work.
    pub fn booked_seat_request(&self, token: &str, poll_no: u64) -> FleetRequest {
        FleetRequest {
            key: format!("sgr/book-seat{poll_no}"),
            provider: Provider::Sgr,
            url: format!("{SGR_BASE}/Public/Ticket/CreateBookedSeat"),
            headers: booking_headers(token),
            body: Some((self.booked_seat_body(), "application/json".into())),
            cells: Vec::new(),
        }
    }

    /// The ticket request (`Ticket/CreateTicket`). Code only, like the hold.
    pub fn create_ticket_request(&self, token: &str, access_key: &str, poll_no: u64) -> FleetRequest {
        FleetRequest {
            key: format!("sgr/book-ticket{poll_no}"),
            provider: Provider::Sgr,
            url: format!("{SGR_BASE}/Public/Ticket/CreateTicket"),
            headers: booking_headers(token),
            body: Some((self.create_ticket_body(access_key), "application/json".into())),
            cells: Vec::new(),
        }
    }
}

/// The vendor's phone-OTP start (`User/PublicPhoneLoginSendSms`) — the booking account's
/// sign-in, code only.
pub fn otp_sms_request(phone: &str, poll_no: u64) -> FleetRequest {
    FleetRequest {
        key: format!("sgr/book-otp-start{poll_no}"),
        provider: Provider::Sgr,
        url: format!("{SGR_BASE}/Public/User/PublicPhoneLoginSendSms"),
        headers: booking_headers(""),
        body: Some((format!(r#"{{"phone":"{phone}"}}"#), "application/json".into())),
        cells: Vec::new(),
    }
}

/// The vendor's OTP confirm (`User/PublicPhoneLogin`) — answers with the Bearer token the
/// booking calls ride, code only.
pub fn otp_login_request(key: &str, code: &str, poll_no: u64) -> FleetRequest {
    FleetRequest {
        key: format!("sgr/book-otp-confirm{poll_no}"),
        provider: Provider::Sgr,
        url: format!("{SGR_BASE}/Public/User/PublicPhoneLogin"),
        headers: booking_headers(""),
        body: Some((format!(r#"{{"key":"{key}","validationCode":"{code}"}}"#), "application/json".into())),
        cells: Vec::new(),
    }
}

/// The vendor's payment providers (`Gepg/GetActivePaymentProviders`), code only.
pub fn payment_providers_request(token: &str, poll_no: u64) -> FleetRequest {
    FleetRequest {
        key: format!("sgr/book-pay{poll_no}"),
        provider: Provider::Sgr,
        url: format!("{SGR_BASE}/Public/Gepg/GetActivePaymentProviders"),
        headers: booking_headers(token),
        body: None,
        cells: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The vendor's own `TrainSetBy` answer, as the SGR2026 reference records it: the Royal
    /// Class kiosk coach (12 rows × 5 columns, the aisle at column 3) with its cancelled
    /// positions, followed by a Business coach — tomorrow's trip, as the sweep would ask it.
    fn the_vendor_answer() -> &'static str {
        r#"{"code":200,"ok":true,"message":"GNRL100 - Success.","data":{"id":1849,"trainTypeId":13,
            "trainType":"High Speed Train","name":"EXPRESS TRAIN","no":1011,
            "trainSetRailwayCars":[
              {"id":417,"standardCapacity":44,"disabledCapacity":0,"capacity":44,"emptySeats":2,
               "rowCount":12,"columnCount":5,"isActive":true,"isDisabled":false,"name":"ROYAL CLASS KIOSK",
               "railwayCarId":19,"railwayCarTypeId":151,"railwayCarType":"Royal Class","hasSeat":true,"orderNo":1,
               "seats":[
                 {"column":1,"columnDescription":"A","isAvailable":false,"seatTypeId":39,"row":1,"rowDescription":"1","id":1417,"rotation":180,"newSeatNo":"1"},
                 {"column":2,"columnDescription":"B","isAvailable":true,"seatTypeId":39,"row":1,"rowDescription":"1","id":1418,"rotation":0,"newSeatNo":"2"},
                 {"column":4,"columnDescription":"C","isAvailable":true,"seatTypeId":39,"row":1,"rowDescription":"1","id":1419,"rotation":0,"newSeatNo":"3"},
                 {"column":5,"columnDescription":"D","isAvailable":false,"seatTypeId":39,"row":1,"rowDescription":"1","id":1420,"rotation":0,"newSeatNo":"4"},
                 {"column":3,"columnDescription":"","isAvailable":false,"seatTypeId":42,"row":7,"rowDescription":"7","id":1418,"rotation":0,"newSeatNo":""},
                 {"column":4,"columnDescription":"C","isAvailable":true,"seatTypeId":39,"row":7,"rowDescription":"7","id":1419,"rotation":0,"newSeatNo":"27"},
                 {"column":1,"columnDescription":"A","isAvailable":true,"seatTypeId":39,"row":8,"rowDescription":"8","id":1421,"rotation":0,"newSeatNo":"29"}
               ]},
              {"id":418,"capacity":45,"emptySeats":12,"rowCount":12,"columnCount":5,
               "name":"BUSINESS","railwayCarId":13,"railwayCarType":"Business","hasSeat":true,"orderNo":2,
               "seats":[
                 {"column":1,"columnDescription":"A","isAvailable":true,"seatTypeId":39,"row":1,"rowDescription":"1"},
                 {"column":2,"columnDescription":"B","isAvailable":true,"seatTypeId":39,"row":1,"rowDescription":"1"},
                 {"column":4,"columnDescription":"C","isAvailable":true,"seatTypeId":39,"row":1,"rowDescription":"1"},
                 {"column":5,"columnDescription":"D","isAvailable":true,"seatTypeId":39,"row":1,"rowDescription":"1"},
                 {"column":3,"columnDescription":"","isAvailable":false,"seatTypeId":41,"row":2,"rowDescription":"2"}
               ]}]}}"#
    }

    #[test]
    fn the_vendor_answer_parses_into_the_set() {
        let set = parse_train_set(the_vendor_answer().as_bytes()).unwrap();
        assert_eq!(set.no, 1011);
        assert_eq!(set.name, "EXPRESS TRAIN");
        assert_eq!(set.train_type, "High Speed Train");
        assert_eq!(set.coaches.len(), 2);
        let royal = &set.coaches[0];
        assert_eq!((royal.id, royal.railway_car_id, royal.order), (417, 19, 1));
        assert_eq!((royal.row_count, royal.column_count, royal.capacity), (12, 5, 44));
        assert_eq!(royal.name, "Royal Class");
        assert!(royal.has_seat);
    }

    #[test]
    fn seat_types_read_from_the_vendors_table() {
        let set = parse_train_set(the_vendor_answer().as_bytes()).unwrap();
        let royal = &set.coaches[0];
        // The cancelled position at row 7 / column 3 reads Cancelled, not a seat.
        let cancelled = royal.seat_at(7, 3).unwrap();
        assert_eq!(cancelled.seat_type, SeatType::Cancelled);
        assert!(!cancelled.seat_type.is_seat());
        assert_eq!(cancelled.column_label, "");
        // The WC on the Business coach reads WC.
        let business = &set.coaches[1];
        assert_eq!(business.seat_at(2, 3).unwrap().seat_type, SeatType::Wc);
        // Seat 1A carries the vendor's own rotation (the facing read).
        assert_eq!(royal.seat_at(1, 1).unwrap().rotation, 180);
    }

    #[test]
    fn the_arrangement_respects_the_aisle() {
        let set = parse_train_set(the_vendor_answer().as_bytes()).unwrap();
        // The aisle at column 3 splits the Royal Class into 2+2 — never 4 across.
        assert_eq!(set.coaches[0].arrangement(), "2+2 · 12 rows · 44 seats");
        assert_eq!(set.coaches[1].arrangement(), "2+2 · 12 rows · 45 seats");
    }

    #[test]
    fn seat_numbers_follow_the_vendors_own_rule() {
        let set = parse_train_set(the_vendor_answer().as_bytes()).unwrap();
        // The vendor's own numbers stay; the Business coach's silent seats are generated
        // row by row, left to right, seats only (the WC at 2/3 takes none).
        let business = &set.coaches[1];
        assert_eq!(business.seat_at(1, 1).unwrap().no.as_deref(), Some("1"));
        assert_eq!(business.seat_at(1, 5).unwrap().no.as_deref(), Some("4"));
        assert_eq!(business.seat_at(2, 3).unwrap().no, None);
        // The Royal Class keeps the vendor's own numbering (1, 2, 3, 4, 27, 29).
        let royal = &set.coaches[0];
        assert_eq!(royal.seat_at(1, 2).unwrap().no.as_deref(), Some("2"));
        assert_eq!(royal.seat_at(7, 4).unwrap().no.as_deref(), Some("27"));
    }

    #[test]
    fn free_count_reads_the_grid_and_the_link_finds_the_coach() {
        let set = parse_train_set(the_vendor_answer().as_bytes()).unwrap();
        assert_eq!(set.coaches[1].free_count(), 4);
        // The timetable's SgrCoach id (417) finds its grid (the panel's link).
        assert!(set.coach(417).is_some());
        assert!(set.coach(19).is_some());
        assert!(set.coach(999).is_none());
    }

    #[test]
    fn the_ask_is_the_vendors_own_endpoint_and_body() {
        let req = train_set_request(1, 12, 1755, 7);
        assert!(req.url.ends_with("/Public/TrainSetBy"));
        assert_eq!(req.provider, Provider::Sgr);
        let (body, ct) = req.body.unwrap();
        assert_eq!(ct, "application/json");
        assert!(body.contains(r#""tripTimeTableTrainSetId":1755"#));
        assert!(body.contains(r#""boardingStationId":1"#));
        assert_eq!(req.key, "sgr/seats7");
    }

    #[test]
    fn the_booking_chain_builds_the_vendors_own_calls() {
        let draft = BookingDraft {
            boarding_station_id: 1,
            landing_station_id: 6,
            trip_set_id: 1755,
            railway_car_id: 204,
            seat_no: "27".into(),
            passenger_name: "RAIS".into(),
            phone: "657649154".into(),
        };
        // The hold and the ticket build the vendor's bodies; nothing here is sent — the
        // booking flow stays the vendor's own login's work.
        let hold = draft.booked_seat_request("tok", 3);
        assert!(hold.url.ends_with("/Public/Ticket/CreateBookedSeat"));
        assert!(hold.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer tok"));
        assert!(hold.body.as_ref().unwrap().0.contains(r#""seatNo":"27""#));
        let ticket = draft.create_ticket_request("tok", "a4-uuid", 4);
        assert!(ticket.url.ends_with("/Public/Ticket/CreateTicket"));
        assert!(ticket.body.as_ref().unwrap().0.contains(r#""tripsTimeTableTrainsetId":1755"#));
        assert!(ticket.body.as_ref().unwrap().0.contains(r#""accessKey":"a4-uuid""#));
        let otp = otp_sms_request("255700000000", 5);
        assert!(otp.url.ends_with("/Public/User/PublicPhoneLoginSendSms"));
        let login = otp_login_request("k", "218463", 6);
        assert!(login.body.as_ref().unwrap().0.contains(r#""validationCode":"218463""#));
        assert!(payment_providers_request("tok", 7).url.ends_with("/Public/Gepg/GetActivePaymentProviders"));
    }

    #[test]
    fn the_timetable_round_trips_through_the_document() {
        // The seat set rides the timetable into the saved map file (serde round trip).
        let set = parse_train_set(the_vendor_answer().as_bytes()).unwrap();
        let body = serde_json::to_string(&set).unwrap();
        let back: TrainSet = serde_json::from_str(&body).unwrap();
        assert_eq!(back, set);
    }
}
