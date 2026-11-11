//! Time-of-day schedule: which wallpaper is due now and when the next change is.

use std::collections::BTreeMap;

fn seconds(hhmm: &str) -> Option<u32> {
    let (h, m) = hhmm.split_once(':')?;
    let (h, m): (u32, u32) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h < 24 && m < 60).then_some(h * 3600 + m * 60)
}

/// For `now` (seconds since local midnight): the wallpaper due, and seconds until the next entry starts.
/// Entries with an unreadable time are ignored.
pub fn due(hours: &BTreeMap<String, String>, now: u32) -> Option<(&str, u32)> {
    let mut slots: Vec<(u32, &str)> = hours.iter().filter_map(|(k, v)| Some((seconds(k)?, v.as_str()))).collect();
    slots.sort();
    // Before the first entry of the day, yesterday's last one still applies.
    let current = slots.iter().rev().find(|(t, _)| *t <= now).or(slots.last())?.1;
    let next = slots.iter().find(|(t, _)| *t > now).map_or(slots[0].0 + 86400, |(t, _)| *t);
    Some((current, next - now))
}

/// Seconds since local midnight.
pub fn local_seconds() -> u32 {
    // SAFETY: time() and localtime_r() only write into the locals passed to them.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        (tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_current_slot_and_next_boundary() {
        let hours: BTreeMap<String, String> =
            [("07:00", "day"), ("19:30", "night"), ("oops", "x"), ("25:00", "y")].map(|(k, v)| (k.into(), v.into())).into();
        assert_eq!(due(&hours, 8 * 3600), Some(("day", 11 * 3600 + 1800)));
        assert_eq!(due(&hours, 7 * 3600), Some(("day", 12 * 3600 + 1800)));
        assert_eq!(due(&hours, 23 * 3600), Some(("night", 8 * 3600)));
        // Early morning belongs to last night's entry.
        assert_eq!(due(&hours, 3600), Some(("night", 6 * 3600)));
        assert_eq!(due(&BTreeMap::new(), 0), None);
    }
}
