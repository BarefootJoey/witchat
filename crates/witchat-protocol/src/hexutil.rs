pub fn encode(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

pub fn decode(text: &str) -> Result<Vec<u8>, hex::FromHexError> {
    hex::decode(text.trim())
}

pub fn encode_fixed(bytes: &[u8]) -> String {
    hex::encode(bytes)
}
