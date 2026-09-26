const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];

/// 以 1024 為底的人類可讀大小：小於 100 顯示一位小數。
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{:.0}{}", value, UNITS[unit])
    } else {
        format!("{:.1}{}", value, UNITS[unit])
    }
}

/// 解析 "64K"、"10"、"1G"、"2MB"（不分大小寫）。
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().map_err(|_| format!("無效的大小：{s}"))?;
    let mult: u64 = match unit.to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" => 1 << 10,
        "M" | "MB" => 1 << 20,
        "G" | "GB" => 1 << 30,
        _ => return Err(format!("無效的單位：{unit}")),
    };
    n.checked_mul(mult).ok_or_else(|| format!("大小溢位：{s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_below_one_kib() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(1023), "1023B");
    }

    #[test]
    fn formats_with_one_decimal_below_100() {
        assert_eq!(format_size(1536), "1.5K");
        assert_eq!(format_size(3_865_470_566), "3.6G");
    }

    #[test]
    fn formats_without_decimal_at_or_above_100() {
        assert_eq!(format_size(150 * 1024 * 1024), "150M");
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("64K"), Ok(65_536));
        assert_eq!(parse_size("10"), Ok(10));
        assert_eq!(parse_size("1g"), Ok(1 << 30));
        assert_eq!(parse_size("2MB"), Ok(2 << 20));
    }

    #[test]
    fn rejects_invalid_sizes() {
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("5X").is_err());
    }
}
