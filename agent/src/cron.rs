//! Cron schedules as Vercel's `crons` use them: five fields (minute, hour,
//! day of month, month, day of week), always in UTC.
//!
//! Each field takes `*`, numbers, ranges `a-b`, steps `*/n` and `a-b/n`, and
//! comma lists. Like Vixie cron, when both day of month and day of week are
//! restricted a day matches if either does.

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    minutes: u64,
    hours: u32,
    /// Bits 1..=31.
    days: u32,
    /// Bits 1..=12.
    months: u16,
    /// Bits 0..=6, Sunday = 0.
    weekdays: u8,
    days_restricted: bool,
    weekdays_restricted: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn value(s: &str, names: &[&str], first_name: u32) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    let lower = s.to_ascii_lowercase();
    names.iter().position(|n| *n == lower).map(|i| i as u32 + first_name)
}

/// Bits set for a field. `max` is inclusive.
fn field(text: &str, min: u32, max: u32, names: &[&str], first_name: u32) -> Result<u64, String> {
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let s: u32 = s.parse().map_err(|_| format!("bad step in `{part}`"))?;
                if s == 0 {
                    return Err(format!("step 0 in `{part}`"));
                }
                (r, s)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let a = value(a, names, first_name).ok_or_else(|| format!("bad value `{a}`"))?;
            let b = value(b, names, first_name).ok_or_else(|| format!("bad value `{b}`"))?;
            (a, b)
        } else {
            let a = value(range, names, first_name).ok_or_else(|| format!("bad value `{range}`"))?;
            // `5/15` means from 5 to the end, every 15.
            (a, if part.contains('/') { max } else { a })
        };
        if lo < min || hi > max || lo > hi {
            return Err(format!("`{part}` is outside {min}-{max}"));
        }
        // `step_by` can't overflow, whatever step a file asks for.
        for v in (lo..=hi).step_by(step as usize) {
            bits |= 1 << v;
        }
    }
    Ok(bits)
}

pub fn parse(expr: &str) -> Result<Schedule, String> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return Err(format!(
            "`{expr}` needs five fields (minute hour day-of-month month day-of-week)"
        ));
    }
    let mut weekdays = field(f[4], 0, 7, &WEEKDAYS, 0)?;
    // 7 is Sunday too.
    if weekdays & (1 << 7) != 0 {
        weekdays = (weekdays | 1) & 0x7f;
    }
    Ok(Schedule {
        minutes: field(f[0], 0, 59, &[], 0)?,
        hours: field(f[1], 0, 23, &[], 0)? as u32,
        days: field(f[2], 1, 31, &[], 0)? as u32,
        months: field(f[3], 1, 12, &MONTHS, 1)? as u16,
        weekdays: weekdays as u8,
        days_restricted: !f[2].starts_with('*'),
        weekdays_restricted: !f[4].starts_with('*'),
    })
}

impl Schedule {
    fn day_matches(&self, t: &DateTime<Utc>) -> bool {
        if self.months & (1 << t.month()) == 0 {
            return false;
        }
        let dom = self.days & (1 << t.day()) != 0;
        let dow = self.weekdays & (1 << t.weekday().num_days_from_sunday()) != 0;
        match (self.days_restricted, self.weekdays_restricted) {
            (true, true) => dom || dow,
            (true, false) => dom,
            (false, true) => dow,
            (false, false) => true,
        }
    }

    /// Whether the schedule fires in the minute containing `t`.
    pub fn matches(&self, t: DateTime<Utc>) -> bool {
        self.minutes & (1 << t.minute()) != 0 && self.hours & (1 << t.hour()) != 0 && self.day_matches(&t)
    }

    /// The first minute strictly after `t` when it fires (within 5 years).
    pub fn next_after(&self, t: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let start = t.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        let mut day = Utc
            .with_ymd_and_hms(start.year(), start.month(), start.day(), 0, 0, 0)
            .single()?;
        for _ in 0..(366 * 5) {
            if self.day_matches(&day) {
                for h in 0..24u32 {
                    if self.hours & (1 << h) == 0 {
                        continue;
                    }
                    for m in 0..60u32 {
                        if self.minutes & (1 << m) == 0 {
                            continue;
                        }
                        let at = day + Duration::hours(h.into()) + Duration::minutes(m.into());
                        if at >= start {
                            return Some(at);
                        }
                    }
                }
            }
            day += Duration::days(1);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn parses_fields() {
        let s = parse("*/15 9-17 * * mon-fri").unwrap();
        assert!(s.matches(at("2026-10-22T09:30:00Z"))); // Thursday
        assert!(!s.matches(at("2026-10-22T09:31:00Z")));
        assert!(!s.matches(at("2026-10-22T18:00:00Z")));
        assert!(!s.matches(at("2026-10-25T10:00:00Z"))); // Sunday
        assert!(parse("0 0 1 1 *").unwrap().matches(at("2027-01-01T00:00:59Z")));
        assert!(parse("5,10 0 * * 7").unwrap().matches(at("2026-10-25T00:10:00Z")));
        assert!(parse("5/20 * * * *").unwrap().matches(at("2026-10-25T03:45:00Z")));
        assert!(!parse("5/20 * * * *").unwrap().matches(at("2026-10-25T03:40:00Z")));
        assert!(parse("0 12 * jan-mar,dec *")
            .unwrap()
            .matches(at("2026-12-03T12:00:00Z")));
    }

    #[test]
    fn rejects_bad_expressions() {
        for bad in [
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
            "* * * * 8",
            "@daily",
            "5/4294967296 * * * *",
        ] {
            assert!(parse(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn day_of_month_or_day_of_week() {
        // Both restricted: the 13th OR any Friday.
        let s = parse("0 0 13 * 5").unwrap();
        assert!(s.matches(at("2026-10-13T00:00:00Z"))); // Tuesday the 13th
        assert!(s.matches(at("2026-10-16T00:00:00Z"))); // Friday
        assert!(!s.matches(at("2026-10-15T00:00:00Z")));
        // Only day of week restricted: Fridays only.
        assert!(!parse("0 0 * * 5").unwrap().matches(at("2026-10-13T00:00:00Z")));
    }

    #[test]
    fn next_run() {
        let s = parse("30 4 * * *").unwrap();
        assert_eq!(
            s.next_after(at("2026-10-22T04:29:10Z")),
            Some(at("2026-10-22T04:30:00Z"))
        );
        assert_eq!(
            s.next_after(at("2026-10-22T04:30:00Z")),
            Some(at("2026-10-23T04:30:00Z"))
        );
        assert_eq!(
            parse("0 0 29 2 *").unwrap().next_after(at("2026-03-01T00:00:00Z")),
            Some(at("2028-02-29T00:00:00Z"))
        );
        assert_eq!(
            parse("0 0 31 2 *").unwrap().next_after(at("2026-01-01T00:00:00Z")),
            None
        );
    }
}
