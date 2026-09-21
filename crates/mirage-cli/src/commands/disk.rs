use mirage_types::MirageError;

/// Byte-size parser for `set-floor`/`--hysteresis`: plain integer bytes or a
/// decimal-prefixed suffix (B, K/KB/KiB, M/MB/MiB, G/GB/GiB, T/TB/TiB).
pub fn parse_byte_size(input: &str) -> Result<u64, MirageError> {
    let trimmed = input.trim();
    let split = trimmed
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(trimmed.len());
    let (digits, unit) = trimmed.split_at(split);
    let value: f64 = digits
        .parse()
        .map_err(|_| MirageError::invalid_argument("invalid byte size"))?;
    if !(value.is_finite() && value >= 0.0) {
        return Err(MirageError::invalid_argument("invalid byte size"));
    }
    let multiplier: u64 = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" => 1_000,
        "KIB" => 1 << 10,
        "M" | "MB" => 1_000_000,
        "MIB" => 1 << 20,
        "G" | "GB" => 1_000_000_000,
        "GIB" => 1 << 30,
        "T" | "TB" => 1_000_000_000_000,
        "TIB" => 1 << 40,
        _ => return Err(MirageError::invalid_argument("unknown byte-size unit")),
    };
    let bytes = value * multiplier as f64;
    if bytes > u64::MAX as f64 {
        return Err(MirageError::invalid_argument("byte size overflows u64"));
    }
    Ok(bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(parse_byte_size("0").unwrap(), 0);
        assert_eq!(parse_byte_size("150GiB").unwrap(), 150 << 30);
        assert_eq!(parse_byte_size("150GB").unwrap(), 150_000_000_000);
        assert_eq!(parse_byte_size("5MiB").unwrap(), 5 << 20);
        assert_eq!(parse_byte_size("4096").unwrap(), 4096);
        assert_eq!(
            parse_byte_size("1.5GiB").unwrap(),
            (1.5 * (1u64 << 30) as f64) as u64
        );
        assert_eq!(parse_byte_size("2T").unwrap(), 2_000_000_000_000);
        assert!(parse_byte_size("abc").is_err());
        assert!(parse_byte_size("-5").is_err());
        assert!(parse_byte_size("1QB").is_err());
        assert!(parse_byte_size("1e30").is_err());
    }
}
