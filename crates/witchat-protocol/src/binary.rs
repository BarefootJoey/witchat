//! Binary frame codec matching `BinaryProtocol.swift`.

use crate::compression;
use crate::constants::*;
use crate::packet::{pad_id, Packet};
use crate::padding;
use crate::payload_limits;

struct Reader<'a> {
    buf: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.offset)
    }

    fn read_u8(&mut self) -> Option<u8> {
        if self.remaining() < 1 {
            return None;
        }
        let v = self.buf[self.offset];
        self.offset += 1;
        Some(v)
    }

    fn read_u16(&mut self) -> Option<u16> {
        if self.remaining() < 2 {
            return None;
        }
        let v = u16::from_be_bytes([self.buf[self.offset], self.buf[self.offset + 1]]);
        self.offset += 2;
        Some(v)
    }

    fn read_u32(&mut self) -> Option<u32> {
        if self.remaining() < 4 {
            return None;
        }
        let v = u32::from_be_bytes([
            self.buf[self.offset],
            self.buf[self.offset + 1],
            self.buf[self.offset + 2],
            self.buf[self.offset + 3],
        ]);
        self.offset += 4;
        Some(v)
    }

    fn read_exact(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.remaining() < n {
            return None;
        }
        let slice = &self.buf[self.offset..self.offset + n];
        self.offset += n;
        Some(slice)
    }
}

fn header_size(version: u8) -> Option<usize> {
    match version {
        1 => Some(V1_HEADER_SIZE),
        2 => Some(V2_HEADER_SIZE),
        _ => None,
    }
}

