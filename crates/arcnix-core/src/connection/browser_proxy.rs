//! Loopback forwarding through a dedicated, host-key-verified SSH transport:
//! SOCKS5 CONNECT for the browser and one fixed destination for managed apps.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ssh2::{Channel, ErrorCode, Session};
use zeroize::Zeroizing;

use crate::remote::{CommandStream, PendingConnection, RemoteSession};
use crate::remote_tasks::quote;
use crate::{ServerProfile, transport_error};

const MAX_CLIENTS: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const BUFFER_SIZE: usize = 16 * 1024;
const EAGAIN: ErrorCode = ErrorCode::Session(-37);

/// Reserve a stable loopback endpoint for one browser profile during this client run.
pub fn listener() -> Result<TcpListener> {
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("Cannot start browser proxy")?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagnosis {
    Reachable,
    ForwardingDenied,
    DestinationUnreachable,
    TimedOut,
    TunnelUnavailable,
    ForwardingFailed,
}

pub fn forwarding_destination(address: &str) -> Result<String> {
    let url = crate::browser::parse_address(address)?;
    let host = url.host_str().context("Missing browser destination")?;
    let port = url
        .port_or_known_default()
        .filter(|&port| port != 0)
        .context("Invalid browser destination port")?;
    ensure!(
        host.len() <= 255
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_:[]".contains(&byte)),
        "Unsupported forwarding destination"
    );
    Ok(format!("{host}:{port}"))
}

pub fn repair_forwarding(session: &RemoteSession, sudo: &str) -> Result<()> {
    ensure!(
        sudo.len() <= 4095 && !sudo.contains(['\r', '\n', '\0']),
        "Invalid administrator password"
    );
    let program = include_str!("browser_forwarding.py");
    let sudo_options = if sudo.is_empty() { "-n" } else { "-S -p ''" };
    let command = format!(
        r#"# arcnix:browser:repair account
set -eu
login=$(/usr/bin/id -un)
connection=${{SSH_CONNECTION:-}}
set -- /usr/bin/python3 -I -c {program} "$login" "$connection"
if [ "$(/usr/bin/id -u)" = 0 ]; then exec "$@"; fi
if [ -x /usr/bin/sudo ]; then exec /usr/bin/sudo {sudo_options} -- "$@"; fi
printf 'Administrator access is required to change SSH forwarding.\n' >&2
exit 1
"#,
        program = quote(program),
    );
    let password = Zeroizing::new(format!("{sudo}\n"));
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let result = session.exec_stream_checked(
        &command,
        (!sudo.is_empty()).then_some(password.as_bytes()),
        Duration::from_secs(90),
        None,
        false,
        |stream, chunk| {
            ensure!(
                output.len() + errors.len() + chunk.len() <= 4096,
                "SSH forwarding repair response exceeded the size limit"
            );
            match stream {
                CommandStream::Stdout => output.extend_from_slice(chunk),
                CommandStream::Stderr => errors.extend_from_slice(chunk),
            }
            Ok(())
        },
    )?;
    ensure!(
        result.exit_code == 0
            && String::from_utf8_lossy(&output).trim() == "ARCNIX_BROWSER_FORWARDING_READY_V1",
        "{}",
        if errors.is_empty() {
            "The server did not confirm the SSH forwarding change.".into()
        } else {
            String::from_utf8_lossy(&errors).trim().to_owned()
        }
    );
    Ok(())
}

/// Check only the current destination through an authenticated SSH channel.
/// No HTTP request, browser state, or remote configuration is involved.
pub fn diagnose(
    profile: &ServerProfile,
    fingerprint: &str,
    secret: Zeroizing<String>,
    address: &str,
) -> Diagnosis {
    let Ok(url) = crate::browser::parse_address(address) else {
        return Diagnosis::ForwardingFailed;
    };
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return Diagnosis::ForwardingFailed;
    };
    let session = PendingConnection::open_for_known_host(profile, fingerprint)
        .and_then(|pending| pending.authenticate_transport(&secret, fingerprint));
    drop(secret);
    let Ok(session) = session else {
        return Diagnosis::TunnelUnavailable;
    };
    session.set_timeout(5_000);
    match session.channel_direct_tcpip(host.trim_matches(['[', ']']), port, None) {
        Ok(_channel) => Diagnosis::Reachable,
        Err(error) if error.message().ends_with("(administratively prohibited)") => {
            Diagnosis::ForwardingDenied
        }
        Err(error) if error.message().ends_with("(connect failed)") => {
            Diagnosis::DestinationUnreachable
        }
        Err(error) if matches!(error.code(), ErrorCode::Session(-9 | -30)) => Diagnosis::TimedOut,
        Err(error) => {
            if transport_error::is_connection_error(&error.into()) {
                Diagnosis::TunnelUnavailable
            } else {
                Diagnosis::ForwardingFailed
            }
        }
    }
}

