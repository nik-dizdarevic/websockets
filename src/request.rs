use std::collections::HashMap;
use std::str;
use std::str::Utf8Error;
use sha1::{Sha1, Digest};
use base64::prelude::*;
use base64::prelude::BASE64_STANDARD;

pub struct Request<'a> {
    data: &'a str,
}

impl<'a> Request<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self, Utf8Error> {
        str::from_utf8(data)
            .map(|request| Self { data: request })
    }

    fn parse_headers(&self) -> HashMap<String, String> {
        self.data
            .lines()
            .skip(1)
            .filter_map(|line| {
                let mut split = line.splitn(2, ':');
                let header = split.next()?.trim().to_string().to_lowercase();
                let value = split.next()?.trim().to_string();
                Some((header, value))
            })
            .collect()
    }

    fn websocket_accept_key(&self, key: &str) -> String {
        let mut sha1 = Sha1::new();
        sha1.update(key.as_bytes());
        sha1.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let hash = sha1.finalize();
        BASE64_STANDARD.encode(hash)
    }

    pub fn response(&self) -> Option<String> {
        let headers = self.parse_headers();
        headers.get("sec-websocket-key").map(|key| {
            let accept_key = self.websocket_accept_key(key);
            format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
                Upgrade: websocket\r\n\
                Connection: Upgrade\r\n\
                Sec-WebSocket-Accept: {accept_key}\r\n\r\n"
            )
        })
    }
}