//! The serial deframer against frames made by the reference client's encoder.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ch32rv_oep::codec::{SerialDeframer, cobs_decode};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn decodes_reference_frames() {
    let text = include_str!("cobs_vectors.txt");
    let mut n = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut it = line.split(' ');
        let msg = hex(it.next().unwrap());
        let frame = hex(it.next().unwrap());
        // COBS itself round-trips message + CRC.
        let body = cobs_decode(&frame[..frame.len() - 1]).expect("cobs");
        assert_eq!(&body[..msg.len()], &msg[..]);
        if msg.is_empty() {
            continue; // an empty message is not an OEP message
        }
        // A fresh deframer takes the frame as sent (trailing delimiter only)...
        let mut d = SerialDeframer::new(1024);
        assert_eq!(d.push(&frame), vec![msg.clone()], "{} bytes", msg.len());
        // ...and after console noise with a delimiter in front.
        let mut s = b"noise\r\n".to_vec();
        s.push(0);
        s.extend_from_slice(&frame);
        assert_eq!(SerialDeframer::new(1024).push(&s), vec![msg]);
        n += 1;
    }
    assert!(n >= 20);
}
