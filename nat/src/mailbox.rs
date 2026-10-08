//! «Почтовый ящик» для обмена адресами перед пробивкой NAT.
//!
//! Используется бесплатный сервис ntfy.sh (без регистрации): сообщение,
//! отправленное в «тему», получают все, кто на неё подписан. Тема — длинная
//! случайная строка из кода приглашения, угадать её нельзя. Через ящик идут
//! только адреса (несколько сотен байт), голос — напрямую между участниками.
//!
//! Адрес сервиса можно заменить переменной `VWF_SIGNAL_URL` (например, на свой
//! ntfy-сервер или на испытательный стенд).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tracing::debug;

pub const DEFAULT_URL: &str = "https://ntfy.sh";

/// ntfy.sh присылает keep-alive раз в 45 секунд; если дольше тишина —
/// соединение умерло, переподключаемся.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxEvent {
    /// Подписка установлена: с этого момента сообщения не потеряются.
    Open,
    Message(String),
}

#[derive(Clone)]
pub enum Mailbox {
    Ntfy {
        client: reqwest::Client,
        base: String,
    },
    /// Ящик в памяти процесса — для тестов.
    Memory(Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>),
}

impl Mailbox {
    /// ntfy.sh или сервис из `VWF_SIGNAL_URL`.
    pub fn from_env() -> anyhow::Result<Self> {
        let base = std::env::var("VWF_SIGNAL_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_URL.to_string());
        crate::install_crypto_provider();
        let builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(10));
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
        let client = builder.build().context("не удалось создать HTTP-клиент")?;
        Ok(Mailbox::Ntfy { client, base: base.trim_end_matches('/').to_string() })
    }

    pub fn memory() -> Self {
        Mailbox::Memory(Arc::default())
    }

    pub async fn publish(&self, topic: &str, text: &str) -> anyhow::Result<()> {
        match self {
            Mailbox::Ntfy { client, base } => {
                let response = client
                    .post(format!("{base}/{topic}"))
                    .body(text.to_string())
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await
                    .with_context(|| format!("сервис обмена адресами ({base}) недоступен"))?;
                if !response.status().is_success() {
                    bail!("сервис обмена адресами ответил ошибкой {}", response.status());
                }
                Ok(())
            }
            Mailbox::Memory(topics) => {
                let _ = memory_topic(topics, topic).send(text.to_string());
                Ok(())
            }
        }
    }

    /// Подписывается на тему. Первым придёт `Open`, затем новые сообщения.
    /// Обрыв соединения переживает сам: переподключается и не теряет сообщений.
    pub fn subscribe(&self, topic: &str) -> mpsc::UnboundedReceiver<MailboxEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        match self {
            Mailbox::Ntfy { client, base } => {
                let url = format!("{base}/{topic}/json");
                tokio::spawn(ntfy_subscription(client.clone(), url, tx));
            }
            Mailbox::Memory(topics) => {
                let mut sub = memory_topic(topics, topic).subscribe();
                let _ = tx.send(MailboxEvent::Open);
                tokio::spawn(async move {
                    while let Ok(text) = sub.recv().await {
                        if tx.send(MailboxEvent::Message(text)).is_err() {
                            break;
                        }
                    }
                });
            }
        }
        rx
    }
}

fn memory_topic(
    topics: &Mutex<HashMap<String, broadcast::Sender<String>>>,
    topic: &str,
) -> broadcast::Sender<String> {
    topics
        .lock()
        .unwrap()
        .entry(topic.to_string())
        .or_insert_with(|| broadcast::channel(64).0)
        .clone()
}

/// Строка потока ntfy: `{"id":"…","event":"message","message":"…"}`.
#[derive(Deserialize)]
struct NtfyLine {
    id: Option<String>,
    event: String,
    message: Option<String>,
}

async fn ntfy_subscription(
    client: reqwest::Client,
    url: String,
    tx: mpsc::UnboundedSender<MailboxEvent>,
) {
    // После переподключения просим сообщения, пришедшие после последнего
    // полученного, чтобы ничего не потерять.
    let mut last_id: Option<String> = None;
    let mut backoff = Duration::from_secs(1);
    while !tx.is_closed() {
        // Идентификаторы сообщений ntfy — буквы и цифры, экранировать нечего.
        let request = match &last_id {
            Some(id) => client.get(format!("{url}?since={id}")),
            None => client.get(&url),
        };
        let mut response = match request.send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                debug!("почтовый ящик: {}", r.status());
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
            Err(e) => {
                debug!("почтовый ящик: {e}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        backoff = Duration::from_secs(1);

        let mut pending = Vec::new();
        loop {
            let chunk = tokio::select! {
                // Подписка больше не нужна — закрываем соединение сразу, а не
                // когда в тему придёт следующее сообщение.
                () = tx.closed() => return,
                chunk = tokio::time::timeout(SILENCE_TIMEOUT, response.chunk()) => chunk,
            };
            let chunk = match chunk {
                Ok(Ok(Some(chunk))) => chunk,
                _ => break, // конец потока, ошибка или тишина — переподключаемся
            };
            pending.extend_from_slice(&chunk);
            while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                let Ok(line) = serde_json::from_slice::<NtfyLine>(&line) else { continue };
                let event = match line.event.as_str() {
                    "open" => MailboxEvent::Open,
                    "message" => {
                        if line.id.is_some() {
                            last_id.clone_from(&line.id);
                        }
                        MailboxEvent::Message(line.message.unwrap_or_default())
                    }
                    _ => continue, // keepalive и прочее
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
        }
    }
}
