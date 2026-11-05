//! A parsed date/time value that is *either* the raw civil fields as typed or an
//! already-normalized instant.
//!
//! SQLite stores the parsed civil fields verbatim and only normalizes when the Julian
//! day is computed (a modifier, or a Julian-day-derived output). A literal hour 24
//! (`%H` is documented `00-24`) therefore survives on a field render: the field codes
//! `%F`/`%d`/`%H` read the stored fields, and a normalized instant could never yield
//! hour 24. To reproduce that, a [`DateTime::Raw`] carries the raw civil fields (hour 24
//! and all) until [`DateTime::to_instant`] is called; the field-rendering functions read
//! the raw fields, while every modifier and every Julian-day-derived output
//! (`%s`/`%J`/`%j`/`%w`/`julianday`/`unixepoch`/`timediff`) normalizes first.
//!
//! A day past 28 is the exception: with no modifier at all, sqlite recomputes the year,
//! month and day from the Julian day before rendering, so `date('2023-02-29')` is
//! `'2023-03-01'` (see [`DateTime::normalize_unmodified_day`]).

use super::julian::{Breakdown, Instant};

/// Raw civil fields exactly as parsed. `day` may be out of range for the month (e.g.
/// `31` in September) and `time_ms` (milliseconds since midnight) may reach `86_400_000`
/// for a literal `24:00`; both survive verbatim until the Julian day is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawCivil {
    pub(crate) year: i64,
    pub(crate) month: u32,
    pub(crate) day: u32,
    pub(crate) time_ms: i64,
}

/// A parsed time-value that has been parsed but not necessarily normalized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateTime {
    /// Civil fields straight from an ISO string with no numeric timezone offset — the
    /// fields (an out-of-range day, a literal hour 24) are rendered verbatim by the
    /// field functions and only normalize when an instant is computed.
    Raw(RawCivil),
    /// A normalized instant (from `now`, a Julian/Unix number, a timezone-adjusted
    /// string, or any applied modifier).
    Normalized(Instant),
}

impl DateTime {
    /// A normalized instant, rolling any day/time overflow (an out-of-range day, a
    /// literal hour 24) — SQLite's lazy Julian-day computation.
    pub(crate) fn to_instant(self) -> Instant {
        match self {
            DateTime::Raw(r) => Instant::from_civil(r.year, r.month as i64, r.day as i64, r.time_ms),
            DateTime::Normalized(i) => i,
        }
    }

    /// sqlite's `isDate()` (date.c) with no modifier: a `Raw` value whose day is past 28
    /// gets its year, month and day recomputed from the Julian day, so a day that does
    /// not exist in its month rolls into the next one (`2023-02-29` is `2023-03-01`).
    /// A day of 28 or less is left alone, as in sqlite.
    ///
    /// A literal hour 24 is left alone too. sqlite keeps the original Julian day for
    /// `julianday`/`%s`/`%j` and the recomputed date only for display (so it prints
    /// `2024-01-30 24:00` as `2024-01-31 24:00:00`); a `Raw` value has one set of fields
    /// for both, and rolling the date here would move the instant a day forward.
    pub(crate) fn normalize_unmodified_day(self) -> DateTime {
        match self {
            DateTime::Raw(r) if r.day > 28 && r.time_ms < 86_400_000 => {
                let b = self.to_instant().breakdown();
                DateTime::Raw(RawCivil { year: b.year, month: b.month, day: b.day, time_ms: r.time_ms })
            }
            other => other,
        }
    }

    /// The civil fields for field-level rendering. `Raw` fields are returned verbatim
    /// (an out-of-range day and a literal hour 24 and all); a `Normalized` instant is
    /// broken down normally.
    pub(crate) fn civil(self) -> Breakdown {
        match self {
            DateTime::Raw(r) => {
                let mut ms = r.time_ms;
                let hour = (ms / 3_600_000) as u32;
                ms %= 3_600_000;
                let minute = (ms / 60_000) as u32;
                ms %= 60_000;
                let second = (ms / 1_000) as u32;
                let millis = (ms % 1_000) as u32;
                Breakdown { year: r.year, month: r.month, day: r.day, hour, minute, second, millis }
            }
            DateTime::Normalized(i) => i.breakdown(),
        }
    }
}
