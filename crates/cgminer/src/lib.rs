//! The cgminer API over TCP, the way an Avalon's MM speaks it.
//!
//! A client connects, writes one request — JSON `{"command": "estats"}`
//! (optionally with `"parameter"`) or a bare word — and reads the reply:
//! one JSON document terminated by a NUL byte, after which the server
//! closes the connection. Clients like WarpTune's `AvalonClient` write
//! the request and then `read_to_end`, so the server must recognise a
//! complete request on its own: a balanced JSON object, a newline, or a
//! short silence.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use warpaxe_sim::AvalonQ;

/// Shared device behind the server.
pub type Shared = Arc<Mutex<AvalonQ>>;

/// Read one request off the socket. Returns `(command, parameter)`.
pub fn read_request(stream: &mut TcpStream) -> Option<(String, Option<String>)> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if complete(&buf) {
                    break;
                }
            }
            Err(_) => break, // timeout or error: work with what we have
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let text = text.trim().trim_end_matches('\0').trim();
    if text.is_empty() {
        return None;
    }
    if text.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let cmd = v.get("command")?.as_str()?.to_string();
        let param = v
            .get("parameter")
            .and_then(|p| p.as_str())
            .map(str::to_string);
        Some((cmd, param))
    } else {
        // Plain text: `estats` or `ascset|0,workmode,2`.
        let mut it = text.splitn(2, '|');
        let cmd = it.next()?.trim().to_string();
        let param = it.next().map(|p| p.trim().to_string());
        Some((cmd, param))
    }
}

fn complete(buf: &[u8]) -> bool {
    let s = String::from_utf8_lossy(buf);
    let t = s.trim_start();
    if t.starts_with('{') {
        let mut depth = 0i32;
        for c in t.chars() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    } else {
        t.contains('\n') || t.contains('\0')
    }
}

/// Answer one connection.
pub fn handle(dev: &Shared, mut stream: TcpStream) {
    let Some((cmd, param)) = read_request(&mut stream) else {
        return;
    };
    let reply = dev.lock().unwrap().command(&cmd, param.as_deref());
    let mut bytes = reply.to_string().into_bytes();
    bytes.push(0);
    let _ = stream.write_all(&bytes);
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Serve forever.
pub fn serve(listener: &TcpListener, dev: &Shared) {
    for stream in listener.incoming().flatten() {
        handle(dev, stream);
    }
}

/// Bind on `addr` (use port 0 for an ephemeral one) and serve on a
/// background thread. Returns the bound address.
pub fn spawn(addr: &str, dev: Shared) -> std::io::Result<std::net::SocketAddr> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    std::thread::spawn(move || serve(&listener, &dev));
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(addr: std::net::SocketAddr, req: &str) -> serde_json::Value {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        assert_eq!(buf.last(), Some(&0), "NUL-terminated");
        buf.pop();
        serde_json::from_slice(&buf).unwrap()
    }

    #[test]
    fn json_request_without_client_eof_gets_a_nul_terminated_reply() {
        let dev = Arc::new(Mutex::new(AvalonQ::settled(0, 23.0, 1)));
        let addr = spawn("127.0.0.1:0", Arc::clone(&dev)).unwrap();
        let v = ask(addr, r#"{"command":"estats"}"#);
        assert_eq!(v["STATUS"][0]["STATUS"], "S");
        assert!(v["STATS"][0]["MM ID0:Summary"]
            .as_str()
            .unwrap()
            .contains("WORKMODE[0]"));
        let v = ask(addr, r#"{"command":"ascset","parameter":"0,workmode,2"}"#);
        assert_eq!(v["STATUS"][0]["STATUS"], "S");
        assert_eq!(dev.lock().unwrap().mode().index, 2);
        let v = ask(addr, "summary\n");
        assert!(v["SUMMARY"][0]["Accepted"].is_u64());
        let v = ask(addr, r#"{"command":"nonsense"}"#);
        assert_eq!(v["STATUS"][0]["STATUS"], "E");
    }
}
