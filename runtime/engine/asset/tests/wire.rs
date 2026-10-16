//! Frames cut out of a byte stream, however it arrives and whatever is damaged.

use marlin_asset::wire::{FrameReader, MAX_PAYLOAD, Reader, WireError, Writer, encode, frame};

#[test]
fn frames_arrive_in_any_pieces() {
    let mut stream = Vec::new();
    for i in 0..50u32 {
        encode(
            &mut stream,
            (i % 7) as u16,
            i,
            &vec![i as u8; (i * 37 % 300) as usize],
        );
    }
    for piece in [1, 3, 16, 17, 1000, stream.len()] {
        let mut reader = FrameReader::new();
        let mut frames = Vec::new();
        for chunk in stream.chunks(piece) {
            reader.push(chunk);
            while let Some(f) = reader.next_frame().unwrap() {
                frames.push(f);
            }
        }
        assert_eq!(frames.len(), 50, "pieces of {piece}");
        for (i, f) in frames.iter().enumerate() {
            let i = i as u32;
            assert_eq!((f.kind, f.id), ((i % 7) as u16, i));
            assert_eq!(f.payload, vec![i as u8; (i * 37 % 300) as usize]);
        }
        assert_eq!(reader.pending(), 0);
    }
}

#[test]
fn damage_is_an_error_never_a_panic() {
    let good = frame(3, 9, b"payload bytes");
    let mut bad = good.clone();
    bad[0] = b'X';
    assert_eq!(read_one(&bad), Err(WireError::BadMagic));
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(read_one(&bad), Err(WireError::Checksum));
    let mut bad = good.clone();
    bad[8..12].copy_from_slice(&(MAX_PAYLOAD + 1).to_le_bytes());
    assert_eq!(read_one(&bad), Err(WireError::TooLarge(MAX_PAYLOAD + 1)));

    let mut state = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..20_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let mut bad = good.clone();
        bad[(state % good.len() as u64) as usize] ^= (state >> 40) as u8 | 1;
        let _ = read_one(&bad);
    }
}

fn read_one(bytes: &[u8]) -> Result<Option<marlin_asset::wire::Frame>, WireError> {
    let mut r = FrameReader::new();
    r.push(bytes);
    r.next_frame()
}

#[test]
fn payload_reads_are_checked() {
    let mut w = Writer::new();
    w.u8(1).u32(2).u64(3).u128(4).str("five").bytes(&[6, 6]);
    let bytes = w.finish();
    let mut r = Reader::new(&bytes);
    assert_eq!(
        (r.u8(), r.u32(), r.u64(), r.u128()),
        (Ok(1), Ok(2), Ok(3), Ok(4))
    );
    assert_eq!(r.str().unwrap(), "five");
    assert_eq!(r.bytes().unwrap(), &[6, 6]);
    assert_eq!(r.end(), Ok(()));
    assert_eq!(r.u8(), Err(WireError::Malformed));
    // A length past the end.
    let mut w = Writer::new();
    w.u32(1000).raw(b"short");
    let bytes = w.finish();
    assert_eq!(Reader::new(&bytes).bytes(), Err(WireError::Malformed));
}
