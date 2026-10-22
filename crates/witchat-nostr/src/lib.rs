//! Internet side of BitChat: geohash location channels and private
//! messages to favorites, both over Nostr relays.

pub mod embedded;
pub mod envelope;
pub mod geo;
pub mod geo_level;
pub mod geo_probe;
pub mod geo_sample;
mod https_get;
mod kdf;
pub mod mailbox;
mod quick_join_table;
pub mod relay_directory;
pub mod relay_health;
pub mod relay_url;
pub mod transport;

pub use geo::{
    derive_geo_keys, derive_geo_seed, ios_relays_for, is_valid_geohash, relays_for, GeoChannel, GeoMessage, Published,
};
pub use geo_level::{cells_for, coverage, is_high_precision, quick_join_cell, GeoLevel};
pub use geo_sample::GeoSampler;
pub use mailbox::{derive_keys, npub_of, parse_public_key, InboundDm, Mailbox, DM_RELAYS};
pub use nostr::key::Keys;
pub use relay_url::{add_custom_relay, normalize_relay, RelayError, MAX_CUSTOM_RELAYS};
pub use transport::{socks_ready, tor_circuit_ready, TorHealth, Transport, TOR_BROWSER_PROXY};

#[derive(Debug, thiserror::Error)]
pub enum NostrError {
    #[error("nostr: {0}")]
    Client(#[from] nostr_sdk::error::Error),
    #[error("nostr event: {0}")]
    Event(#[from] nostr::error::Error),
    #[error("{0}")]
    Other(String),
}
