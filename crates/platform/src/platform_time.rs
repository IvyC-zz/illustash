//! Host calendar conversion; libc normalization and failure behavior are unchanged.
pub fn local_timestamp(tm: &mut libc::tm) -> libc::time_t {
    #[cfg(unix)]
    {
        unsafe { libc::mktime(tm) }
    }
    // UCRT exports mktime as the header inline over _mktime64, as used by Python.
    #[cfg(windows)]
    {
        extern "C" {
            fn _mktime64(tm: *mut libc::tm) -> i64;
        }
        unsafe { _mktime64(tm) }
    }
}

/// The host calendar `delta` seconds before now.
pub fn local_date(delta: i64) -> libc::tm {
    unsafe {
        let t = now() - delta;
        let mut out: libc::tm = std::mem::zeroed();
        #[cfg(unix)]
        libc::localtime_r(&t, &mut out);
        #[cfg(windows)]
        libc::localtime_s(&mut out, &t);
        out
    }
}

/// 只供测试：`ILLUSTASH_TEST_TODAY=YYYY-MM-DD` 时「现在」是当地这一天的正午（#301）。按今天算的窗口
/// （角色的「近 30 天」和「近 12 个月」）在固定期望里是录制那天的结果，测试把今天固定在录制日；产品不设置它。
/// 值不是合法日期时照常用当前时间。
fn now() -> libc::time_t {
    std::env::var("ILLUSTASH_TEST_TODAY")
        .ok()
        .and_then(|day| test_today(&day))
        .unwrap_or_else(|| unsafe { libc::time(std::ptr::null_mut()) })
}

fn test_today(day: &str) -> Option<libc::time_t> {
    let mut parts = day.split('-');
    let (y, m, d): (i32, i32, i32) =
        (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    if parts.next().is_some() {
        return None;
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = m - 1;
    tm.tm_mday = d;
    tm.tm_hour = 12;
    tm.tm_isdst = -1;
    let stamp = local_timestamp(&mut tm);
    // A normalized date (2026-02-30 → 03-02) is not the day asked for.
    (stamp != -1 && (tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday) == (y, m, d)).then_some(stamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_today_is_local_noon_of_a_valid_day() {
        let stamp = test_today("2026-10-07").unwrap();
        let mut out: libc::tm = unsafe { std::mem::zeroed() };
        #[cfg(unix)]
        unsafe {
            libc::localtime_r(&stamp, &mut out);
        }
        #[cfg(windows)]
        unsafe {
            libc::localtime_s(&mut out, &stamp);
        }
        assert_eq!((out.tm_year + 1900, out.tm_mon + 1, out.tm_mday, out.tm_hour), (2026, 10, 7, 12));
        let before = test_today("2026-10-07").unwrap() - 30 * 86400;
        unsafe {
            #[cfg(unix)]
            libc::localtime_r(&before, &mut out);
            #[cfg(windows)]
            libc::localtime_s(&mut out, &before);
        }
        assert_eq!((out.tm_year + 1900, out.tm_mon + 1, out.tm_mday), (2026, 9, 7));
        for bad in ["", "2026-10", "2026-02-30", "2026-13-01", "2026-10-07-1", "2026-10-x", " 2026-10-07"] {
            assert_eq!(test_today(bad), None, "{bad:?}");
        }
    }
    #[test]
    fn calendar_normalizes_leap_day_and_month_boundary() {
        for (year, month, day, expected) in [(2024, 2, 29, (2024, 3, 1)), (2023, 12, 31, (2024, 1, 1))] {
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            tm.tm_year = year - 1900;
            tm.tm_mon = month - 1;
            tm.tm_mday = day + 1;
            tm.tm_hour = 12;
            tm.tm_isdst = -1;
            assert_ne!(local_timestamp(&mut tm), -1);
            assert_eq!((tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday), expected);
        }
    }
    #[test]
    fn local_date_preserves_host_calendar() {
        // Compare against the host API within a stable second, without changing TZ.
        for _ in 0..10 {
            let before = unsafe { libc::time(std::ptr::null_mut()) };
            let actual = local_date(0);
            let after = unsafe { libc::time(std::ptr::null_mut()) };
            if before != after {
                continue;
            }
            let mut expected: libc::tm = unsafe { std::mem::zeroed() };
            #[cfg(unix)]
            unsafe {
                libc::localtime_r(&before, &mut expected);
            }
            #[cfg(windows)]
            unsafe {
                libc::localtime_s(&mut expected, &before);
            }
            assert_eq!(
                (actual.tm_year, actual.tm_mon, actual.tm_mday, actual.tm_hour, actual.tm_min, actual.tm_sec),
                (
                    expected.tm_year,
                    expected.tm_mon,
                    expected.tm_mday,
                    expected.tm_hour,
                    expected.tm_min,
                    expected.tm_sec
                )
            );
            return;
        }
        panic!("host clock did not remain within one second");
    }
}
