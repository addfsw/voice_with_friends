//! Пробивка NAT для подключения через интернет без Tailscale и белого IP.
//! Подробности — в `rendezvous`, открытие порта на роутере — в `upnp`.

pub mod invite;
pub mod mailbox;
pub mod rendezvous;
pub mod socket;
pub mod stun;
pub mod upnp;

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

/// HTTPS-клиент для почтового ящика и проверки обновлений программы.
pub fn http_client() -> anyhow::Result<reqwest::Client> {
    install_crypto_provider();
    let builder = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(10));
    // На Android reqwest по умолчанию проверяет сертификат через Java
    // (rustls-platform-verifier), а её без инициализации из JNI нет: TLS-
    // рукопожатие с ntfy.sh паниковало, и вход по коду приглашения с
    // телефона зависал. Проверяем по встроенному списку корневых сертификатов.
    #[cfg(target_os = "android")]
    let builder = builder.tls_certs_only(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|der| reqwest::Certificate::from_der(der).ok()),
    );
    anyhow::Context::context(builder.build(), "не удалось создать HTTP-клиент")
}

/// Новый случайный идентификатор комнаты для кода приглашения.
pub fn new_room_id() -> [u8; 16] {
    random_bytes()
}
