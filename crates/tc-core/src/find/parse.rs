use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};

/// Whole number with an optional K, M, G or T suffix (1024 steps) and optional B: `500`, `10k`, `2 MB`.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim().trim_end_matches(['b', 'B']);
    let (digits, mult) = match t.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&t[..t.len() - 1], 1u64 << 10),
        Some('M') => (&t[..t.len() - 1], 1 << 20),
        Some('G') => (&t[..t.len() - 1], 1 << 30),
        Some('T') => (&t[..t.len() - 1], 1 << 40),
        _ => (t, 1),
    };
    digits
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mult))
        .ok_or_else(|| format!("not a size: {}", s.trim()))
}

/// Date `YYYY-MM-DD` in local time: its start, or with `end` the start of the next day
/// (an exclusive bound, so the whole last second of that day is included).
pub fn parse_date(s: &str, end: bool) -> Result<Timestamp, String> {
    let bad = || format!("not a date (YYYY-MM-DD): {}", s.trim());
    let date: Date = s.trim().parse().map_err(|_| bad())?;
    let start = date
        .to_zoned_datetime(TimeZone::system())
        .map_err(|_| bad())?
        .timestamp();
    if !end {
        return Ok(start);
    }
    let next = date.tomorrow().map_err(|_| bad())?;
    next.to_zoned_datetime(TimeZone::system())
        .map(|z| z.timestamp())
        .map_err(|_| bad())
}

/// Age like `30m`, `12h`, `7d` or `2w` (a bare number = days): the time that long ago.
pub fn parse_age(s: &str) -> Result<Timestamp, String> {
    let bad = || format!("not an age (30m, 12h, 7d, 2w): {}", s.trim());
    let t = s.trim().to_ascii_lowercase();
    let unit = |n: usize| &t[..t.len() - n];
    let (digits, secs) = match t.chars().last() {
        Some('m') => (unit(1), 60),
        Some('h') => (unit(1), 3600),
        Some('d') => (unit(1), 86_400),
        Some('w') => (unit(1), 604_800),
        _ => (t.as_str(), 86_400),
    };
    // u32 days fit i64 seconds, and negative ages are refused
    let n: u32 = digits.trim().parse().map_err(|_| bad())?;
    Timestamp::now()
        .checked_sub(SignedDuration::from_secs(i64::from(n) * secs))
        .map_err(|_| bad())
}

/// Comma separated names, trimmed; empty entries dropped.
pub fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Extensions as typed (`.rs, toml`): lowercase, without dots.
pub fn parse_exts(s: &str) -> Vec<String> {
    parse_list(s)
        .into_iter()
        .map(|e| e.trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}
