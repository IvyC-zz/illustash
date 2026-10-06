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

pub fn local_date(delta: i64) -> libc::tm {
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