fn length_field_size(version: u8) -> usize {
    if version == 2 {
        4
    } else {
        2
    }
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub fn encode(packet: &Packet, do_padding: bool) -> Option<Vec<u8>> {
    let version = packet.version;
    if version != 1 && version != 2 {
        return None;
    }

    let mut payload = packet.payload.clone();
    let mut is_compressed = false;
    let mut original_payload_size = None;
    if compression::should_compress(&payload) {
        let max_representable = if version == 2 { u32::MAX as usize } else { u16::MAX as usize };
        if payload.len() <= max_representable {
            if let Some(compressed) = compression::compress(&payload) {
                original_payload_size = Some(payload.len());
                payload = compressed;
                is_compressed = true;
            }
        }
    }

    let length_field_bytes = length_field_size(version);
    let original_route = if version >= 2 { packet.route.clone().unwrap_or_default() } else { Vec::new() };
    if original_route.len() > 255 {
        return None;
    }

    let has_route = !original_route.is_empty();
    let original_size_field_bytes = if is_compressed { length_field_bytes } else { 0 };
    let payload_data_size = payload.len() + original_size_field_bytes;
    if version == 1 && payload_data_size > u16::MAX as usize {
        return None;
    }
    if version == 2 && payload_data_size > u32::MAX as usize {
        return None;
    }

    let mut data = Vec::new();
    data.push(version);
    data.push(packet.type_id);
    data.push(packet.ttl);
    push_u64(&mut data, packet.timestamp);

    let mut flags = 0u8;
    if packet.recipient_id.is_some() {
        flags |= FLAG_HAS_RECIPIENT;
    }
    if packet.signature.is_some() {
        flags |= FLAG_HAS_SIGNATURE;
    }
    if is_compressed {
        flags |= FLAG_IS_COMPRESSED;
    }
    if has_route && version >= 2 {
        flags |= FLAG_HAS_ROUTE;
    }
    if packet.is_rsr {
        flags |= FLAG_IS_RSR;
    }
    data.push(flags);

    if version == 2 {
        push_u32(&mut data, payload_data_size as u32);
    } else {
        push_u16(&mut data, payload_data_size as u16);
    }

    data.extend_from_slice(&packet.sender_id);
    if let Some(recipient) = packet.recipient_id {
        data.extend_from_slice(&recipient);
    }

    if has_route {
        data.push(original_route.len() as u8);
        for hop in &original_route {
            data.extend_from_slice(hop);
        }
    }

    if is_compressed {
        if let Some(original) = original_payload_size {
            if version == 2 {
                push_u32(&mut data, original as u32);
            } else {
                push_u16(&mut data, original as u16);
            }
        }
    }
    data.extend_from_slice(&payload);

    if let Some(signature) = packet.signature {
        data.extend_from_slice(&signature);
    }

    if do_padding {
        let optimal = padding::optimal_block_size(data.len());
        Some(padding::pad(&data, optimal))
    } else {
        Some(data)
    }
}

pub fn decode(data: &[u8]) -> Option<Packet> {
    if let Some(pkt) = decode_core(data) {
        return Some(pkt);
    }
    let unpadded = padding::unpad(data);
    if unpadded.as_slice() == data {
        return None;
    }
    decode_core(&unpadded)
}

fn decode_core(raw: &[u8]) -> Option<Packet> {
    if raw.len() < V1_HEADER_SIZE + SENDER_ID_SIZE {
        return None;
    }
    let mut r = Reader::new(raw);
    let version = r.read_u8()?;
    if version != 1 && version != 2 {
        return None;
    }
    let hdr = header_size(version)?;
    if raw.len() < hdr + SENDER_ID_SIZE {
        return None;
    }
    let type_id = r.read_u8()?;
    let ttl = r.read_u8()?;
    let mut timestamp = 0u64;
    for _ in 0..8 {
        timestamp = (timestamp << 8) | u64::from(r.read_u8()?);
    }
    let flags = r.read_u8()?;
    let has_recipient = flags & FLAG_HAS_RECIPIENT != 0;
    let has_signature = flags & FLAG_HAS_SIGNATURE != 0;
    let is_compressed = flags & FLAG_IS_COMPRESSED != 0;
    let has_route = version >= 2 && flags & FLAG_HAS_ROUTE != 0;
    let is_rsr = flags & FLAG_IS_RSR != 0;

    let payload_length = if version == 2 { r.read_u32()? as usize } else { r.read_u16()? as usize };
    if payload_length > payload_limits::MAX_FRAMED_FILE_BYTES {
        return None;
    }

    let sender = r.read_exact(SENDER_ID_SIZE)?;
    let mut sender_id = [0u8; SENDER_ID_SIZE];
    sender_id.copy_from_slice(sender);

    let recipient_id = if has_recipient {
        let rec = r.read_exact(RECIPIENT_ID_SIZE)?;
        let mut id = [0u8; RECIPIENT_ID_SIZE];
        id.copy_from_slice(rec);
        Some(id)
    } else {
        None
    };

    let route = if has_route {
        let count = r.read_u8()? as usize;
        if count == 0 {
            None
        } else {
            let mut hops = Vec::with_capacity(count);
            for _ in 0..count {
                let hop = r.read_exact(SENDER_ID_SIZE)?;
                let mut id = [0u8; SENDER_ID_SIZE];
                id.copy_from_slice(hop);
                hops.push(id);
            }
            Some(hops)
        }
    } else {
        None
    };

    let max_payload = payload_limits::max_payload_bytes(type_id);
    let payload = if is_compressed {
        let length_field_bytes = length_field_size(version);
        if payload_length < length_field_bytes {
            return None;
        }
        let original_size = if version == 2 { r.read_u32()? as usize } else { r.read_u16()? as usize };
        if original_size > max_payload {
            return None;
        }
        let compressed_size = payload_length - length_field_bytes;
        if compressed_size == 0 {
            return None;
        }
        let compressed = r.read_exact(compressed_size)?;
        compression::decompress(compressed, original_size)?
    } else {
        if payload_length > max_payload {
            return None;
        }
        r.read_exact(payload_length)?.to_vec()
    };

    let signature = if has_signature {
        let sig = r.read_exact(SIGNATURE_SIZE)?;
        let mut out = [0u8; SIGNATURE_SIZE];
        out.copy_from_slice(sig);
        Some(out)
    } else {
        None
    };

    Some(Packet { version, type_id, sender_id, recipient_id, timestamp, payload, signature, ttl, route, is_rsr })
}

pub fn fixed_id(bytes: &[u8]) -> [u8; SENDER_ID_SIZE] {
    let padded = pad_id(bytes, SENDER_ID_SIZE);
    let mut id = [0u8; SENDER_ID_SIZE];
    id.copy_from_slice(&padded);
    id
}
