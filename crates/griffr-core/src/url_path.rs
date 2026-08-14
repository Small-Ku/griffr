/// Join a CDN base URL and one launcher-native logical path while percent-encoding
/// each path segment. Launcher manifests use both slash styles, so normalize them
/// before encoding rather than treating a backslash as a literal path byte.
pub fn build_cdn_file_url(base_url: &str, logical_path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let logical = logical_path.trim_start_matches(['/', '\\']);
    let encoded = logical
        .replace('\\', "/")
        .split('/')
        .map(percent_encode_path_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("{base}/{encoded}")
}

fn percent_encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for &byte in segment.as_bytes() {
        if is_unreserved_path_byte(byte) {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(nibble_to_hex(byte >> 4));
            encoded.push(nibble_to_hex(byte & 0x0f));
        }
    }
    encoded
}

fn is_unreserved_path_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
    )
}

fn nibble_to_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + (nibble - 10)) as char,
        _ => unreachable!("nibble must be <= 15"),
    }
}

#[cfg(test)]
mod tests {
    use super::build_cdn_file_url;

    #[test]
    fn encodes_each_logical_path_segment() {
        assert_eq!(
            build_cdn_file_url("https://cdn.example/files/", "ui\\[uc]battle finish.ab"),
            "https://cdn.example/files/ui/%5Buc%5Dbattle%20finish.ab"
        );
    }
}
