//! 顯示用格式化。

pub fn size(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", U[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", U[i])
    } else {
        format!("{v:.2} {}", U[i])
    }
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// FILETIME 轉成「N 天前」。
pub fn ago(filetime: u64) -> String {
    if filetime == 0 {
        return "—".into();
    }
    let now = crate::win::now_filetime();
    if filetime >= now {
        return "剛剛".into();
    }
    let secs = (now - filetime) / 10_000_000;
    match secs {
        0..=3599 => format!("{} 分鐘前", (secs / 60).max(1)),
        3600..=86399 => format!("{} 小時前", secs / 3600),
        86400..=2_591_999 => format!("{} 天前", secs / 86400),
        2_592_000..=31_535_999 => format!("{} 個月前", secs / 2_592_000),
        _ => format!("{} 年前", secs / 31_536_000),
    }
}

pub fn pct(part: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { part as f32 / total as f32 }
}
