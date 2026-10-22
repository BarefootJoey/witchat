//! Reassemble BLE GATT notifications into complete BitChat frames.
//!
//! Pinned to `NotificationStreamAssembler.swift`. Windows (and many
//! phones) deliver a 256-byte padded packet as several ATT notifications;
//! `Packet::decode` on a partial chunk silently drops the message.

use crate::constants::{
    FLAGS_OFFSET, FLAG_HAS_RECIPIENT, FLAG_HAS_ROUTE, FLAG_HAS_SIGNATURE, FLAG_IS_COMPRESSED, RECIPIENT_ID_SIZE,
    SENDER_ID_SIZE, SIGNATURE_SIZE, V1_HEADER_SIZE, V2_HEADER_SIZE,
};
use std::time::{Duration, Instant};

const HARD_CAP: usize = 8 * 1024 * 1024;
const STALL_RESET: Duration = Duration::from_millis(500);
const LENGTH_OFFSET: usize = 12;

#[derive(Debug, Default)]
pub struct NotificationStreamAssembler {
    buffer: Vec<u8>,
    pending_started: Option<Instant>,
    pending_expected: usize,
}

#[derive(Debug, Default)]
pub struct AssembleResult {
    pub frames: Vec<Vec<u8>>,
    pub dropped_prefixes: Vec<u8>,
    pub reset: bool,
}

