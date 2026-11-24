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
}
