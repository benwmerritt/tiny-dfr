use crate::helper_proto::{
    encode_intent, parse_line, sanitize_state, ErrorBudget, HelperMessage, Intent, LineBuffer,
    StateMsg, SUPPORTED_VERSION,
};
use anyhow::{Context, Result};
use nix::sys::{
    epoll::{Epoll, EpollEvent, EpollFlags},
    socket::{getsockopt, sockopt::PeerCredentials},
};
use std::{
    io::{ErrorKind, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    time::{Duration, Instant},
};

pub const LISTENER_EPOLL_DATA: u64 = 4;
pub const CLIENT_EPOLL_DATA: u64 = 5;
// Three missed 2s heartbeats.
const STALE_AFTER: Duration = Duration::from_secs(6);

// The daemon end of the helper socket. Bound while the daemon is still root
// (RuntimeDirectory is root-owned; the socket is chowned to the helper's uid
// and chmod 0600 so kernel DAC gates connects), then served post-privdrop.
// Single client, newest wins: a fresh validated connection replaces the old
// one so helper restarts never get locked out.
pub struct HelperLink {
    listener: UnixListener,
    client: Option<Client>,
    allowed_uid: u32,
    state: StateMsg,
    last_state_at: Option<Instant>,
}

struct Client {
    stream: UnixStream,
    buffer: LineBuffer,
    budget: ErrorBudget,
    hello_done: bool,
}

// Outcome of feeding one read's worth of bytes through a client connection.
#[derive(Default)]
struct Ingest {
    states: Vec<StateMsg>,
    disconnect: bool,
}

impl Client {
    fn new(stream: UnixStream) -> Client {
        Client {
            stream,
            buffer: LineBuffer::new(),
            budget: ErrorBudget::new(),
            hello_done: false,
        }
    }

    fn ingest(&mut self, data: &[u8], now: Instant) -> Ingest {
        let mut out = Ingest::default();
        let lines = match self.buffer.push(data) {
            Ok(lines) => lines,
            Err(_) => {
                eprintln!("helper: oversize line; dropping connection");
                out.disconnect = true;
                return out;
            }
        };
        for line in lines {
            if self.budget.on_message(now) {
                eprintln!("helper: message flood; dropping connection");
                out.disconnect = true;
                return out;
            }
            match parse_line(&line) {
                Ok(HelperMessage::Hello { v }) if !self.hello_done => {
                    if v != SUPPORTED_VERSION {
                        eprintln!("helper: unsupported protocol version {v}; dropping");
                        out.disconnect = true;
                        return out;
                    }
                    self.hello_done = true;
                    self.budget.on_valid();
                }
                Ok(HelperMessage::State(state)) if self.hello_done => {
                    out.states.push(sanitize_state(state));
                    self.budget.on_valid();
                }
                // Hello out of order, state before hello, or garbage: count
                // against the budget; the connection dies when it runs out.
                _ => {
                    if !self.hello_done {
                        // First line must be a valid hello.
                        eprintln!("helper: connection did not start with hello; dropping");
                        out.disconnect = true;
                        return out;
                    }
                    if self.budget.on_invalid() {
                        eprintln!("helper: too many invalid lines; dropping connection");
                        out.disconnect = true;
                        return out;
                    }
                }
            }
        }
        out
    }
}

fn peer_allowed(peer_uid: u32, allowed_uid: u32) -> bool {
    peer_uid == allowed_uid || peer_uid == 0
}

impl HelperLink {
    // Must run before the privilege drop: only root can chown the socket to
    // the helper's uid inside the root-owned RuntimeDirectory.
    pub fn bind(path: &Path, allowed_uid: u32) -> Result<HelperLink> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)
            .with_context(|| format!("binding helper socket at {}", path.display()))?;
        listener.set_nonblocking(true)?;
        std::os::unix::fs::chown(path, Some(allowed_uid), None)
            .with_context(|| format!("chowning {} to uid {allowed_uid}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(HelperLink {
            listener,
            client: None,
            allowed_uid,
            state: StateMsg::default(),
            last_state_at: None,
        })
    }

    pub fn register(&self, epoll: &Epoll) -> Result<()> {
        epoll.add(
            &self.listener,
            EpollEvent::new(EpollFlags::EPOLLIN, LISTENER_EPOLL_DATA),
        )?;
        Ok(())
    }

    // Deliberately keeps `state`/`last_state_at`: a quick helper reconnect
    // (newest-wins replacement) shouldn't flash the fallback strip, and the
    // staleness clock still bounds how long cached state can be rendered.
    fn drop_client(&mut self, epoll: &Epoll) {
        if let Some(client) = self.client.take() {
            let _ = epoll.delete(&client.stream);
            eprintln!("helper disconnected");
        }
    }

    // Accept pending connections (newest validated one wins) and drain the
    // active client's bytes. Returns true when any state message was applied.
    pub fn pump(&mut self, epoll: &Epoll, now: Instant) -> bool {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let uid = match getsockopt(&stream, PeerCredentials) {
                        Ok(creds) => creds.uid(),
                        Err(e) => {
                            eprintln!("helper: SO_PEERCRED failed: {e}; dropping connection");
                            continue;
                        }
                    };
                    if !peer_allowed(uid, self.allowed_uid) {
                        eprintln!("helper: rejecting connection from uid {uid}");
                        continue;
                    }
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    self.drop_client(epoll);
                    if epoll
                        .add(
                            &stream,
                            EpollEvent::new(EpollFlags::EPOLLIN, CLIENT_EPOLL_DATA),
                        )
                        .is_err()
                    {
                        continue;
                    }
                    eprintln!("helper connected (uid {uid})");
                    self.client = Some(Client::new(stream));
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    eprintln!("helper: accept failed: {e}");
                    break;
                }
            }
        }

        let mut applied = false;
        let mut disconnect = false;
        if let Some(client) = self.client.as_mut() {
            let mut chunk = [0u8; 2048];
            loop {
                match client.stream.read(&mut chunk) {
                    Ok(0) => {
                        disconnect = true;
                        break;
                    }
                    Ok(n) => {
                        let result = client.ingest(&chunk[..n], now);
                        for state in result.states {
                            self.state = state;
                            self.last_state_at = Some(now);
                            applied = true;
                        }
                        if result.disconnect {
                            disconnect = true;
                            break;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => {
                        eprintln!("helper: read failed: {e}");
                        disconnect = true;
                        break;
                    }
                }
            }
        }
        if disconnect {
            self.drop_client(epoll);
        }
        applied
    }

    // Best-effort, never blocking: a lost intent is repaired by the next
    // touch (focus) or the state echo (volume).
    pub fn send_intent(&mut self, epoll: &Epoll, intent: &Intent) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        // Never talk to a peer that hasn't completed the hello handshake.
        if !client.hello_done {
            return;
        }
        let payload = encode_intent(intent);
        match client.stream.write(payload.as_bytes()) {
            Ok(n) if n == payload.len() => {}
            Ok(_) | Err(_) => {
                // Partial writes would desync NDJSON framing; a full send
                // buffer means the helper is wedged. Either way: drop the
                // client and let it reconnect fresh.
                self.drop_client(epoll);
            }
        }
    }

    pub fn state(&self) -> &StateMsg {
        &self.state
    }

    pub fn is_fresh(&self, now: Instant) -> bool {
        self.client.is_some()
            && self
                .last_state_at
                .is_some_and(|at| now.saturating_duration_since(at) < STALE_AFTER)
    }

    // Bound the epoll wait so a silently-hung helper is detected within the
    // staleness window rather than at the next unrelated wakeup.
    pub fn staleness_timeout_ms(&self, now: Instant) -> Option<i32> {
        self.client.as_ref()?;
        let at = self.last_state_at?;
        let remaining = STALE_AFTER.saturating_sub(now.saturating_duration_since(at));
        Some(remaining.as_millis().max(1) as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper_proto::{OutputGroup, WsEntry};
    use std::os::unix::net::UnixStream;

    fn test_client() -> Client {
        let (a, _b) = UnixStream::pair().unwrap();
        Client::new(a)
    }

    #[test]
    fn hello_must_come_first_and_match_version() {
        let mut client = test_client();
        let out = client.ingest(b"{\"t\":\"state\",\"outs\":[]}\n", Instant::now());
        assert!(out.disconnect);

        let mut client = test_client();
        let out = client.ingest(b"{\"t\":\"hello\",\"v\":2}\n", Instant::now());
        assert!(out.disconnect);

        let mut client = test_client();
        let out = client.ingest(b"{\"t\":\"hello\",\"v\":1}\n", Instant::now());
        assert!(!out.disconnect);
        assert!(client.hello_done);
    }

    #[test]
    fn state_parses_across_partial_reads() {
        let mut client = test_client();
        let now = Instant::now();
        assert!(
            !client
                .ingest(b"{\"t\":\"hello\",\"v\":1}\n", now)
                .disconnect
        );

        let full = br#"{"t":"state","outs":[{"name":"eDP-1","ws":[{"id":7,"idx":1,"occ":true,"foc":true}]}]}"#;
        let (first, second) = full.split_at(20);
        assert!(client.ingest(first, now).states.is_empty());
        let mut tail = second.to_vec();
        tail.push(b'\n');
        let out = client.ingest(&tail, now);

        assert_eq!(out.states.len(), 1);
        assert_eq!(
            out.states[0].outs,
            vec![OutputGroup {
                name: "eDP-1".into(),
                ws: vec![WsEntry {
                    id: 7,
                    idx: 1,
                    occ: true,
                    foc: true
                }],
            }]
        );
    }

    #[test]
    fn garbage_after_hello_eventually_disconnects() {
        let mut client = test_client();
        let now = Instant::now();
        assert!(
            !client
                .ingest(b"{\"t\":\"hello\",\"v\":1}\n", now)
                .disconnect
        );

        let mut disconnected = false;
        for _ in 0..8 {
            if client.ingest(b"garbage\n", now).disconnect {
                disconnected = true;
                break;
            }
        }
        assert!(disconnected);
    }

    #[test]
    fn oversize_line_disconnects_immediately() {
        let mut client = test_client();
        let now = Instant::now();
        assert!(
            !client
                .ingest(b"{\"t\":\"hello\",\"v\":1}\n", now)
                .disconnect
        );

        let big = vec![b'x'; 5000];
        assert!(client.ingest(&big, now).disconnect);
    }

    #[test]
    fn peer_gate_allows_owner_and_root_only() {
        assert!(peer_allowed(1000, 1000));
        assert!(peer_allowed(0, 1000));
        assert!(!peer_allowed(1001, 1000));
        assert!(!peer_allowed(65534, 1000));
    }

    #[test]
    fn staleness_math() {
        let (a, _b) = UnixStream::pair().unwrap();
        let mut link = HelperLink {
            listener: UnixListener::bind(std::env::temp_dir().join(format!(
                "tiny-dfr-link-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .subsec_nanos()
            )))
            .unwrap(),
            client: Some(Client::new(a)),
            allowed_uid: 1000,
            state: StateMsg::default(),
            last_state_at: None,
        };
        let t0 = Instant::now();

        assert!(!link.is_fresh(t0)); // connected but never any state
        link.last_state_at = Some(t0);
        assert!(link.is_fresh(t0 + Duration::from_secs(5)));
        assert!(!link.is_fresh(t0 + Duration::from_secs(7)));

        link.client = None;
        assert!(!link.is_fresh(t0)); // no client is always stale
    }
}
