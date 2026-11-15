//! 宿主的日历换算；保持 libc 的规整化和失败行为不变。
pub fn local_timestamp(tm: &mut libc::tm) -> libc::time_t {
    #[cfg(unix)]
    {
        // SAFETY: `tm` 是独占的可变引用，mktime 只读写这个结构。
        unsafe { libc::mktime(tm) }
    }
    // UCRT 的 mktime 是头文件里包在 _mktime64 外面的内联函数，旧实现用的也是它，结果与以前一致。
    #[cfg(windows)]
    {
        extern "C" {
            fn _mktime64(tm: *mut libc::tm) -> i64;
        }
        // SAFETY: `tm` 是独占的可变引用，_mktime64 只读写这个结构。
        unsafe { _mktime64(tm) }
    }
}

/// 「现在」往前 `delta` 秒时的本机日历时间。
pub fn local_date(delta: i64) -> libc::tm {
    // SAFETY: time 的出参为空；libc::tm 只含整数（Unix 上另有可为空的时区名指针），全零是合法值；`t`、`out`
    // 是本地变量。
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
        // SAFETY: time 的出参为空指针，只返回当前时间。
        .unwrap_or_else(|| unsafe { libc::time(std::ptr::null_mut()) })
}

fn test_today(day: &str) -> Option<libc::time_t> {
    let mut parts = day.split('-');
    let (y, m, d): (i32, i32, i32) =
        (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    if parts.next().is_some() {
        return None;
    }
    // SAFETY: libc::tm 只含整数（Unix 上另有可为空的时区名指针），全零是合法值。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = m - 1;
    tm.tm_mday = d;
    tm.tm_hour = 12;
    tm.tm_isdst = -1;
    let stamp = local_timestamp(&mut tm);
    // 被规范化的日期（2026-02-30 → 03-02）不是要的那一天。
    (stamp != -1 && (tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday) == (y, m, d)).then_some(stamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_today_is_local_noon_of_a_valid_day() {
        let stamp = test_today("2026-10-07").unwrap();
        // SAFETY: libc::tm 只含整数（Unix 上另有可为空的时区名指针），全零是合法值。
        let mut out: libc::tm = unsafe { std::mem::zeroed() };
        #[cfg(unix)]
        // SAFETY: `stamp`、`out` 是本地变量。
        unsafe {
            libc::localtime_r(&stamp, &mut out);
        }
        #[cfg(windows)]
        // SAFETY: `stamp`、`out` 是本地变量。
        unsafe {
            libc::localtime_s(&mut out, &stamp);
        }
        assert_eq!((out.tm_year + 1900, out.tm_mon + 1, out.tm_mday, out.tm_hour), (2026, 10, 7, 12));
        let before = test_today("2026-10-07").unwrap() - 30 * 86400;
        // SAFETY: `before`、`out` 是本地变量。
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
            // SAFETY: libc::tm 只含整数（Unix 上另有可为空的时区名指针），全零是合法值。
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
        // 在同一秒之内与宿主 API 的结果比较，不改 TZ。
        for _ in 0..10 {
            // SAFETY: time 的出参为空。
            let before = unsafe { libc::time(std::ptr::null_mut()) };
            let actual = local_date(0);
            // SAFETY: time 的出参为空。
            let after = unsafe { libc::time(std::ptr::null_mut()) };
            if before != after {
                continue;
            }
            // SAFETY: libc::tm 只含整数（Unix 上另有可为空的时区名指针），全零是合法值。
            let mut expected: libc::tm = unsafe { std::mem::zeroed() };
            #[cfg(unix)]
            // SAFETY: `before`、`expected` 是本地变量。
            unsafe {
                libc::localtime_r(&before, &mut expected);
            }
            #[cfg(windows)]
            // SAFETY: `before`、`expected` 是本地变量。
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