/// Runs until the owning browser closes or the transport fails.
pub fn run(
    listener: &TcpListener,
    profile: &ServerProfile,
    fingerprint: &str,
    secret: Zeroizing<String>,
    stopped: impl Fn() -> bool,
    ready: impl FnOnce(u16),
    denied: impl Fn(&str, u16),
) -> Result<()> {
    let session = PendingConnection::open_for_known_host(profile, fingerprint)?
        .authenticate_transport(&secret, fingerprint)?;
    drop(secret);
    if stopped() {
        return Ok(());
    }
    session.set_blocking(false);
    // Clear anything queued while this profile was closed.
    for _ in 0..MAX_CLIENTS {
        if listener.accept().is_err() {
            break;
        }
    }
    ready(listener.local_addr()?.port());
    serve(
        listener,
        &session,
        None,
        stopped,
        |host, port, diagnosis| {
            if diagnosis == Diagnosis::ForwardingDenied {
                denied(host, port);
            }
        },
    )
}

/// Forward every connection on the listener to one destination, resolved and
/// opened by the SSH server. Runs until stopped or the transport fails.
#[allow(clippy::too_many_arguments)]
pub fn run_fixed(
    listener: &TcpListener,
    profile: &ServerProfile,
    fingerprint: &str,
    secret: Zeroizing<String>,
    destination: (&str, u16),
    stopped: impl Fn() -> bool,
    ready: impl FnOnce(),
    failed: impl Fn(Diagnosis),
) -> Result<()> {
    let session = PendingConnection::open_for_known_host(profile, fingerprint)?
        .authenticate_transport(&secret, fingerprint)?;
    drop(secret);
    if stopped() {
        return Ok(());
    }
    session.set_blocking(false);
    ready();
    serve(
        listener,
        &session,
        Some(destination),
        stopped,
        |_, _, diagnosis| failed(diagnosis),
    )
}

struct Target {
    host: String,
    port: u16,
}

enum Request {
    Incomplete,
    Reject(u8),
    Connect(Target, usize),
}

fn request(bytes: &[u8]) -> Request {
    if bytes.len() < 4 {
        return Request::Incomplete;
    }
    if bytes[0] != 5 || bytes[1] != 1 || bytes[2] != 0 {
        return Request::Reject(7);
    }
    let end = match bytes[3] {
        1 => 8,
        3 if bytes.len() < 5 => return Request::Incomplete,
        3 if bytes[4] > 0 => 5 + usize::from(bytes[4]),
        4 => 20,
        _ => return Request::Reject(8),
    };
    if bytes.len() < end + 2 {
        return Request::Incomplete;
    }
    let host = match bytes[3] {
        1 => Ipv4Addr::from(<[u8; 4]>::try_from(&bytes[4..8]).unwrap()).to_string(),
        4 => Ipv6Addr::from(<[u8; 16]>::try_from(&bytes[4..20]).unwrap()).to_string(),
        _ => {
            let Ok(name) = std::str::from_utf8(&bytes[5..end]) else {
                return Request::Reject(8);
            };
            // Chromium uses the domain field for textual IP addresses as well.
            let literal = name
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or(name);
            if literal.parse::<Ipv6Addr>().is_ok() {
                literal.to_owned()
            } else if name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
            {
                name.to_owned()
            } else {
                return Request::Reject(8);
            }
        }
    };
    let port = u16::from_be_bytes([bytes[end], bytes[end + 1]]);
    if port == 0 {
        return Request::Reject(1);
    }
    Request::Connect(Target { host, port }, end + 2)
}

enum Phase {
    Greeting,
    Request,
    Waiting(Target),
    Connecting { target: Target, since: Instant },
    Relay(Box<Relay>),
    Reject,
}

