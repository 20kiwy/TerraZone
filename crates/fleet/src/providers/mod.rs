//! The per-provider API modules — one module per vendor feed, each carrying that vendor's
//! own answer shapes and fingerprints, separated from the shared plumbing the way the
//! vendors separate them:
//!
//! - [`bolt`] — the ride-hailing poll (`{code, data:{vehicles:{…}}}`, the CDN artwork).
//! - [`marine`] — the vessel radar's mobile tile (`data.rows`, the marine keys).
//! - [`flight`] — the public aircraft feed's flexible shape and the ICAO kind read.
//!
//! The SGR (the fourth provider) keeps its own folder ([`crate::sgr`]): it is a schedule
//! service with its own atlas, timetable and seat maps, not a vehicle poll. The sign-in
//! flows for every provider live together in [`crate::auth`]; the request building lives
//! together in [`crate::feed`].

pub mod bolt;
pub mod flight;
pub mod marine;
