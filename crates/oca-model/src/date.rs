//! UTC calendar dates for provenance fields (`created:`, `imported:`) —
//! not a clock feature: files carry their date so the filesystem metadata
//! never has to be trusted, and nothing ever reads it back for logic.

/// Today's UTC date (`YYYY-MM-DD`).
pub fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    utc_date((secs / 86_400) as i64)
}

/// Days-since-epoch → civil date (Howard Hinnant's `civil_from_days`).
pub fn utc_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_date_matches_known_epochs() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(19_723), "2024-01-01");
        assert_eq!(utc_date(20_753), "2026-10-27");
    }

    #[test]
    fn today_is_a_wellformed_date() {
        let today = today_utc();
        assert_eq!(today.len(), 10, "{today}");
        assert_eq!(&today[4..5], "-", "{today}");
        assert_eq!(&today[7..8], "-", "{today}");
    }
}
