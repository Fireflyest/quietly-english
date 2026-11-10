//! 时间与“学习日”换算。所有落库时间用 UTC，统计按本地日期切分。

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

/// 本地时区下的“今天”。
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// UTC 时间戳对应的本地学习日。
pub fn local_day(ts: DateTime<Utc>) -> NaiveDate {
    ts.with_timezone(&Local).date_naive()
}

/// 本地学习日 00:00 对应的 UTC 时间。
pub fn day_start(day: NaiveDate) -> DateTime<Utc> {
    let local = Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight is valid"))
        .single()
        .unwrap_or_else(|| Local.from_utc_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight")));
    local.with_timezone(&Utc)
}

pub fn format_day(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

pub fn format_short(day: NaiveDate) -> String {
    day.format("%m-%d").to_string()
}

/// 解析数据库里的 RFC3339 时间戳。
pub fn parse_rfc(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// 仅供示例/测试使用的固定时间。
#[cfg(test)]
pub fn utc(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
}
