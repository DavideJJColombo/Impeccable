//! Keeps tiny_http's connection pool from stranding a connection.
//!
//! tiny_http 0.12 hands every accepted connection to a `TaskPool` task that
//! serves that connection's requests until the client closes it. `spawn`
//! queues the task instead of starting a thread whenever the pool's count of
//! idle workers is above zero, but that count only drops once a woken worker
//! takes the queue lock again. A burst of connections that arrives faster
//! than the idle workers wake (a browser opening one socket per image on a
//! decision page) can queue more tasks than there are workers. The workers
//! that did wake then sit in a keep-alive read on their own sockets, which a
//! browser holds open for minutes, and the leftover tasks never run: those
//! requests hang until the server exits. The decision page's missing-comp
//! probes hit this on Linux CI, where the burst outran the wakeups on every
//! failing run; the regression test below reproduces it on macOS too.
//!
//! The pool still makes progress whenever a new connection arrives: with no
//! idle worker it starts a thread that drains the queue after its own task,
//! and with an idle one the notify wakes it. So a guard opens and drops an
//! empty connection to the server on a short interval. The empty connection
//! reads EOF and never surfaces as a request, so request handling and idle
//! timers are unaffected, and a stranded connection is served within one
//! interval.

use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const NUDGE_INTERVAL: Duration = Duration::from_millis(250);

/// Stops the nudge thread when dropped. Hold it for the server's lifetime.
pub struct PoolNudge {
    stop: Arc<AtomicBool>,
}

impl Drop for PoolNudge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Starts nudging the server's pool. Returns `None` for a non-TCP listener.
pub fn keep_pool_moving(server: &tiny_http::Server) -> Option<PoolNudge> {
    let addr = server.server_addr().to_ip()?;
    Some(nudge(addr))
}

fn nudge(addr: SocketAddr) -> PoolNudge {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            std::thread::sleep(NUDGE_INTERVAL);
            if flag.load(Ordering::SeqCst) {
                return;
            }
            if TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err() {
                return;
            }
        }
    });
    PoolNudge { stop }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    // Every connection of a burst that lands before the pool's idle workers
    // wake gets an answer. Without the nudge, some round of the six leaves
    // all but four (the pool's floor of workers) unanswered on macOS too.
    #[test]
    fn a_burst_of_keep_alive_connections_is_fully_served() {
        for _ in 0..6 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            // Connected before the server accepts, so its accept loop drains
            // the backlog as one burst.
            let mut socks: Vec<TcpStream> = (0..8).map(|_| TcpStream::connect(addr).unwrap()).collect();
            let server = Arc::new(tiny_http::Server::from_listener(listener, None).unwrap());
            let _nudge = keep_pool_moving(&server).unwrap();
            let responder = server.clone();
            std::thread::spawn(move || {
                for rq in responder.incoming_requests() {
                    let _ = rq.respond(tiny_http::Response::empty(404));
                }
            });
            for s in socks.iter_mut() {
                s.write_all(b"GET /img/0 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
                // Each nudge frees one stranded connection, so four take about a
                // second; without the nudge they never answer.
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            }
            let answered = socks
                .iter_mut()
                .map(|s| {
                    let mut buf = [0u8; 64];
                    matches!(s.read(&mut buf), Ok(n) if n > 0)
                })
                .filter(|answered| *answered)
                .count();
            assert_eq!(answered, 8, "connections stranded in the pool");
            server.unblock();
        }
    }
}
