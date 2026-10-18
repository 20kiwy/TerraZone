//! The SGR provider's own folder: everything the Tanzania Standard Gauge Railway feed is,
//! separated from the poll-feed providers the way the vendor separates it — a schedule
//! service (TICIDIS) that knows stations, trip times, coach sets and seat maps, not a
//! vehicle poll.
//!
//! - [`rail`] — the sweep, the timetable and the OSM rail atlas the trains ride.
//! - [`seats`] — the coach/seat-map API (`TrainSetBy`): rows, columns, the vendor's own
//!   arrangement, the seat types and numbers — plus the booking chain's request builders
//!   (code only; booking itself stays the vendor's own login's work).

pub mod rail;
pub mod seats;
