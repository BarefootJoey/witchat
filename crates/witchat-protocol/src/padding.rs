//! PKCS#7-style padding from `MessagePadding.swift`.

pub const BLOCK_SIZES: [usize; 4] = [256, 512, 1024, 2048];

pub fn pad(data: &[u8], target_size: usize) -> Vec<u8> {
    if data.len() >= target_size {
        return data.to_vec();
    }
    let needed = target_size - data.len();
    if needed == 0 || needed > 255 {
        return data.to_vec();
    }
    let mut out = data.to_vec();
    out.extend(std::iter::repeat_n(needed as u8, needed));
    out
}

pub fn unpad(data: &[u8]) -> Vec<u8> {
    let Some(&last) = data.last() else {
        return Vec::new();
    };
    let padding_length = last as usize;
    if padding_length == 0 || padding_length > data.len() {
        return data.to_vec();
    }
    let start = data.len() - padding_length;
    if data[start..].iter().any(|b| *b != last) {
        return data.to_vec();
    }
    data[..start].to_vec()
}

pub fn optimal_block_size(data_size: usize) -> usize {
    let total = data_size + 16;
    for block in BLOCK_SIZES {
        if total <= block {
            return block;
        }
    }
    data_size
}
