use std::io::{self, BufRead, Read, Write};

const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

pub(crate) fn read_message(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut content_length = None;
    let mut saw_header = false;
    loop {
        let mut line = Vec::new();
        let bytes = reader.read_until(b'\n', &mut line)?;
        if bytes == 0 {
            if saw_header {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LSP headers ended before the blank line",
                ));
            }
            return Ok(None);
        }
        saw_header = true;
        if line == b"\r\n" {
            break;
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "LSP header is not UTF-8"))?;
        let line = line.strip_suffix("\r\n").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP header is not CRLF terminated",
            )
        })?;
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP header is missing ':'",
            ));
        };
        if name.trim().eq_ignore_ascii_case("Content-Length") {
            content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
            })?);
        }
    }

    let length = content_length.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP message is missing Content-Length",
        )
    })?;
    if length > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP message exceeds the maximum size",
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "LSP body is not UTF-8"))
}

pub(crate) fn write_message(writer: &mut impl Write, body: &str) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{MAX_MESSAGE_SIZE, read_message, write_message};

    fn must<T>(result: std::io::Result<T>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("expected I/O success: {error}"),
        }
    }

    #[test]
    fn frames_utf8_message_round_trip() {
        let body = "{\"text\":\"\u{65e5}\u{672c}\u{8a9e}\"}";
        let mut wire = Vec::new();
        must(write_message(&mut wire, body));
        let mut reader = Cursor::new(wire);
        assert_eq!(must(read_message(&mut reader)), Some(body.to_owned()));
        assert_eq!(must(read_message(&mut reader)), None);
    }

    #[test]
    fn rejects_missing_or_invalid_content_length() {
        let mut missing = Cursor::new(b"X-Test: yes\r\n\r\n{}".to_vec());
        assert!(read_message(&mut missing).is_err());
        let mut invalid = Cursor::new(b"Content-Length: nope\r\n\r\n{}".to_vec());
        assert!(read_message(&mut invalid).is_err());
    }

    #[test]
    fn rejects_oversized_messages_before_allocation() {
        let header = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_SIZE + 1);
        let mut reader = Cursor::new(header.into_bytes());
        assert!(read_message(&mut reader).is_err());
    }
}
