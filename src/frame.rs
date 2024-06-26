use std::io::Cursor;
use std::error::Error;
use std::str;
use bytes::Buf;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Clone)]
pub enum Frame {
    Continuation(Option<FragmentedMessage>),
    Text(String),
    Binary(Vec<u8>),
    Close(StatusCode),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum StatusCode {
    Normal = 1000,
    ProtocolError = 1002,
    InvalidDataFormat = 1007,
}

#[derive(Debug, Clone)]
pub enum FragmentedMessage {
    Text(Vec<Vec<u8>>),
    Binary(Vec<Vec<u8>>),
}

impl FragmentedMessage {
    fn is_empty(&self) -> bool {
        matches!(
            self,
            FragmentedMessage::Text(messages) | FragmentedMessage::Binary(messages) if messages.is_empty()
        )
    }

    fn push(&mut self, message: Vec<u8>) {
        match self {
            FragmentedMessage::Text(messages) |
            FragmentedMessage::Binary(messages) => messages.push(message),
        }
    }

    fn invalid(&self) -> bool {
        matches!(
            self,
            FragmentedMessage::Text(messages) if String::from_utf8(messages.concat()).is_err()
        )
    }

    fn response(&self) -> Vec<u8> {
        let (message, first_byte) = match self {
            FragmentedMessage::Text(messages) => (messages.concat(), 0b1000_0001),
            FragmentedMessage::Binary(messages) => (messages.concat(), 0b1000_0010),
        };
        let mut response = vec![first_byte];
        response.extend(Frame::payload_length_response(message.len()));
        response.extend(message);
        response
    }
}

impl Frame {
    pub fn parse(
        src: &mut Cursor<&[u8]>,
        fragmented_message: &mut FragmentedMessage,
    ) -> Result<Frame> {
        let f_byte = get_u8(src)?;
        let fin = f_byte & 0b1000_0000 != 0;
        let rsv = f_byte & 0b0111_0000 != 0;
        let opcode = f_byte & 0b0000_1111;

        let s_byte = get_u8(src)?;
        let mask = s_byte & 0b1000_0000 != 0;
        let mut payload_length = (s_byte & 0b0111_1111) as usize;

        if rsv || !mask {
            return Ok(Frame::Close(StatusCode::ProtocolError));
        }

        payload_length = Frame::payload_length(src, payload_length)?;
        let mask = Frame::mask(src)?;
        let message = Frame::decoded_message(src, payload_length, &mask)?;

        if !fin {
            match opcode {
                0x0 if fragmented_message.is_empty() => return Ok(Frame::Close(StatusCode::ProtocolError)),
                0x0 | 0x1 => fragmented_message.push(message),
                0x2 => {
                    *fragmented_message = FragmentedMessage::Binary(Vec::new());
                    fragmented_message.push(message)
                }
                _ => return Ok(Frame::Close(StatusCode::ProtocolError)),
            }
            return Ok(Frame::Continuation(None));
        }

        match opcode {
            0x0 if fragmented_message.is_empty() => Ok(Frame::Close(StatusCode::ProtocolError)),
            0x0 if fragmented_message.invalid() => Ok(Frame::Close(StatusCode::InvalidDataFormat)),
            0x0 => {
                fragmented_message.push(message);
                Ok(Frame::Continuation(Some(fragmented_message.clone())))
            }
            0x1 | 0x2 if !fragmented_message.is_empty() => Ok(Frame::Close(StatusCode::ProtocolError)),
            0x1 => match String::from_utf8(message) {
                Ok(message) => Ok(Frame::Text(message)),
                Err(_) => Ok(Frame::Close(StatusCode::InvalidDataFormat))
            }
            0x2 => Ok(Frame::Binary(message)),
            0x8 if payload_length == 0 => Ok(Frame::Close(StatusCode::Normal)),
            0x8 if (2..=125).contains(&payload_length) => Frame::parse_close_frame(message),
            0x9 if (0..=125).contains(&payload_length) => Ok(Frame::Ping(message)),
            0xA => Ok(Frame::Pong(message)),
            _ => Ok(Frame::Close(StatusCode::ProtocolError)),
        }
    }

