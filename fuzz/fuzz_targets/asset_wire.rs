//! `marlin_asset::wire`: a TCP byte stream cut into frames by `FrameReader`, arriving in pieces,
//! and each payload read with `Reader`.
//!
//! Input: one byte that sets the piece size the stream arrives in, then the stream.
#![no_main]

use marlin_asset::wire::{FrameReader, Reader};

fn run(data: &[u8]) {
    let Some(([piece], stream)) = marlin_fuzz::split::<1>(data) else {
        return;
    };
    let piece = 1 + piece as usize * 16;
    let mut frames = FrameReader::new();
    for bytes in stream.chunks(piece) {
        frames.push(bytes);
        loop {
            match frames.next_frame() {
                Ok(Some(frame)) => read_payload(frame.kind, &frame.payload),
                Ok(None) => break,
                // A bad frame ends the connection.
                Err(_) => return,
            }
        }
    }
    let _ = frames.pending();
}

/// Reads `payload` as a sequence of fields, their types chosen by the bits of `kind`.
fn read_payload(kind: u16, payload: &[u8]) {
    let mut r = Reader::new(payload);
    for i in 0..8 {
        let ok = match (kind >> (2 * i)) & 3 {
            0 => r.u32().is_ok(),
            1 => r.uuid().is_ok(),
            2 => r.str().is_ok(),
            _ => r.bytes().is_ok() && r.u64().is_ok(),
        };
        if !ok {
            break;
        }
    }
    let _ = r.remaining();
    let _ = r.end();
    let _ = r.rest();
}

libfuzzer_sys::fuzz_target!(init: marlin_fuzz::init(), |data: &[u8]| run(data));