impl NotificationStreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    fn reset_state(&mut self) {
        self.buffer.clear();
        self.pending_started = None;
        self.pending_expected = 0;
    }

    fn discard_leading_padding(&mut self) -> bool {
        let first = match self.buffer.first().copied() {
            Some(b) => b,
            None => return false,
        };
        if first == 1 || first == 2 {
            return false;
        }
        let padding_length = first as usize;
        if padding_length == 0 || padding_length > self.buffer.len() {
            return false;
        }
        if !self.buffer[..padding_length].iter().all(|b| *b == first) {
            return false;
        }
        self.buffer.drain(..padding_length);
        self.pending_started = None;
        self.pending_expected = 0;
        true
    }

    pub fn append(&mut self, chunk: &[u8]) -> AssembleResult {
        if chunk.is_empty() {
            return AssembleResult::default();
        }
        self.buffer.extend_from_slice(chunk);

        let mut frames = Vec::new();
        let mut dropped = Vec::new();
        let mut did_reset = false;
        let min_prefix = V1_HEADER_SIZE + SENDER_ID_SIZE;

        if self.buffer.len() > HARD_CAP {
            self.reset_state();
            return AssembleResult { reset: true, ..AssembleResult::default() };
        }

        while self.buffer.len() >= min_prefix {
            let version = self.buffer[0];
            if version != 1 && version != 2 {
                if self.discard_leading_padding() {
                    continue;
                }
                dropped.push(self.buffer.remove(0));
                self.pending_started = None;
                self.pending_expected = 0;
                continue;
            }
            let header_size = match version {
                1 => V1_HEADER_SIZE,
                2 => V2_HEADER_SIZE,
                _ => {
                    dropped.push(self.buffer.remove(0));
                    self.pending_started = None;
                    self.pending_expected = 0;
                    continue;
                }
            };
            let frame_prefix = header_size + SENDER_ID_SIZE;
            if self.buffer.len() < frame_prefix {
                break;
            }
            if FLAGS_OFFSET >= self.buffer.len() {
                break;
            }
            let flags = self.buffer[FLAGS_OFFSET];
            let has_recipient = flags & FLAG_HAS_RECIPIENT != 0;
            let has_signature = flags & FLAG_HAS_SIGNATURE != 0;
            let is_compressed = flags & FLAG_IS_COMPRESSED != 0;
            let has_route = version >= 2 && flags & FLAG_HAS_ROUTE != 0;

            let payload_length = if version == 2 {
                if self.buffer.len() < LENGTH_OFFSET + 4 {
                    break;
                }
                u32::from_be_bytes([
                    self.buffer[LENGTH_OFFSET],
                    self.buffer[LENGTH_OFFSET + 1],
                    self.buffer[LENGTH_OFFSET + 2],
                    self.buffer[LENGTH_OFFSET + 3],
                ]) as usize
            } else {
                if self.buffer.len() < LENGTH_OFFSET + 2 {
                    break;
                }
                u16::from_be_bytes([self.buffer[LENGTH_OFFSET], self.buffer[LENGTH_OFFSET + 1]]) as usize
            };

            let mut frame_length = frame_prefix + payload_length;
            if has_recipient {
                frame_length += RECIPIENT_ID_SIZE;
            }
            if has_signature {
                frame_length += SIGNATURE_SIZE;
            }
            if has_route {
                let route_count_offset = frame_prefix + if has_recipient { RECIPIENT_ID_SIZE } else { 0 };
                if self.buffer.len() <= route_count_offset {
                    break;
                }
                let route_count = self.buffer[route_count_offset] as usize;
                frame_length += 1 + route_count * SENDER_ID_SIZE;
            }

            if is_compressed {
                let raw_len_bytes = if version == 2 { 4 } else { 2 };
                if payload_length < raw_len_bytes {
                    self.reset_state();
                    did_reset = true;
                    break;
                }
            }

            if frame_length == 0 || frame_length > HARD_CAP {
                self.reset_state();
                did_reset = true;
                break;
            }

            if self.buffer.len() < frame_length {
                if self.pending_started.is_none() || self.pending_expected != frame_length {
                    self.pending_started = Some(Instant::now());
                    self.pending_expected = frame_length;
                } else if let Some(started) = self.pending_started {
                    if started.elapsed() >= STALL_RESET {
                        self.reset_state();
                        did_reset = true;
                    }
                }
                break;
            }

            self.pending_started = None;
            self.pending_expected = 0;
            frames.push(self.buffer[..frame_length].to_vec());
            self.buffer.drain(..frame_length);
            let _ = self.discard_leading_padding();
        }

        if self.discard_leading_padding() {
            return AssembleResult { frames, dropped_prefixes: dropped, reset: did_reset };
        }

        if !self.buffer.is_empty() && self.buffer.iter().all(|b| *b == 0) {
            self.reset_state();
        }

        AssembleResult { frames, dropped_prefixes: dropped, reset: did_reset }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_type::MessageType;
    use crate::packet::Packet;

    fn sample() -> Vec<u8> {
        let pkt = Packet::new(
            MessageType::Message as u8,
            [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
            vec![0xDE, 0xAD, 0xBE, 0xEF],
        );
        pkt.encode(false).expect("encode")
    }

    #[test]
    fn assembles_single_frame_across_20_byte_chunks() {
        let frame = sample();
        let mut asm = NotificationStreamAssembler::new();
        let split = 20.min(frame.len() / 2).max(1);
        let first = asm.append(&frame[..split]);
        assert!(first.frames.is_empty());
        let second = asm.append(&frame[split..]);
        assert_eq!(second.frames.len(), 1);
        assert_eq!(second.frames[0], frame);
        assert!(Packet::decode(&second.frames[0]).is_some());
    }

    #[test]
    fn whole_frame_in_one_chunk() {
        let frame = sample();
        let mut asm = NotificationStreamAssembler::new();
        let result = asm.append(&frame);
        assert_eq!(result.frames.len(), 1);
        assert_eq!(result.frames[0], frame);
    }

    #[test]
    fn discards_pkcs7_padding_between_frames() {
        let a = Packet::new(MessageType::Message as u8, [1; 8], vec![1, 2, 3]);
        let b = Packet::new(MessageType::Message as u8, [2; 8], vec![4, 5, 6]);
        let padded_a = a.encode(true).unwrap();
        let padded_b = b.encode(true).unwrap();
        let mut asm = NotificationStreamAssembler::new();
        assert_eq!(asm.append(&padded_a).frames.len(), 1);
        let second = asm.append(&padded_b);
        assert_eq!(second.frames.len(), 1);
        let decoded = Packet::decode(&second.frames[0]).unwrap();
        assert_eq!(decoded.payload, vec![4, 5, 6]);
    }

    #[test]
    fn drops_invalid_prefix_byte() {
        let frame = sample();
        let mut noisy = vec![0x00];
        noisy.extend_from_slice(&frame);
        let mut asm = NotificationStreamAssembler::new();
        let result = asm.append(&noisy);
        assert_eq!(result.dropped_prefixes, vec![0x00]);
        assert_eq!(result.frames.len(), 1);
    }
}
