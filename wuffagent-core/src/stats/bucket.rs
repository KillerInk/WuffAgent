//! 2b: pure time-bucketing shared by the usage store (`usage/stats.rs`) and
//! the metrics store (`agents/metrics.rs`).
//!
//! All bucket math is done on local **wall-clock** `NaiveDateTime`s
//! (converted from the UTC `ts` once per entry). Wall-clock arithmetic is
//! exact calendar math — no duration division across timezones — so DST
//! transitions cannot shift bucket boundaries; a repeated "fold" hour during
//! fall-back simply lands in the same wall-clock bucket.

use chrono::{Datelike, DateTime, Local, NaiveDate, NaiveDateTime, Timelike, Utc};

/// Bucket granularity (also selects the window size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// One bucket per hour; window = the last 24 hours.
    Hour,
    /// One bucket per day; window = the last 30 days.
    Day,
    /// One bucket per week (Monday-based); window = the last 12 weeks.
    Week,
}

impl Granularity {
    /// Number of buckets in the window for this granularity.
    pub fn bucket_count(self) -> usize {
        match self {
            Granularity::Hour => 24,
            Granularity::Day => 30,
            Granularity::Week => 12,
        }
    }

    /// The local wall-clock start of the first bucket in the window ending at
    /// `now_local`.
    pub fn window_start(&self, now_local: NaiveDateTime) -> NaiveDateTime {
        let days = chrono::Duration::days;
        match self {
            Granularity::Hour => {
                let hour_start = now_local
                    .date()
                    .and_hms_opt(now_local.hour(), 0, 0)
                    .expect("valid hour");
                hour_start - chrono::Duration::hours(23)
            }
            Granularity::Day => (now_local.date() - days(29)).and_hms_opt(0, 0, 0).unwrap(),
            Granularity::Week => monday_of(now_local.date()) - days(7 * 11),
        }
    }

    /// Zero-based index of the bucket containing `nd` (local wall clock), or
    /// `None` when the timestamp falls outside the window
    /// (`window_start..window_start + bucket_count * unit`).
    pub fn bucket_index(&self, nd: NaiveDateTime, window_start: NaiveDateTime) -> Option<usize> {
        let n = self.bucket_count();
        match self {
            Granularity::Hour => {
                let i = (nd - window_start).num_hours();
                (0..n as i64).contains(&i).then_some(i as usize)
            }
            Granularity::Day => {
                let i = (nd.date() - window_start.date()).num_days();
                (0..n as i64).contains(&i).then_some(i as usize)
            }
            Granularity::Week => {
                let i = (monday_of(nd.date()) - window_start).num_days() / 7;
                (0..n as i64).contains(&i).then_some(i as usize)
            }
        }
    }

    /// Local wall-clock start of bucket `i` (0-based) in the window.
    pub fn bucket_start(&self, window_start: NaiveDateTime, i: usize) -> NaiveDateTime {
        match self {
            Granularity::Hour => window_start + chrono::Duration::hours(i as i64),
            Granularity::Day => window_start + chrono::Duration::days(i as i64),
            Granularity::Week => window_start + chrono::Duration::days(7 * i as i64),
        }
    }
}

/// Monday (local wall clock, midnight) of the week containing `d`.
pub(crate) fn monday_of(d: NaiveDate) -> NaiveDateTime {
    (d - chrono::Duration::days(d.weekday().number_from_monday() as i64 - 1))
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

/// The local wall-clock start of every bucket in the window for
/// `granularity` ending at `now` (UTC), oldest first. Shared skeleton for
/// the zero-filled builders of the usage and metrics stores.
pub fn bucket_starts(granularity: Granularity, now: DateTime<Utc>) -> Vec<NaiveDateTime> {
    let now_local = now.with_timezone(&Local).naive_local();
    let window_start = granularity.window_start(now_local);
    (0..granularity.bucket_count())
        .map(|i| granularity.bucket_start(window_start, i))
        .collect()
}

/// Zero-based bucket index of a UTC timestamp within the window for
/// `granularity` ending at `now` (UTC); `None` outside the window.
pub fn bucket_index_utc(
    granularity: Granularity,
    now: DateTime<Utc>,
    ts: DateTime<Utc>,
) -> Option<usize> {
    let now_local = now.with_timezone(&Local).naive_local();
    let window_start = granularity.window_start(now_local);
    granularity.bucket_index(ts.with_timezone(&Local).naive_local(), window_start)
}
