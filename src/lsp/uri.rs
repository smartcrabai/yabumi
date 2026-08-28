use std::path::{Path, PathBuf};

pub(crate) fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    if !encoded.starts_with('/') {
        return None;
    }
    let decoded = percent_decode(encoded)?;
    let decoded = if cfg!(windows)
        && decoded.starts_with('/')
        && decoded.get(1..).is_some_and(is_windows_drive_path)
    {
        &decoded[1..]
    } else {
        &decoded
    };
    Some(PathBuf::from(decoded))
}

pub(crate) fn path_to_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let windows_drive = is_windows_drive_path(&path);
    let mut uri = String::from("file://");
    if windows_drive {
        uri.push('/');
    }
    uri.push_str(&percent_encode(&path, windows_drive));
    uri
}

fn is_windows_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn percent_decode(input: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(character) = chars.next() {
        if character != '%' {
            let mut encoded = [0; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            continue;
        }
        let high = chars.next()?.to_digit(16)?;
        let low = chars.next()?.to_digit(16)?;
        bytes.push(u8::try_from((high << 4) | low).ok()?);
    }
    String::from_utf8(bytes).ok()
}

fn percent_encode(path: &str, allow_drive_colon: bool) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
            || (allow_drive_colon && *byte == b':')
        {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
    }
    encoded
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'A' + value - 10),
        _ => unreachable!("a hexadecimal digit has four bits"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{path_to_uri, uri_to_path};

    fn path_string(uri: &str) -> String {
        match uri_to_path(uri) {
            Some(path) => path.to_string_lossy().into_owned(),
            None => panic!("expected a file URI: {uri}"),
        }
    }

    #[test]
    fn converts_utf8_file_uri() {
        assert_eq!(
            path_string("file:///tmp/a%20b/%E6%97%A5%E6%9C%AC"),
            "/tmp/a b/\u{65e5}\u{672c}"
        );
        assert_eq!(
            path_to_uri(Path::new("/tmp/a b/\u{65e5}\u{672c}")),
            "file:///tmp/a%20b/%E6%97%A5%E6%9C%AC"
        );
    }

    #[test]
    fn rejects_non_file_and_invalid_percent_sequences() {
        assert_eq!(uri_to_path("https://example.test/a"), None);
        assert_eq!(uri_to_path("file:///tmp/%zz"), None);
        assert_eq!(uri_to_path("file://relative/path"), None);
    }

    #[test]
    fn keeps_windows_drive_uri_shape() {
        assert_eq!(
            path_to_uri(Path::new("C:\\Users\\name")),
            "file:///C:/Users/name"
        );
        if cfg!(windows) {
            assert_eq!(path_string("file:///C:/Users/name"), "C:\\Users\\name");
        } else {
            assert_eq!(path_string("file:///C:/Users/name"), "/C:/Users/name");
        }
    }
}
