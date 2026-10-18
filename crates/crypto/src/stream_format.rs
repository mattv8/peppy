use crate::{CryptoError, STREAM_CHUNK_BYTES};
use libsodium_rs::crypto_secretstream::xchacha20poly1305 as stream;
use std::io::{self, Read};

pub(crate) const MAGIC: &[u8; 5] = b"PPSS\x01";
pub(crate) const HEADER_BYTES: usize = stream::HEADERBYTES;
pub(crate) const FINAL_FRAME_BYTES: usize = stream::ABYTES;
pub(crate) const MAX_FRAME_BYTES: usize = STREAM_CHUNK_BYTES + stream::ABYTES;

pub(crate) fn validate(input: &mut impl Read) -> Result<(), CryptoError> {
    let mut magic = [0; MAGIC.len()];
    read_exact_or_invalid(input, &mut magic)?;
    if magic != *MAGIC {
        return Err(CryptoError::InvalidStream);
    }

    let mut header = [0; HEADER_BYTES];
    read_exact_or_invalid(input, &mut header)?;

    let mut frame = vec![0; MAX_FRAME_BYTES];
    loop {
        let Some(first_length_byte) = read_byte(input)? else {
            return Err(CryptoError::InvalidStream);
        };
        let mut length = [first_length_byte, 0, 0, 0];
        read_exact_or_invalid(input, &mut length[1..])?;
        let length = u32::from_be_bytes(length) as usize;

        if length == FINAL_FRAME_BYTES {
            read_exact_or_invalid(input, &mut frame[..length])?;
            return require_eof(input);
        }
        if !(FINAL_FRAME_BYTES < length && length <= MAX_FRAME_BYTES) {
            return Err(CryptoError::InvalidStream);
        }
        read_exact_or_invalid(input, &mut frame[..length])?;
    }
}

pub(crate) fn is_frame_len(length: usize) -> bool {
    (FINAL_FRAME_BYTES..=MAX_FRAME_BYTES).contains(&length)
}

fn read_exact_or_invalid(input: &mut impl Read, buffer: &mut [u8]) -> Result<(), CryptoError> {
    input
        .read_exact(buffer)
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => CryptoError::InvalidStream,
            _ => error.into(),
        })
}

fn read_byte(input: &mut impl Read) -> Result<Option<u8>, CryptoError> {
    let mut byte = [0; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn require_eof(input: &mut impl Read) -> Result<(), CryptoError> {
    if read_byte(input)?.is_none() {
        Ok(())
    } else {
        Err(CryptoError::InvalidStream)
    }
}
