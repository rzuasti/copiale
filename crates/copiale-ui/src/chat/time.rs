//! Local times for display: message times, day labels, chat list ages.

use std::time::Duration;

use chrono::{DateTime, Datelike as _, FixedOffset, Local, NaiveDate, NaiveDateTime, Utc};
use copiale_core::Timestamp;

/// Converts timestamps to local time: the system's, or a fixed one in tests.
#[derive(Clone, Copy, Debug)]
pub struct LocalClock {
    fixed: Option<(FixedOffset, Timestamp)>,
    twelve_hour: bool,
}

impl LocalClock {
    pub fn system() -> Self {
        Self {
            fixed: None,
            twelve_hour: false,
        }
    }

    /// Always `now`, in a fixed time zone.
    pub fn fixed(offset: FixedOffset, now: Timestamp) -> Self {
        Self {
            fixed: Some((offset, now)),
            twelve_hour: false,
        }
    }

    /// Shows times as "2:05 PM" instead of "14:05".
    pub fn set_twelve_hour(&mut self, twelve_hour: bool) {
        self.twelve_hour = twelve_hour;
    }

    pub fn now(&self) -> Timestamp {
        match self.fixed {
            Some((_, now)) => now,
            None => Timestamp::now(),
        }
    }

    pub fn local(&self, timestamp: Timestamp) -> NaiveDateTime {
        let utc = DateTime::<Utc>::from_timestamp_millis(millis(timestamp)).unwrap_or_default();
        match self.fixed {
            Some((offset, _)) => utc.with_timezone(&offset).naive_local(),
            None => utc.with_timezone(&Local).naive_local(),
        }
    }

    pub fn day(&self, timestamp: Timestamp) -> NaiveDate {
        self.local(timestamp).date()
    }

    /// "14:05", or "2:05 PM".
    pub fn time(&self, timestamp: Timestamp) -> String {
        let format = if self.twelve_hour {
            "%-I:%M %p"
        } else {
            "%H:%M"
        };
        self.local(timestamp).format(format).to_string()
    }

    /// "Today", "Yesterday", "Mon 12 Oct", or with the year if not this one.
    pub fn day_label(&self, day: NaiveDate) -> String {
        let today = self.day(self.now());
        if day == today {
            "Today".into()
        } else if today.pred_opt() == Some(day) {
            "Yesterday".into()
        } else if day.year() == today.year() {
            day.format("%a %-d %b").to_string()
        } else {
            day.format("%a %-d %b %Y").to_string()
        }
    }

    /// Compact age for the chat list: "now", "5m", "3h", "2d", then the
    /// date: "12 Oct", or "Oct 2025" for other years.
    pub fn age(&self, timestamp: Timestamp) -> String {
        let now = self.now();
        let minutes = now.as_millis().saturating_sub(timestamp.as_millis()) / 60_000;
        match minutes {
            0 => "now".into(),
            1..60 => format!("{minutes}m"),
            60..1440 => format!("{}h", minutes / 60),
            1440..10_080 => format!("{}d", minutes / 1440),
            _ => {
                let day = self.day(timestamp);
                if day.year() == self.day(now).year() {
                    day.format("%-d %b").to_string()
                } else {
                    day.format("%b %Y").to_string()
                }
            }
        }
    }
}

/// Countdown units, largest first, in milliseconds.
const UNITS: [(u64, &str); 5] = [
    (604_800_000, "w"),
    (86_400_000, "d"),
    (3_600_000, "h"),
    (60_000, "m"),
    (1000, "s"),
];

/// Time left in its largest whole unit: "1w", "3d", "5h", "4m", "30s".
pub fn countdown(millis: u64) -> String {
    let (unit, suffix) = unit(millis);
    format!("{}{suffix}", millis / unit)
}

/// How long until [`countdown`] of `millis` changes as time passes.
pub fn countdown_changes_in(millis: u64) -> Duration {
    let (unit, _) = unit(millis);
    Duration::from_millis(millis % unit + 1)
}

fn unit(millis: u64) -> (u64, &'static str) {
    UNITS
        .into_iter()
        .find(|(unit, _)| millis >= *unit)
        .unwrap_or(UNITS[UNITS.len() - 1])
}

fn millis(timestamp: Timestamp) -> i64 {
    i64::try_from(timestamp.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-13 14:05 at UTC+2.
    const NOW: u64 = 1_791_893_100_000;
    const MINUTE: u64 = 60_000;
    const DAY: u64 = 1440 * MINUTE;

    fn clock() -> LocalClock {
        LocalClock::fixed(
            FixedOffset::east_opt(2 * 3600).unwrap(),
            Timestamp::from_millis(NOW),
        )
    }

    fn ago(millis: u64) -> Timestamp {
        Timestamp::from_millis(NOW - millis)
    }

    #[test]
    fn times_are_local_and_24_hour() {
        assert_eq!(clock().time(Timestamp::from_millis(NOW)), "14:05");
        assert_eq!(clock().time(ago(14 * 60 * MINUTE)), "00:05");
    }

    #[test]
    fn twelve_hour_times() {
        let mut clock = clock();
        clock.set_twelve_hour(true);
        assert_eq!(clock.time(Timestamp::from_millis(NOW)), "2:05 PM");
        assert_eq!(clock.time(ago(14 * 60 * MINUTE)), "12:05 AM");
    }

    #[test]
    fn day_labels() {
        let clock = clock();
        let label = |at| clock.day_label(clock.day(at));
        assert_eq!(label(ago(0)), "Today");
        // 00:05 local is still today; 23:55 the evening before is not.
        assert_eq!(label(ago(14 * 60 * MINUTE)), "Today");
        assert_eq!(label(ago(14 * 60 * MINUTE + 10 * MINUTE)), "Yesterday");
        assert_eq!(label(ago(3 * DAY)), "Sat 10 Oct");
        assert_eq!(label(ago(365 * DAY)), "Mon 13 Oct 2025");
    }

    #[test]
    fn ages() {
        let clock = clock();
        assert_eq!(clock.age(ago(30_000)), "now");
        assert_eq!(clock.age(ago(5 * MINUTE)), "5m");
        assert_eq!(clock.age(ago(3 * 60 * MINUTE)), "3h");
        assert_eq!(clock.age(ago(2 * DAY)), "2d");
        assert_eq!(clock.age(ago(8 * DAY)), "5 Oct");
        assert_eq!(clock.age(ago(400 * DAY)), "Sep 2025");
    }

    #[test]
    fn countdowns_use_the_largest_whole_unit() {
        assert_eq!(countdown(7 * DAY), "1w");
        assert_eq!(countdown(13 * DAY), "1w");
        assert_eq!(countdown(DAY), "1d");
        assert_eq!(countdown(8 * 60 * MINUTE), "8h");
        assert_eq!(countdown(5 * MINUTE - 1), "4m");
        assert_eq!(countdown(30_000), "30s");
        assert_eq!(countdown(400), "0s");
    }

    #[test]
    fn countdowns_change_when_the_number_does() {
        assert_eq!(countdown_changes_in(60_500), Duration::from_millis(501));
        assert_eq!(countdown(60_500 - 501), "59s");
        assert_eq!(countdown_changes_in(60_000), Duration::from_millis(1));
        assert_eq!(countdown_changes_in(59_999), Duration::from_millis(1000));
    }

    #[test]
    fn future_times_are_now() {
        assert_eq!(clock().age(Timestamp::from_millis(NOW + MINUTE)), "now");
    }
}
