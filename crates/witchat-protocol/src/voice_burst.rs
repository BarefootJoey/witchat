//! Live push-to-talk bursts (iOS `VoiceBurstPacket`): the inner payload of
//! `NoisePayloadType::VoiceFrame` in DMs and of `MessageType::VoiceFrame` on
//! the public mesh. The finalized note follows as a normal voice file named
//! `voice_<burst id hex>.m4a`.
//!
//! ```text
//! [burstID: 8][seq: u16 BE][flags: u8][payload…]
//! flags 0x01 START    payload = [codec: u8]
//! flags 0x02 END      payload = [totalDataPackets: u16 BE][durationMs: u32 BE]
//! flags 0x04 CANCELED payload empty
//! flags 0x00 data     payload = repeated [len: u16 BE][AAC-LC frame]
//! ```

pub const BURST_ID_LEN: usize = 8;
const HEADER_LEN: usize = BURST_ID_LEN + 3;
/// iOS rejects packets carrying more frames than this.
pub const MAX_FRAMES_PER_PACKET: usize = 8;
/// AAC-LC, 16 kHz, mono — the only codec iOS sends.
pub const CODEC_AAC_LC_16K_MONO: u8 = 0x01;

const START: u8 = 0x01;
const END: u8 = 0x02;
const CANCELED: u8 = 0x04;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BurstKind {
    Start { codec: u8 },
    Frames(Vec<Vec<u8>>),
    End { total_data_packets: u16, duration_ms: u32 },
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceBurst {
    pub burst_id: [u8; BURST_ID_LEN],
    pub seq: u16,
    pub kind: BurstKind,
}

impl VoiceBurst {
    pub fn burst_hex(&self) -> String {
        hex::encode(self.burst_id)
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < HEADER_LEN {
            return None;
        }
        let burst_id: [u8; BURST_ID_LEN] = data[..BURST_ID_LEN].try_into().ok()?;
        let seq = u16::from_be_bytes([data[BURST_ID_LEN], data[BURST_ID_LEN + 1]]);
        let payload = &data[HEADER_LEN..];
        let kind = match data[BURST_ID_LEN + 2] {
            START => {
                let codec = *payload.first()?;
                if codec != CODEC_AAC_LC_16K_MONO {
                    return None;
                }
                BurstKind::Start { codec }
            }
            END => {
                let b = payload.get(..6)?;
                BurstKind::End {
                    total_data_packets: u16::from_be_bytes([b[0], b[1]]),
                    duration_ms: u32::from_be_bytes([b[2], b[3], b[4], b[5]]),
                }
            }
            CANCELED => BurstKind::Canceled,
            0 => BurstKind::Frames(decode_frames(payload)?),
            _ => return None,
        };
        Some(Self { burst_id, seq, kind })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + 8);
        out.extend_from_slice(&self.burst_id);
        out.extend_from_slice(&self.seq.to_be_bytes());
        match &self.kind {
            BurstKind::Start { codec } => out.extend_from_slice(&[START, *codec]),
            BurstKind::Frames(frames) => {
                out.push(0);
                for frame in frames {
                    out.extend_from_slice(&(frame.len() as u16).to_be_bytes());
                    out.extend_from_slice(frame);
                }
            }
            BurstKind::End { total_data_packets, duration_ms } => {
                out.push(END);
                out.extend_from_slice(&total_data_packets.to_be_bytes());
                out.extend_from_slice(&duration_ms.to_be_bytes());
            }
            BurstKind::Canceled => out.push(CANCELED),
        }
        out
    }
}

fn decode_frames(mut payload: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut frames = Vec::new();
    while !payload.is_empty() {
        let len_bytes = payload.get(..2)?;
        let len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
        if len == 0 || frames.len() >= MAX_FRAMES_PER_PACKET {
            return None;
        }
        frames.push(payload.get(2..2 + len)?.to_vec());
        payload = &payload[2 + len..];
    }
    (!frames.is_empty()).then_some(frames)
}

/// The burst a finalized note belongs to, from its `voice_<16 hex>.m4a` name.
pub fn burst_of_note(file_name: &str) -> Option<String> {
    let hex = file_name.strip_prefix("voice_")?.strip_suffix(".m4a")?;
    (hex.len() == BURST_ID_LEN * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst(kind: BurstKind) -> VoiceBurst {
        VoiceBurst { burst_id: [0xAB; 8], seq: 3, kind }
    }

    #[test]
    fn every_kind_roundtrips() {
        for kind in [
            BurstKind::Start { codec: CODEC_AAC_LC_16K_MONO },
            BurstKind::Frames(vec![vec![1, 2, 3], vec![4]]),
            BurstKind::End { total_data_packets: 40, duration_ms: 2560 },
            BurstKind::Canceled,
        ] {
            let b = burst(kind);
            assert_eq!(VoiceBurst::decode(&b.encode()), Some(b));
        }
    }

    #[test]
    fn malformed_bursts_are_rejected() {
        let mut start = burst(BurstKind::Start { codec: 9 }).encode();
        assert!(VoiceBurst::decode(&start).is_none(), "unknown codec");
        start.truncate(5);
        assert!(VoiceBurst::decode(&start).is_none(), "short header");
        let mut frames = burst(BurstKind::Frames(vec![vec![1, 2, 3]])).encode();
        frames.pop();
        assert!(VoiceBurst::decode(&frames).is_none(), "frame runs past the end");
        let many = burst(BurstKind::Frames(vec![vec![1]; MAX_FRAMES_PER_PACKET + 1])).encode();
        assert!(VoiceBurst::decode(&many).is_none(), "too many frames");
    }

    #[test]
    fn notes_name_their_burst() {
        assert_eq!(burst_of_note("voice_ABABABABABABABAB.m4a").as_deref(), Some("abababababababab"));
        assert!(burst_of_note("voice_AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE.m4a").is_none());
        assert!(burst_of_note("img_1.jpg").is_none());
    }
}
