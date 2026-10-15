//! House-number helpers shared by the indexer (mid-street median) and the
//! service (nearest-number tier).

/// The leading decimal number of a BeSt house number: `"32"` → 32,
/// `"32B"` → 32, `"32-34"` → 32. `None` when it doesn't start with a digit
/// or overflows `u32`.
pub fn leading_number(s: &str) -> Option<u32> {
    let end = s.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return None;
    }
    s[..end].parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_suffixes_and_garbage() {
        assert_eq!(leading_number("32"), Some(32));
        assert_eq!(leading_number("32B"), Some(32));
        assert_eq!(leading_number("32-34"), Some(32));
        assert_eq!(leading_number("A12"), None);
        assert_eq!(leading_number(""), None);
        assert_eq!(leading_number("99999"), Some(99999));
        assert_eq!(leading_number("99999999999"), None);
    }
}
