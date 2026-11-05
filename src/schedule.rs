//! Active windows: several per day, per weekday set, crossing midnight allowed, DST-correct.

use crate::config::Window;
use anyhow::{Context, Result, bail};
use jiff::Zoned;
use jiff::civil::{Time, Weekday};

/// True when `now` falls in any window. No windows = always active.
pub fn is_active(windows: &[Window], now: &Zoned) -> Result<bool> {
    if windows.is_empty() {
        return Ok(true);
    }
    for w in windows {
        let local = match &w.tz {
            Some(tz) => now.in_tz(tz).with_context(|| format!("unknown time zone {tz:?}"))?,
            None => now.clone(),
        };
        let days = parse_days(&w.days)?;
        let t = local.time();
        let today = local.weekday();
        let yesterday = today.previous();
        for range in &w.windows {
            let (start, end) = parse_range(range)?;
            let hit = if start <= end {
                days.contains(&today) && t >= start && t < end
            } else {
                // Crosses midnight: the evening part belongs to `today`, the morning part to the
                // day the window started on.
                (days.contains(&today) && t >= start) || (days.contains(&yesterday) && t < end)
            };
            if hit {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Validate windows without evaluating them (used when config is loaded/edited).
pub fn validate(windows: &[Window]) -> Result<()> {
    for w in windows {
        parse_days(&w.days)?;
        for r in &w.windows {
            parse_range(r)?;
        }
        if let Some(tz) = &w.tz {
            jiff::tz::TimeZone::get(tz).with_context(|| format!("unknown time zone {tz:?}"))?;
        }
    }
    Ok(())
}

fn parse_days(s: &str) -> Result<Vec<Weekday>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part {
            "daily" | "all" | "every day" => return Ok(ALL.to_vec()),
            "weekdays" => out.extend(&ALL[..5]),
            "weekends" => out.extend(&ALL[5..]),
            _ => match part.split_once('-') {
                Some((a, b)) => {
                    let (mut d, end) = (day(a)?, day(b)?);
                    loop {
                        out.push(d);
                        if d == end {
                            break;
                        }
                        d = d.next();
                    }
                }
                None => out.push(day(part)?),
            },
        }
    }
    if out.is_empty() {
        bail!("empty days {s:?}");
    }
    Ok(out)
}

const ALL: [Weekday; 7] =
    [Weekday::Monday, Weekday::Tuesday, Weekday::Wednesday, Weekday::Thursday, Weekday::Friday, Weekday::Saturday, Weekday::Sunday];

fn day(s: &str) -> Result<Weekday> {
    let s = s.trim().to_ascii_lowercase();
    let i = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
        .iter()
        .position(|d| s.starts_with(d))
        .with_context(|| format!("bad weekday {s:?} (use mon..sun)"))?;
    Ok(ALL[i])
}

fn parse_range(s: &str) -> Result<(Time, Time)> {
    let (a, b) = s.split_once('-').with_context(|| format!("bad window {s:?} (use HH:MM-HH:MM)"))?;
    let t = |x: &str| -> Result<Time> {
        let x = x.trim();
        if x == "24:00" {
            return Ok(Time::MAX);
        }
        Time::strptime("%H:%M", x).with_context(|| format!("bad time {x:?} in {s:?}"))
    };
    let (start, end) = (t(a)?, t(b)?);
    if start == end {
        bail!("empty window {s:?}");
    }
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(days: &str, ranges: &[&str]) -> Window {
        Window { days: days.into(), windows: ranges.iter().map(|s| s.to_string()).collect(), tz: Some("Europe/Zurich".into()) }
    }
    fn at(s: &str) -> Zoned {
        format!("{s}[Europe/Zurich]").parse().unwrap()
    }

    #[test]
    fn empty_is_always() {
        assert!(is_active(&[], &at("2026-11-05T03:00")).unwrap());
    }

    #[test]
    fn multiple_windows_per_day() {
        // 2026-11-05 is a Thursday.
        let s = [w("mon-fri", &["08:00-09:00", "11:00-13:00", "17:00-22:00"])];
        assert!(is_active(&s, &at("2026-11-05T08:30")).unwrap());
        assert!(!is_active(&s, &at("2026-11-05T09:00")).unwrap());
        assert!(is_active(&s, &at("2026-11-05T12:59")).unwrap());
        assert!(!is_active(&s, &at("2026-11-05T14:00")).unwrap());
        assert!(!is_active(&s, &at("2026-11-07T12:00")).unwrap()); // Saturday
    }

    #[test]
    fn crossing_midnight_belongs_to_start_day() {
        let s = [w("fri", &["22:00-06:00"])];
        assert!(is_active(&s, &at("2026-11-06T23:00")).unwrap()); // Fri night
        assert!(is_active(&s, &at("2026-11-07T05:00")).unwrap()); // Sat early = Fri window
        assert!(!is_active(&s, &at("2026-11-06T05:00")).unwrap()); // Fri early = Thu window
    }

    #[test]
    fn day_lists() {
        assert_eq!(parse_days("sat,sun").unwrap().len(), 2);
        assert_eq!(parse_days("fri-mon").unwrap().len(), 4);
        assert_eq!(parse_days("daily").unwrap().len(), 7);
        assert!(parse_days("someday").is_err());
        assert!(parse_range("9-17").is_err());
        assert!(parse_range("17:00-24:00").is_ok());
    }

    #[test]
    fn validates_tz() {
        let mut bad = w("mon", &["08:00-09:00"]);
        bad.tz = Some("Mars/Olympus".into());
        assert!(validate(&[bad]).is_err());
    }
}