struct Client {
    socket: TcpStream,
    phase: Phase,
    created: Instant,
    input: Vec<u8>,
    reply: Vec<u8>,
    sent: usize,
}

impl Client {
    fn new(socket: TcpStream, fixed: Option<(&str, u16)>) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        socket.set_nodelay(true)?;
        Ok(Self {
            socket,
            phase: fixed.map_or(Phase::Greeting, |(host, port)| {
                Phase::Waiting(Target {
                    host: host.to_owned(),
                    port,
                })
            }),
            created: Instant::now(),
            input: Vec::new(),
            reply: Vec::new(),
            sent: 0,
        })
    }

    fn connect_reply(&mut self, status: u8) {
        self.reply = vec![5, status, 0, 1, 0, 0, 0, 0, 0, 0];
        self.sent = 0;
    }

    fn handshake(&mut self) -> io::Result<bool> {
        if self.sent < self.reply.len() {
            match self.socket.write(&self.reply[self.sent..]) {
                Ok(0) => return Ok(false),
                Ok(count) => self.sent += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(error) => return Err(error),
            }
            if self.sent < self.reply.len() {
                return Ok(true);
            }
        }
        if matches!(self.phase, Phase::Reject) {
            return Ok(false);
        }
        if !matches!(self.phase, Phase::Greeting | Phase::Request) {
            return Ok(true);
        }
        let mut incoming = [0; 1024];
        match self.socket.read(&mut incoming) {
            Ok(0) => return Ok(false),
            Ok(count) => self.input.extend_from_slice(&incoming[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if matches!(self.phase, Phase::Greeting) && self.input.len() >= 2 {
            let end = 2 + usize::from(self.input[1]);
            if self.input.len() >= end {
                let accepted = self.input[0] == 5 && self.input[2..end].contains(&0);
                self.reply = vec![5, if accepted { 0 } else { 255 }];
                self.sent = 0;
                self.input.drain(..end);
                self.phase = if accepted {
                    Phase::Request
                } else {
                    Phase::Reject
                };
            }
        } else if matches!(self.phase, Phase::Request) {
            match request(&self.input) {
                Request::Connect(target, used) => {
                    self.input.drain(..used);
                    self.phase = Phase::Waiting(target);
                }
                Request::Reject(code) => {
                    self.connect_reply(code);
                    self.phase = Phase::Reject;
                }
                Request::Incomplete => {}
            }
        }
        Ok(true)
    }
}

struct Pipe {
    data: [u8; BUFFER_SIZE],
    start: usize,
    end: usize,
    eof: bool,
}

impl Default for Pipe {
    fn default() -> Self {
        Self {
            data: [0; BUFFER_SIZE],
            start: 0,
            end: 0,
            eof: false,
        }
    }
}

impl Pipe {
    fn empty(&self) -> bool {
        self.start == self.end
    }

    fn fill(&mut self, reader: &mut impl Read) -> io::Result<bool> {
        if self.eof || !self.empty() {
            return Ok(false);
        }
        match reader.read(&mut self.data) {
            Ok(count) => {
                self.start = 0;
                self.end = count;
                self.eof = count == 0;
                Ok(count > 0)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn flush(&mut self, writer: &mut impl Write) -> io::Result<bool> {
        if self.empty() {
            return Ok(false);
        }
        match writer.write(&self.data[self.start..self.end]) {
            Ok(0) => Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => {
                self.start += count;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }
}

struct Relay {
    channel: Channel,
    to_remote: Pipe,
    to_local: Pipe,
    sent_eof: bool,
    local_shutdown: bool,
    closing: bool,
}

impl Relay {
    fn new(channel: Channel, input: &[u8]) -> Self {
        let mut to_remote = Pipe::default();
        to_remote.data[..input.len()].copy_from_slice(input);
        to_remote.end = input.len();
        Self {
            channel,
            to_remote,
            to_local: Pipe::default(),
            sent_eof: false,
            local_shutdown: false,
            closing: false,
        }
    }

    fn tick(&mut self, socket: &mut TcpStream, session: &Session) -> Result<bool> {
        let mut progress = false;
        if !self.closing {
            let local = self
                .to_remote
                .fill(socket)
                .and_then(|read| self.to_local.flush(socket).map(|write| read || write));
            match local {
                Ok(changed) => progress |= changed,
                Err(_) => self.closing = true,
            }
            if !self.closing {
                let remote = self.to_local.fill(&mut self.channel).and_then(|read| {
                    self.to_remote
                        .flush(&mut self.channel)
                        .map(|write| read || write)
                });
                match remote {
                    Ok(changed) => progress |= changed,
                    Err(error) => {
                        let error = transport_error::stream_error(session, error);
                        if transport_error::is_connection_error(&error) {
                            return Err(error);
                        }
                        self.closing = true;
                    }
                }
                if self.to_remote.eof && self.to_remote.empty() && !self.sent_eof {
                    match self.channel.send_eof() {
                        Ok(()) => self.sent_eof = true,
                        Err(error) if error.code() == EAGAIN => {}
                        Err(_) => self.closing = true,
                    }
                }
                if self.to_local.eof && self.to_local.empty() && !self.local_shutdown {
                    let _ = socket.shutdown(Shutdown::Write);
                    self.local_shutdown = true;
                }
                self.closing |= self.local_shutdown && self.sent_eof;
            }
        }
        Ok(progress)
    }

    fn closed(&mut self) -> bool {
        self.closing && !matches!(self.channel.close(), Err(error) if error.code() == EAGAIN)
    }
}

fn serve(
    listener: &TcpListener,
    session: &Session,
    fixed: Option<(&str, u16)>,
    stopped: impl Fn() -> bool,
    failed: impl Fn(&str, u16, Diagnosis),
) -> Result<()> {
    let mut clients = Vec::<Client>::new();
    let mut opening = false;
    while !stopped() {
        let mut progress = false;
        if clients.len() < MAX_CLIENTS {
            match listener.accept() {
                Ok((socket, _)) => {
                    if let Ok(client) = Client::new(socket, fixed) {
                        clients.push(client);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut index = 0;
        while index < clients.len() {
            let client = &mut clients[index];
            let mut keep = true;
            match &mut client.phase {
                Phase::Connecting { target, since } => {
                    if since.elapsed() > CONNECT_TIMEOUT {
                        bail!("SSH browser forwarding timed out");
                    }
                    // The SSH server resolves names and opens the destination socket.
                    match session.channel_direct_tcpip(&target.host, target.port, None) {
                        Ok(channel) => {
                            if fixed.is_none() {
                                client.connect_reply(0);
                            }
                            client.phase =
                                Phase::Relay(Box::new(Relay::new(channel, &client.input)));
                            client.input.clear();
                            opening = false;
                            progress = true;
                        }
                        Err(error) if error.code() == EAGAIN => {}
                        Err(error) => {
                            let diagnosis =
                                if error.message().ends_with("(administratively prohibited)") {
                                    Diagnosis::ForwardingDenied
                                } else if error.message().ends_with("(connect failed)") {
                                    Diagnosis::DestinationUnreachable
                                } else {
                                    Diagnosis::ForwardingFailed
                                };
                            let error = anyhow::Error::new(error);
                            if transport_error::is_connection_error(&error) {
                                return Err(error);
                            }
                            failed(&target.host, target.port, diagnosis);
                            if fixed.is_none() {
                                client.connect_reply(5);
                            }
                            client.phase = Phase::Reject;
                            opening = false;
                        }
                    }
                }
                _ => {
                    keep = client.handshake().unwrap_or(false);
                    if let Phase::Relay(relay) = &mut client.phase {
                        if !keep {
                            relay.closing = true;
                        }
                        if client.sent == client.reply.len() {
                            progress |= relay.tick(&mut client.socket, session)?;
                        }
                        keep = !relay.closed();
                    } else if client.created.elapsed() > HANDSHAKE_TIMEOUT {
                        keep = false;
                    } else if !opening && matches!(client.phase, Phase::Waiting(_)) {
                        let Phase::Waiting(target) =
                            std::mem::replace(&mut client.phase, Phase::Reject)
                        else {
                            unreachable!()
                        };
                        client.phase = Phase::Connecting {
                            target,
                            since: Instant::now(),
                        };
                        opening = true;
                        progress = true;
                    }
                }
            }
            if keep {
                index += 1;
            } else {
                clients.swap_remove(index);
            }
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(if clients.is_empty() {
                18
            } else {
                4
            }));
        }
    }
    Ok(())
}
