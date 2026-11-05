//! The pool connection under the Stratum session: plain TCP or TLS.
//!
//! The session reads on its own thread and a writer thread sends shares,
//! at the same time. A socket is full duplex, so plain TCP just shares the
//! stream. An mbedTLS context is not safe to use from two threads at once
//! (no MBEDTLS_THREADING_C in this build), so TLS runs on a non-blocking
//! socket behind a mutex: the reader waits for data with `poll` outside the
//! lock and holds it only for one record's read; the writer holds it for
//! one write.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use esp_idf_svc::sys;
use esp_idf_svc::tls::{Config, EspTls, InternalSocket};
use warpaxe_trust::pin::Pin;

use crate::tls_trust::{self, Seen};

/// No data from the pool for this long ends the session (as the plain
/// socket's read timeout).
const READ_TIMEOUT: Duration = Duration::from_secs(180);
/// A write that cannot go out for this long counts as a broken link.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub enum Link {
    Tcp(TcpStream),
    Tls { tls: Mutex<SendTls>, fd: i32 },
}

/// `EspTls` is only `Send` with MBEDTLS_THREADING_C; every use here goes
/// through the mutex, one thread at a time.
pub struct SendTls(EspTls<InternalSocket>);
unsafe impl Send for SendTls {}

/// The pool's certificate was refused for its key alone: no public
/// authority issued it and the key is not the confirmed one (or there is
/// none). The stratum thread keeps `seen` for the Pool page.
#[derive(Debug, Clone, Copy)]
pub struct KeyRefused(pub Seen);

impl std::fmt::Display for KeyRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.changed {
            write!(
                f,
                "the pool's key changed: {} is not the trusted key (confirm it on the Pool page if the change is yours)",
                self.0.pin.text().short()
            )
        } else {
            write!(
                f,
                "pool certificate not verifiable against a public authority (self-signed, or the chain incomplete): key {} (trust it on the Pool page)",
                self.0.pin.text().short()
            )
        }
    }
}

impl std::error::Error for KeyRefused {}

fn esp_io(e: sys::EspError) -> io::Error {
    io::Error::other(e.to_string())
}

fn is_want(e: &sys::EspError) -> bool {
    let c = e.code();
    c == sys::ESP_TLS_ERR_SSL_WANT_READ || c == sys::ESP_TLS_ERR_SSL_WANT_WRITE
}

impl Link {
    /// Connect to `host:port`; with `tls`, verify the pool's certificate
    /// against the built-in CA bundle and its name against `host`, or,
    /// when that fails, its public key against `pin` (the owner's
    /// confirmed key for this pool; `tls_trust`). A refusal for the key
    /// alone is a [`KeyRefused`] error. After a TLS connect,
    /// `tls_trust::pool_outcome` says whether the pin did the accepting.
    pub fn connect(host: &str, port: u16, tls: bool, pin: Option<Pin>) -> anyhow::Result<Link> {
        if !tls {
            let addr = (host, port)
                .to_socket_addrs()?
                .next()
                .ok_or_else(|| anyhow::anyhow!("dns: no address for {host}"))?;
            log::info!("stratum: connecting to {host}:{port} ({addr})");
            let s = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
            s.set_read_timeout(Some(READ_TIMEOUT))?;
            if let Err(e) = s.set_nodelay(true) {
                log::warn!("stratum: nodelay not set: {e}");
            }
            return Ok(Link::Tcp(s));
        }
        log::info!(
            "stratum: connecting to {host}:{port} (TLS{})",
            if pin.is_some() { ", pinned key" } else { "" }
        );
        let mut t = EspTls::new()?;
        let mut cfg = Config::new();
        // The name goes out as SNI and is checked unless the pin accepts
        // the certificate (then an IP address works as the pool address).
        cfg.common_name = Some(host);
        cfg.crt_bundle_attach = Some(tls_trust::attach_pool);
        cfg.timeout_ms = 10_000;
        tls_trust::pool_begin(pin);
        let hs = t.connect(host, port, &cfg).map_err(|e| {
            match tls_trust::pool_outcome().0 {
                Some(seen) => anyhow::Error::new(KeyRefused(seen)),
                None => anyhow::anyhow!("tls handshake with {host}:{port}: {e}"),
            }
        })?;
        drop(hs);
        let mut fd: i32 = -1;
        sys::esp!(unsafe { sys::esp_tls_get_conn_sockfd(t.context_handle(), &mut fd) })?;
        unsafe {
            let fl = sys::fcntl(fd, sys::F_GETFL as i32);
            sys::fcntl(fd, sys::F_SETFL as i32, fl | sys::O_NONBLOCK as i32);
            let one: i32 = 1;
            sys::lwip_setsockopt(
                fd,
                sys::IPPROTO_TCP as i32,
                sys::TCP_NODELAY as i32,
                &one as *const i32 as *const core::ffi::c_void,
                core::mem::size_of::<i32>() as u32,
            );
        }
        if tls_trust::pool_outcome().1 {
            log::info!("stratum: TLS up, certificate accepted by the pinned key for {host}");
        } else {
            log::info!("stratum: TLS up, certificate verified for {host}");
        }
        Ok(Link::Tls {
            tls: Mutex::new(SendTls(t)),
            fd,
        })
    }

    /// End the connection from another thread; a blocked read returns.
    pub fn shutdown(&self) {
        let fd = match self {
            Link::Tcp(s) => s.as_raw_fd(),
            Link::Tls { fd, .. } => *fd,
        };
        unsafe {
            sys::lwip_shutdown(fd, sys::SHUT_RDWR as i32);
        }
    }

    pub fn send(&self, mut buf: &[u8]) -> io::Result<()> {
        match self {
            Link::Tcp(s) => {
                let mut w = s;
                w.write_all(buf)?;
                w.flush()
            }
            Link::Tls { tls, fd } => {
                let since = Instant::now();
                while !buf.is_empty() {
                    let r = tls.lock().unwrap().0.write(buf);
                    match r {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(n) => buf = &buf[n..],
                        Err(e) if is_want(&e) => {
                            if since.elapsed() > WRITE_TIMEOUT {
                                return Err(io::ErrorKind::TimedOut.into());
                            }
                            wait(*fd, sys::POLLOUT as i16, 50);
                        }
                        Err(e) => return Err(esp_io(e)),
                    }
                }
                Ok(())
            }
        }
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Link::Tcp(s) => {
                let mut r = s;
                r.read(buf)
            }
            Link::Tls { tls, fd } => {
                let since = Instant::now();
                loop {
                    // Decrypted bytes may already wait inside mbedTLS with
                    // nothing left on the socket: try a read first.
                    let r = tls.lock().unwrap().0.read(buf);
                    match r {
                        Ok(n) => return Ok(n), // 0 = closed by the pool
                        Err(e) if is_want(&e) => {}
                        Err(e) => return Err(esp_io(e)),
                    }
                    if since.elapsed() > READ_TIMEOUT {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    wait(*fd, sys::POLLIN as i16, 1000);
                }
            }
        }
    }
}

/// Block until `fd` is ready for `events` or `ms` pass.
fn wait(fd: i32, events: i16, ms: i32) {
    let mut p = sys::pollfd {
        fd,
        events,
        revents: 0,
    };
    unsafe {
        sys::poll(&mut p, 1, ms);
    }
}

/// `Read` over a shared link, for a `BufReader` on the session thread.
pub struct LinkReader<'a>(pub &'a Link);

impl Read for LinkReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.recv(buf)
    }
}