    fn payload_length(src: &mut Cursor<&[u8]>, payload_length: usize) -> Result<usize> {
        match payload_length {
            0..=125 => Ok(payload_length),
            126 => Ok(get_u16(src)? as usize),
            127 => Ok(get_u64(src)? as usize),
            _ => Err("Invalid length".into()),
        }
    }

    fn mask(src: &mut Cursor<&[u8]>) -> Result<[u8; 4]> {
        if src.remaining() < 4 {
            return Err("Cannot get the mask".into());
        }
        let mut mask = [0; 4];
        src.copy_to_slice(&mut mask);
        Ok(mask)
    }

    fn decoded_message(
        src: &mut Cursor<&[u8]>,
        payload_length: usize,
        mask: &[u8; 4]
    ) -> Result<Vec<u8>> {
        if src.remaining() < payload_length {
            return Err("Cannot decode message".into())
        }
        let mut encoded = vec![0; payload_length];
        src.copy_to_slice(&mut encoded);
        Ok(encoded
            .iter()
            .enumerate()
            .map(|(i, val)| val ^ mask[i % 4])
            .collect()
        )
    }

    fn parse_close_frame(message: Vec<u8>) -> Result<Frame> {
        let valid = [1000, 1001, 1002, 1003, 1007, 1008, 1009,
            1010, 1011, 3000, 3999, 4000, 4999];
        let status_code = (&message[0..2]).get_u16();
        if !valid.contains(&status_code) {
            return Ok(Frame::Close(StatusCode::ProtocolError));
        }
        match str::from_utf8(&message[2..]) {
            Ok(_) => Ok(Frame::Close(StatusCode::Normal)),
            Err(_) => Ok(Frame::Close(StatusCode::ProtocolError))
        }
    }

    pub fn response(&self) -> Option<Vec<u8>> {
        let mut response = Vec::new();
        match self {
            Frame::Continuation(Some(message)) => response.extend(message.response()),
            Frame::Text(message) => {
                response.push(0b1000_0001);
                let payload = message.as_bytes();
                response.extend(Frame::payload_length_response(payload.len()));
                response.extend(payload);
            }
            Frame::Binary(message) => {
                response.push(0b1000_0010);
                response.extend(Frame::payload_length_response(message.len()));
                response.extend(message);
            }
            Frame::Close(status_code) => {
                response.extend([0b1000_1000, 0b0000_0010]);
                response.extend((status_code.clone() as u16).to_be_bytes());
            }
            Frame::Ping(message) => {
                response.push(0b1000_1010);
                response.push(message.len() as u8);
                response.extend(message);
            }
            _ => {}
        }
        if !response.is_empty() { Some(response) } else { None }
    }

    fn payload_length_response(payload_length: usize) -> Vec<u8> {
        let mut payload_length_info = Vec::new();
        match payload_length {
            0..=125 => payload_length_info.push(payload_length as u8),
            126..= 65535 => {
                payload_length_info.push(126);
                payload_length_info.extend((payload_length as u16).to_be_bytes());
            }
            _ => {
                payload_length_info.push(127);
                payload_length_info.extend((payload_length as u64).to_be_bytes());
            }
        }
        payload_length_info
    }

    pub fn is_close(&self) -> bool {
        matches!(self, Frame::Close(_))
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Frame::Text(_))
    }

    pub fn is_binary(&self) -> bool {
        matches!(self, Frame::Binary(_))
    }

    pub fn is_continuation(&self) -> bool {
        matches!(self, Frame::Continuation(_))
    }
}

fn get_u8(src: &mut Cursor<&[u8]>) -> Result<u8> {
    if !src.has_remaining() {
        return Err("Cannot get u8".into());
    }
    Ok(src.get_u8())
}

fn get_u16(src: &mut Cursor<&[u8]>) -> Result<u16> {
    if src.remaining() < 2 {
        return Err("Cannot get u16".into());
    }
    Ok(src.get_u16())
}

fn get_u64(src: &mut Cursor<&[u8]>) -> Result<u64> {
    if src.remaining() < 8 {
        return Err("Cannot get u64".into());
    }
    Ok(src.get_u64())
}

pub trait VecExt {
    fn is_close(&self) -> bool;
}

impl VecExt for Vec<u8> {
    fn is_close(&self) -> bool {
        self.first().map_or(false, |&byte| byte == 0b1000_1000)
    }
}