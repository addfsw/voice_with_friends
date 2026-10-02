//! Пробивка NAT для подключения через интернет без Tailscale и белого IP.
//! Подробности — в `rendezvous`.

pub mod invite;
pub mod mailbox;
pub mod rendezvous;
pub mod socket;
pub mod stun;

/// Случайные байты из того же источника, что у `HashMap` в std
/// (ключи от ОС), смешанного со временем.
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; N];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_usize(i);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        let bytes = hasher.finish().to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    out
}

/// HTTPS-клиенту почтового ящика нужен криптопровайдер rustls по умолчанию.
/// Ставим тот же ring, что используем для QUIC.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Новый случайный идентификатор комнаты для кода приглашения.
pub fn new_room_id() -> [u8; 16] {
    random_bytes()
}
