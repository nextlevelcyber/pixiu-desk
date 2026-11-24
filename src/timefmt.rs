//! UTC 时间的文本格式（不引入日期库）。

/// 公历日期：从 1970-01-01 起的天数换算（Howard Hinnant 的 civil_from_days）。
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// 从公历日期到 1970-01-01 起的天数（civil 的逆运算）。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// ISO 8601 时间（`2026-10-11T06:16:08.000Z`、`+08:00` 偏移、或不带时区按 UTC）转为纳秒。
/// 与 Python `datetime.fromisoformat` 加 `live_report.py` 的换算一致：精度到微秒。
pub fn parse_iso_ns(text: &str) -> Option<i64> {
    let t = text.trim();
    let (date, rest) = t.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    if dp.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // 时区：Z、+HH:MM、-HH:MM，或没有。
    let (clock, offset_s) = if let Some(c) = rest.strip_suffix('Z') {
        (c, 0i64)
    } else if let Some(i) = rest.rfind(['+', '-']) {
        let (c, z) = rest.split_at(i);
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let (hh, mm) = z[1..].split_once(':')?;
        (
            c,
            sign * (hh.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60),
        )
    } else {
        (rest, 0)
    };
    let (hms, frac) = match clock.split_once('.') {
        Some((a, b)) => (a, b),
        None => (clock, ""),
    };
    let mut hp = hms.split(':');
    let hh: i64 = hp.next()?.parse().ok()?;
    let mi: i64 = hp.next()?.parse().ok()?;
    let ss: i64 = hp.next()?.parse().ok()?;
    if hp.next().is_some()
        || hh > 23
        || mi > 59
        || ss > 59
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // 小数秒截到微秒（Python datetime 的精度）。
    let micros: i64 = format!("{:0<6}", &frac[..frac.len().min(6)]).parse().ok()?;
    let secs = days_from_civil(y, m, d) * 86_400 + hh * 3600 + mi * 60 + ss - offset_s;
    Some(secs * 1_000_000_000 + micros * 1000)
}

/// `2026-10-28T03:00:00.007Z`（与 `live_report.py` 的 `utc(ns)` 一致，毫秒截断）。
pub fn utc_millis(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let rest = ns.rem_euclid(1_000_000_000);
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60,
        rest / 1_000_000
    )
}

/// `YYYYMMDD`（UTC）。
pub fn utc_day(ns: i64) -> String {
    let (y, m, d) = civil(ns.div_euclid(1_000_000_000).div_euclid(86_400));
    format!("{y:04}{m:02}{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_live_report() {
        assert_eq!(
            utc_millis(1_791_450_000_000_000_000),
            "2026-10-08T09:00:00.000Z"
        );
        assert_eq!(
            utc_millis(1_793_152_800_007_890_853),
            "2026-10-28T02:00:00.007Z"
        );
        assert_eq!(utc_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc_day(1_793_152_800_007_890_853), "20261028");
    }

    #[test]
    fn iso_parsing_matches_python() {
        // python: (datetime.fromisoformat("2026-10-11T06:16:08+00:00") - epoch) // 1 µs * 1000
        assert_eq!(
            parse_iso_ns("2026-10-11T06:16:08.000Z"),
            Some(1_791_699_368_000_000_000)
        );
        assert_eq!(
            parse_iso_ns("2026-10-11T06:16:08Z"),
            Some(1_791_699_368_000_000_000)
        );
        assert_eq!(
            parse_iso_ns("2026-10-11T06:16:08.1234567Z"),
            Some(1_791_699_368_123_456_000)
        );
        assert_eq!(
            parse_iso_ns("2026-10-11T14:16:08+08:00"),
            Some(1_791_699_368_000_000_000)
        );
        assert_eq!(
            parse_iso_ns("2026-10-11T06:16:08"),
            Some(1_791_699_368_000_000_000),
            "无时区按 UTC"
        );
        assert_eq!(
            parse_iso_ns("2024-02-29T00:00:00Z"),
            Some(1_709_164_800_000_000_000)
        );
        assert_eq!(parse_iso_ns("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso_ns("garbage"), None);
        for ns in [0i64, 1_791_699_368_007_000_000, 1_795_000_000_123_000_000] {
            assert_eq!(parse_iso_ns(&utc_millis(ns)), Some(ns), "往返 {ns}");
        }
    }
}
