//! UDP-сокет для QUIC, который заодно умеет STUN и «пробивные» пакеты.
//!
//! Пробивка NAT работает, только если всё идёт через **один и тот же** UDP-порт:
//! внешний адрес, который сообщил STUN, — это отображение именно этого порта
//! на роутере. Поэтому сокет у quinn и у STUN общий. Обёртка пропускает все
//! пакеты в quinn, кроме ответов STUN на наши же запросы: их забирает себе.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use anyhow::Context as _;
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, Runtime, TokioRuntime, UdpPoller};
use tokio::sync::oneshot;

use crate::stun::{self, TransactionId};

pub struct NatSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    /// Ждущие ответа STUN-запросы.
    pending: Mutex<HashMap<TransactionId, oneshot::Sender<SocketAddr>>>,
}

impl fmt::Debug for NatSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NatSocket").field("inner", &self.inner).finish()
    }
}

impl NatSocket {
    /// Открывает UDP-сокет. Нужно вызывать внутри среды tokio.
    pub fn bind(addr: SocketAddr) -> io::Result<Arc<Self>> {
        let socket = std::net::UdpSocket::bind(addr)?;
        let inner = TokioRuntime.wrap_udp_socket(socket)?;
        Ok(Arc::new(Self { inner, pending: Mutex::new(HashMap::new()) }))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// QUIC-endpoint поверх этого сокета (`server_config` — для сервера).
    pub fn endpoint(
        self: &Arc<Self>,
        server_config: Option<quinn::ServerConfig>,
    ) -> io::Result<quinn::Endpoint> {
        quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            server_config,
            self.clone(),
            Arc::new(TokioRuntime),
        )
    }

    /// Отправляет датаграмму мимо QUIC (запрос STUN или «пробивной» пакет).
    /// Если сокет занят, пакет просто теряется — все такие отправки повторяются.
    pub fn send_raw(&self, to: SocketAddr, data: &[u8]) {
        let transmit = Transmit {
            destination: to,
            ecn: None,
            contents: data,
            segment_size: None,
            src_ip: None,
        };
        let _ = self.inner.try_send(&transmit);
    }

    /// Спрашивает у STUN-сервера наш внешний адрес.
    pub async fn stun(&self, server: SocketAddr, wait: Duration) -> anyhow::Result<SocketAddr> {
        let id: TransactionId = crate::random_bytes();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let request = stun::binding_request(&id);

        // UDP может потерять пакет — повторяем запрос несколько раз.
        let attempts = 3;
        let mut rx = rx;
        let mut result = None;
        for _ in 0..attempts {
            self.send_raw(server, &request);
            match tokio::time::timeout(wait / attempts, &mut rx).await {
                Ok(Ok(addr)) => {
                    result = Some(addr);
                    break;
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        self.pending.lock().unwrap().remove(&id);
        result.with_context(|| format!("STUN-сервер {server} не ответил"))
    }

    /// Ответы на наши STUN-запросы забираем; true — пакет «наш», quinn его не увидит.
    fn take_stun(&self, data: &[u8]) -> bool {
        let Some((id, addr)) = stun::parse_binding_response(data) else { return false };
        match self.pending.lock().unwrap().remove(&id) {
            Some(waiter) => {
                let _ = waiter.send(addr);
                true
            }
            // Ответ на уже забытый запрос (пришёл после таймаута) — тоже не для QUIC.
            None => true,
        }
    }
}

impl AsyncUdpSocket for NatSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        self.inner.try_send(transmit)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let count = ready!(self.inner.poll_recv(cx, bufs, meta))?;
        for (buf, meta) in bufs.iter().zip(meta.iter_mut()).take(count) {
            let stride = meta.stride.max(1);
            let first = &buf[..meta.len.min(stride)];
            if self.take_stun(first) {
                // Остальные сегменты из той же пачки — тоже ответы STUN.
                for segment in buf[..meta.len].chunks(stride).skip(1) {
                    self.take_stun(segment);
                }
                // Пустую датаграмму quinn просто пропустит.
                meta.len = 0;
            }
        }
        Poll::Ready(Ok(count))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}
