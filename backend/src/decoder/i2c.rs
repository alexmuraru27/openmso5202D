//! I²C decoder — SCL clock plus SDA data.
//!
//! START is SDA falling while SCL is high, STOP is SDA rising while SCL is high, and bits
//! are sampled on SCL rising edges, MSB first, 8 data bits plus an ACK. The first byte
//! after a START carries the address and R/W.
//!
//! The bus is self-framing, so a gapless stream needs none of the bit-grid machinery UART
//! requires — but a capture window that catches no START has no boundary to lock onto,
//! which is what the end-anchored fallback exists for.

use super::{Event, Kind};

/// How byte boundaries are established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    /// Forward only — bytes counted from each START.
    Start,
    /// Forward when a START was captured, else count backward from the transaction end so
    /// a capture that missed the START still recovers its bytes.
    #[default]
    Auto,
    /// Force the end-anchored fallback.
    End,
}

/// Decode an I²C bus.
pub fn decode(scl: &[bool], sda: &[bool], anchor: Anchor) -> Vec<Event> {
    let events = decode_forward(scl, sda);
    let has_start = events.iter().any(|e| e.kind == Kind::Start);
    match anchor {
        Anchor::Start => events,
        Anchor::Auto if has_start => events,
        _ => decode_end_anchored(scl, sda, events),
    }
}

/// Forward decode: bits counted from each START.
///
/// Handles repeated START, 7-bit and 10-bit addressing, and ACK/NACK.
fn decode_forward(scl: &[bool], sda: &[bool]) -> Vec<Event> {
    let n = scl.len().min(sda.len());
    let mut events = Vec::new();
    let mut bits: Vec<bool> = Vec::with_capacity(9);
    let mut byte_start: Option<usize> = None;
    let mut in_frame = false;
    let mut expect_address = false;
    let mut expect_address_low = false;

    for i in 1..n {
        // START and STOP are the two SDA transitions that occur while SCL is held high.
        if scl[i] && scl[i - 1] {
            if sda[i - 1] && !sda[i] {
                events.push(Event {
                    start: i,
                    end: i,
                    value: None,
                    ok: true,
                    kind: if in_frame { Kind::RepeatedStart } else { Kind::Start },
                });
                bits.clear();
                byte_start = None;
                in_frame = true;
                expect_address = true;
                expect_address_low = false;
                continue;
            }
            if !sda[i - 1] && sda[i] {
                events.push(Event {
                    start: i,
                    end: i,
                    value: None,
                    ok: true,
                    kind: Kind::Stop,
                });
                bits.clear();
                byte_start = None;
                in_frame = false;
                expect_address_low = false;
                continue;
            }
        }

        if in_frame && scl[i] && !scl[i - 1] {
            byte_start.get_or_insert(i);
            bits.push(sda[i]);
            if bits.len() == 9 {
                let value = bits[..8].iter().fold(0u8, |acc, &b| (acc << 1) | u8::from(b));
                let ack = !bits[8];
                let kind = if expect_address && (value & 0xF8) == 0xF0 {
                    // 10-bit address, first byte: 11110xx + R/W.
                    expect_address = false;
                    expect_address_low = true;
                    Kind::Address
                } else if expect_address {
                    expect_address = false;
                    Kind::Address
                } else if expect_address_low {
                    expect_address_low = false;
                    Kind::Address
                } else {
                    Kind::Byte
                };
                events.push(Event {
                    start: byte_start.unwrap_or(i),
                    end: i,
                    value: Some(value),
                    ok: ack,
                    kind,
                });
                bits.clear();
                byte_start = None;
            }
        }
    }
    events
}

/// Fallback for a capture with no START, triggered mid-transaction.
///
/// Byte boundaries cannot be counted from a START, so they are anchored to the transaction
/// end instead: SCL rising edges are grouped into 9-clock bytes counting backward from the
/// last STOP, dropping the leading partial. Bytes come out as plain data because the
/// address is off-screen. The direct analog of SPI end-anchoring.
fn decode_end_anchored(scl: &[bool], sda: &[bool], events: Vec<Event>) -> Vec<Event> {
    let n = scl.len().min(sda.len());
    let rises: Vec<usize> = (1..n).filter(|&i| scl[i] && !scl[i - 1]).collect();
    let falls: Vec<usize> = (1..n).filter(|&i| !scl[i] && scl[i - 1]).collect();

    let last_stop = events
        .iter()
        .filter(|e| e.kind == Kind::Stop)
        .map(|e| e.start)
        .next_back();

    let usable: Vec<usize> = match last_stop {
        Some(stop) => {
            // A STOP releases SDA with SCL held high, adding a rising edge that is not a
            // data or ACK bit. The last falling edge before the STOP ends the real final
            // clock, so only rises before that count.
            let cutoff = falls.iter().copied().rfind(|&f| f < stop).unwrap_or(stop);
            rises.into_iter().filter(|&r| r < cutoff).collect()
        }
        None => rises,
    };
    if usable.len() < 9 {
        return events;
    }
    let usable = &usable[usable.len() % 9..];

    let mut out: Vec<Event> = events.into_iter().filter(|e| e.kind == Kind::Stop).collect();
    for group in usable.chunks_exact(9) {
        let value = group[..8]
            .iter()
            .fold(0u8, |acc, &i| (acc << 1) | u8::from(sda[i]));
        out.push(Event {
            start: group[0],
            end: group[8],
            value: Some(value),
            ok: !sda[group[8]],
            kind: Kind::Byte,
        });
    }
    out.sort_by_key(|e| e.start);
    out
}
