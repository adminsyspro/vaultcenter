//! Bucket rotation periods, counted in UTC calendar days.

/// A proleptic Gregorian calendar day (UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CivilDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

fn leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl CivilDate {
    pub fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        ((1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month))
            .then_some(Self { year, month, day })
    }

    /// Reads `YYYY-MM-DD` at the start of `value` (dates and SQLite timestamps).
    pub fn parse(value: &str) -> Option<Self> {
        let date = value.get(..10)?;
        let bytes = date.as_bytes();
        if bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let digits = |range: std::ops::Range<usize>| {
            date.get(range)
                .filter(|part| part.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|part| part.parse::<u32>().ok())
        };
        Self::new(
            i32::try_from(digits(0..4)?).ok()?,
            u8::try_from(digits(5..7)?).ok()?,
            u8::try_from(digits(8..10)?).ok()?,
        )
    }

    /// The given day of the month, or the month's last day when it is shorter.
    fn clamped(year: i32, month: u8, day: u8) -> Self {
        Self {
            year,
            month,
            day: day.min(days_in_month(year, month)),
        }
    }

    /// Days since 1970-01-01 (H. Hinnant's days_from_civil).
    fn days(self) -> i64 {
        let year = i64::from(self.year) - i64::from(self.month <= 2);
        let era = (if year >= 0 { year } else { year - 399 }) / 400;
        let yoe = year - era * 400;
        let mp = (i64::from(self.month) + 9) % 12;
        let doy = (153 * mp + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn from_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Self { year, month, day }
    }

    /// 1 = Monday … 7 = Sunday (1970-01-01 was a Thursday).
    pub fn weekday(self) -> u8 {
        ((self.days() + 3).rem_euclid(7) + 1) as u8
    }
}

impl std::fmt::Display for CivilDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// When a VTL switches to a new export bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationSchedule {
    Manual,
    /// 1 = Monday … 7 = Sunday.
    Weekly {
        weekday: u8,
    },
    /// 1 … 31; a shorter month uses its last day.
    Monthly {
        month_day: u8,
    },
}

impl RotationSchedule {
    pub fn from_parts(
        rotation: &str,
        weekday: Option<u8>,
        month_day: Option<u8>,
    ) -> Result<Self, &'static str> {
        match (rotation, weekday, month_day) {
            ("manual", None, None) => Ok(Self::Manual),
            ("weekly", Some(weekday @ 1..=7), None) => Ok(Self::Weekly { weekday }),
            ("monthly", None, Some(month_day @ 1..=31)) => Ok(Self::Monthly { month_day }),
            _ => Err(
                "This schedule is not valid. Choose a day of the week from 1 to 7 or a day of the month from 1 to 31.",
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Weekly { .. } => "weekly",
            Self::Monthly { .. } => "monthly",
        }
    }

    /// First day of the period containing `today`; `None` without a schedule.
    pub fn period_start(self, today: CivilDate) -> Option<CivilDate> {
        match self {
            Self::Manual => None,
            Self::Weekly { weekday } => Some(CivilDate::from_days(
                today.days() - (i64::from(today.weekday()) - i64::from(weekday)).rem_euclid(7),
            )),
            Self::Monthly { month_day } => {
                let this_month = CivilDate::clamped(today.year, today.month, month_day);
                if today >= this_month {
                    return Some(this_month);
                }
                let (year, month) = if today.month == 1 {
                    (today.year - 1, 12)
                } else {
                    (today.year, today.month - 1)
                };
                Some(CivilDate::clamped(year, month, month_day))
            }
        }
    }

