//! 宿主的日历换算；保持 libc 的规整化和失败行为不变。
pub fn local_timestamp(tm: &mut libc::tm) -> libc::time_t {
    #[cfg(unix)]
    {
        // SAFETY: `tm` 是独占的可变引用，mktime 只读写这个结构。
        unsafe { libc::mktime(tm) }
    }
    // UCRT 的 mktime 是头文件里包在 _mktime64 外面的内联函数，Python 用的也是它。
    #[cfg(windows)]
    {
        extern "C" {
            fn _mktime64(tm: *mut libc::tm) -> i64;
        }
        // SAFETY: `tm` 是独占的可变引用，_mktime64 只读写这个结构。
        unsafe { _mktime64(tm) }
    }
}

pub fn local_date(delta: i64) -> libc::tm {
    // SAFETY: time 的出参为空；libc::tm 只含整数（Unix 上另有可为空的时区名指针），全零是合法值；`t`、`out`
    // 是本地变量。
    unsafe {
        let t = libc::time(std::ptr::null_mut()) - delta;
        let mut out: libc::tm = std::mem::zeroed();
        #[cfg(unix)]
        libc::localtime_r(&t, &mut out);
        #[cfg(windows)]
        libc::localtime_s(&mut out, &t);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
