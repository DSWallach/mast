//! Five-field cron, evaluated in UTC minutes against an explicit IANA zone.
//! DOM/DOW use cron OR semantics. Repeated local minutes run only once.
use chrono::{Datelike, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use mast_proto::TaskSchedule;

#[derive(Debug)]
pub struct Cron {
    fields: Vec<Vec<u32>>,
    dom_any: bool,
    dow_any: bool,
    zone: Tz,
}
impl Cron {
    pub fn parse(expression: &str, timezone: &str) -> Result<Self, String> {
        if expression.len() > 256 {
            return Err("Cron expression is too long".into());
        }
        let parts: Vec<_> = expression.split_whitespace().collect();
        if parts.len() != 5 {
            return Err("Use five cron fields: minute hour day month weekday".into());
        }
        let limits = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
        let mut fields = Vec::new();
        for (part, (min, max)) in parts.iter().zip(limits) {
            let mut values = Vec::new();
            for item in part.split(',') {
                let mut split = item.split('/');
                let range = split.next().unwrap_or("");
                let step = split
                    .next()
                    .map(|v| v.parse::<u32>())
                    .transpose()
                    .map_err(|_| "Invalid cron step")?
                    .unwrap_or(1);
                if step == 0 || split.next().is_some() {
                    return Err("Invalid cron step".into());
                }
                let (start, end) = if range == "*" {
                    (min, max)
                } else if let Some((a, b)) = range.split_once('-') {
                    (
                        a.parse().map_err(|_| "Invalid cron range")?,
                        b.parse().map_err(|_| "Invalid cron range")?,
                    )
                } else {
                    let n = range.parse().map_err(|_| "Invalid cron number")?;
                    (n, if item.contains('/') { max } else { n })
                };
                if start < min || end > max || start > end {
                    return Err("Cron field is out of range".into());
                }
                values.extend((start..=end).step_by(step as usize));
            }
            values.sort_unstable();
            values.dedup();
            fields.push(values);
        }
        Ok(Self {
            fields,
            dom_any: parts[2] == "*",
            dow_any: parts[4] == "*",
            zone: timezone
                .parse()
                .map_err(|_| "Choose a valid IANA timezone")?,
        })
    }
    fn matches(&self, ms: i64) -> bool {
        let Some(utc) = Utc.timestamp_millis_opt(ms).single() else {
            return false;
        };
        let local = utc.with_timezone(&self.zone);
        // On fall-back, the later mapping of the same local minute is skipped.
        if self
            .zone
            .from_local_datetime(&local.naive_local())
            .earliest()
            .is_none_or(|first| first.with_timezone(&Utc) != utc)
        {
            return false;
        }
        let f = &self.fields;
        let dom = f[2].contains(&local.day());
        let dow = local.weekday().num_days_from_sunday();
        let dow = f[4].contains(&dow) || (dow == 0 && f[4].contains(&7));
        let day = match (self.dom_any, self.dow_any) {
            (true, _) => dow,
            (_, true) => dom,
            _ => dom || dow,
        };
        f[0].contains(&local.minute())
            && f[1].contains(&local.hour())
            && f[3].contains(&local.month())
            && day
    }
}
pub fn validate(schedule: &TaskSchedule) -> Result<(), String> {
    match schedule {
        TaskSchedule::Cron {
            expression,
            timezone,
        } => Cron::parse(expression, timezone).map(|_| ()),
        TaskSchedule::Once { at } => Utc
            .timestamp_millis_opt(*at)
            .single()
            .map(|_| ())
            .ok_or("Invalid one-time timestamp".into()),
    }
}

pub fn display_time(schedule: &TaskSchedule, at: i64) -> String {
    let Some(utc) = Utc.timestamp_millis_opt(at).single() else {
        return "Invalid time".into();
    };
    if let TaskSchedule::Cron { timezone, .. } = schedule {
        if let Ok(zone) = timezone.parse::<Tz>() {
            return utc
                .with_timezone(&zone)
                .format("%a, %b %e · %H:%M %Z (UTC%:z)")
                .to_string();
        }
    }
    utc.format("%a, %b %e · %H:%M UTC").to_string()
}
pub fn next(schedule: &TaskSchedule, after: i64) -> Result<Option<i64>, String> {
    match schedule {
        TaskSchedule::Once { at } => Ok((*at > after).then_some(*at)),
        TaskSchedule::Cron {
            expression,
            timezone,
        } => {
            let cron = Cron::parse(expression, timezone)?;
            let start = after
                .div_euclid(60_000)
                .checked_add(1)
                .and_then(|n| n.checked_mul(60_000))
                .ok_or("Timestamp overflow")?;
            // Bounded search, enough for leap-day schedules. Impossible dates yield None.
            for minute in 0..(366 * 24 * 60 * 5i64) {
                let Some(ms) = start.checked_add(minute * 60_000) else {
                    return Ok(None);
                };
                if cron.matches(ms) {
                    return Ok(Some(ms));
                }
            }
            Ok(None)
        }
    }
}
pub fn latest_due(
    schedule: &TaskSchedule,
    first: i64,
    now: i64,
    grace: u32,
) -> Result<Option<i64>, String> {
    if first > now {
        return Ok(None);
    }
    match schedule {
        TaskSchedule::Once { at } => Ok(Some(*at)),
        TaskSchedule::Cron {
            expression,
            timezone,
        } => {
            let cron = Cron::parse(expression, timezone)?;
            let floor = first.max(now.saturating_sub(i64::from(grace.max(1)) * 60_000));
            let mut at = now.div_euclid(60_000) * 60_000;
            while at >= floor {
                if cron.matches(at) {
                    return Ok(Some(at));
                }
                at -= 60_000;
            }
            Ok(Some(first))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ms(s: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .timestamp_millis()
    }
    fn schedule(s: &str) -> TaskSchedule {
        TaskSchedule::Cron {
            expression: s.into(),
            timezone: "America/New_York".into(),
        }
    }
    #[test]
    fn cron_and_timezone() {
        assert_eq!(
            next(&schedule("40 4 * * 1-5"), ms("2026-11-11T09:40:00Z")).unwrap(),
            Some(ms("2026-11-12T09:40:00Z"))
        );
        assert!(validate(&schedule("*/0 * * * *")).is_err());
        assert!(validate(&schedule("60 * * * *")).is_err());
    }
    #[test]
    fn daylight_saving() {
        assert_eq!(
            next(&schedule("30 2 * * *"), ms("2026-03-08T05:00:00Z")).unwrap(),
            Some(ms("2026-03-09T06:30:00Z"))
        );
        assert_eq!(
            next(&schedule("30 1 * * *"), ms("2026-11-01T05:30:00Z")).unwrap(),
            Some(ms("2026-11-02T06:30:00Z"))
        );
    }
    #[test]
    fn coalesces() {
        assert_eq!(
            latest_due(
                &schedule("*/30 9-17 * * 1-5"),
                ms("2026-11-11T14:00:00Z"),
                ms("2026-11-11T17:12:00Z"),
                30
            )
            .unwrap(),
            Some(ms("2026-11-11T17:00:00Z"))
        );
    }
}