    /// Due when no bucket was switched (or schedule set) since the period began.
    pub fn is_due(self, today: CivilDate, served: Option<CivilDate>) -> bool {
        self.period_start(today)
            .is_some_and(|start| served.is_none_or(|served| served < start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(value: &str) -> CivilDate {
        CivilDate::parse(value).unwrap()
    }

    #[test]
    fn civil_dates_parse_timestamps_and_know_their_weekday() {
        assert_eq!(
            day("2026-10-13 08:15:00"),
            CivilDate::new(2026, 10, 13).unwrap()
        );
        assert_eq!(day("1970-01-01").weekday(), 4);
        assert_eq!(day("2026-10-12").weekday(), 1);
        assert_eq!(day("2026-10-13").weekday(), 2);
        assert_eq!(day("2026-10-18").weekday(), 7);
        assert_eq!(day("2028-02-29").to_string(), "2028-02-29");
        for invalid in [
            "2026-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-1-01",
            "",
            "2026-10-1x",
        ] {
            assert!(CivilDate::parse(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn weekly_periods_start_on_the_chosen_weekday() {
        let today = day("2026-10-13");
        let start = |weekday| {
            RotationSchedule::Weekly { weekday }
                .period_start(today)
                .unwrap()
                .to_string()
        };
        assert_eq!(start(1), "2026-10-12");
        assert_eq!(start(2), "2026-10-13");
        assert_eq!(start(3), "2026-10-07");
        assert_eq!(start(7), "2026-10-11");
        assert_eq!(
            RotationSchedule::Weekly { weekday: 4 }
                .period_start(day("2027-01-02"))
                .unwrap()
                .to_string(),
            "2026-12-31"
        );
    }

    #[test]
    fn monthly_periods_clamp_to_the_last_day_of_short_months() {
        let start = |month_day, today| {
            RotationSchedule::Monthly { month_day }
                .period_start(day(today))
                .unwrap()
                .to_string()
        };
        assert_eq!(start(1, "2026-10-13"), "2026-10-01");
        assert_eq!(start(15, "2026-01-10"), "2025-12-15");
        assert_eq!(start(31, "2027-02-15"), "2027-01-31");
        assert_eq!(start(31, "2027-02-28"), "2027-02-28");
        assert_eq!(start(31, "2027-03-01"), "2027-02-28");
        assert_eq!(start(31, "2027-03-31"), "2027-03-31");
        assert_eq!(start(30, "2026-03-01"), "2026-02-28");
        assert_eq!(start(29, "2028-02-29"), "2028-02-29");
        assert_eq!(start(31, "2028-02-28"), "2028-01-31");
        assert_eq!(start(31, "2028-02-29"), "2028-02-29");
        assert_eq!(start(31, "2026-04-30"), "2026-04-30");
        assert_eq!(start(31, "2026-05-01"), "2026-04-30");
    }

    #[test]
    fn a_period_is_due_once_until_a_bucket_is_switched_inside_it() {
        let weekly = RotationSchedule::Weekly { weekday: 1 };
        let today = day("2026-10-13");
        assert!(weekly.is_due(today, None));
        assert!(weekly.is_due(today, Some(day("2026-10-11"))));
        assert!(!weekly.is_due(today, Some(day("2026-10-12"))));
        assert!(!weekly.is_due(today, Some(day("2026-10-13"))));
        assert!(!RotationSchedule::Manual.is_due(today, None));
        assert_eq!(RotationSchedule::Manual.period_start(today), None);
    }

    #[test]
    fn schedules_are_validated_from_their_stored_parts() {
        assert_eq!(
            RotationSchedule::from_parts("manual", None, None),
            Ok(RotationSchedule::Manual)
        );
        assert_eq!(
            RotationSchedule::from_parts("weekly", Some(7), None),
            Ok(RotationSchedule::Weekly { weekday: 7 })
        );
        assert_eq!(
            RotationSchedule::from_parts("monthly", None, Some(31)),
            Ok(RotationSchedule::Monthly { month_day: 31 })
        );
        for (name, weekday, month_day) in [
            ("weekly", None, None),
            ("weekly", Some(0), None),
            ("weekly", Some(8), None),
            ("weekly", Some(1), Some(1)),
            ("monthly", None, None),
            ("monthly", None, Some(0)),
            ("monthly", None, Some(32)),
            ("monthly", Some(1), Some(1)),
            ("manual", Some(1), None),
            ("daily", None, None),
        ] {
            assert!(
                RotationSchedule::from_parts(name, weekday, month_day).is_err(),
                "{name} {weekday:?} {month_day:?}"
            );
        }
        assert_eq!(RotationSchedule::Monthly { month_day: 3 }.name(), "monthly");
    }
}
