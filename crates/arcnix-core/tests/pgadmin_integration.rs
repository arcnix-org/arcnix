//! A real pgAdmin backend and SSH tunnel. Needs a component package, a loopback
//! OpenSSH server that allows local forwarding, and PostgreSQL behind it.
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use arcnix_core::pgadmin::{self, Connection, Event};
use arcnix_core::remote::PendingConnection;
use arcnix_core::{AuthMethod, ServerProfile, new_id};
use zeroize::Zeroizing;

fn variable(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("Set {name}"))
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn get(url: &str) -> Result<(u16, String)> {
    let rest = url.strip_prefix("http://").context("loopback URL")?;
    let (host, path) = rest.split_once('/').context("URL path")?;
    let mut stream = TcpStream::connect(host)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let response = String::from_utf8_lossy(&response).into_owned();
    let status = response.split(' ').nth(1).context("HTTP status")?.parse()?;
    Ok((status, response))
}

/// Ask PostgreSQL for TLS through the tunnel. Any server answers S or N.
fn postgres_answers(port: u16) -> Result<bool> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(&[0, 0, 0, 8, 4, 210, 22, 47])?;
    let mut answer = [0];
    Ok(stream.read(&mut answer)? == 1 && matches!(answer[0], b'S' | b'N'))
}

fn saved_port(base: &std::path::Path) -> Result<u16> {
    // The loaded connection file is written by the first launch only.
    let servers = fs::read_dir(base.join("servers"))?
        .next()
        .context("server folder")??
        .path();
    let data = fs::read_dir(servers)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
        .context("data folder")?;
    let file: serde_json::Value = serde_json::from_slice(&fs::read(data.join("servers.json"))?)?;
    Ok(file["Servers"]["1"]["Port"].as_u64().context("port")? as u16)
}

#[test]
#[ignore = "requires a pgAdmin component, loopback SSH and PostgreSQL"]
fn opens_pgadmin_through_ssh_and_stops_cleanly() -> Result<()> {
    let base = std::env::temp_dir().join(format!("arcnix-pgadmin-e2e-{}", new_id()));
    let _cleanup = Cleanup(base.clone());
    let package = PathBuf::from(variable("ARCNIX_TEST_PGADMIN_PACKAGE")?);
    let component = pgadmin::install_local(&base, &package, || Ok(()))?;
    let profile = ServerProfile {
        host: "127.0.0.1".into(),
        port: variable("ARCNIX_TEST_SSH_PORT")?.parse()?,
        username: variable("ARCNIX_TEST_SSH_USER")?,
        auth: AuthMethod::PrivateKey,
        private_key: Some(PathBuf::from(variable("ARCNIX_TEST_SSH_KEY")?)),
        ..Default::default()
    };
    let fingerprint = PendingConnection::open(&profile)?.fingerprint().to_owned();
    let connection = Connection {
        port: variable("ARCNIX_TEST_POSTGRES_PORT")?.parse()?,
        ..Default::default()
    };

    let mut addresses = Vec::new();
    for start in 0..2 {
        let stop = AtomicBool::new(false);
        let (send, receive) = mpsc::channel();
        let address = std::thread::scope(|scope| -> Result<String> {
            let runner = scope.spawn(|| {
                pgadmin::run(
                    &base,
                    &component,
                    &profile,
                    &fingerprint,
                    Zeroizing::new(String::new()),
                    &connection,
                    || stop.load(Ordering::Acquire),
                    |event| {
                        let _ = send.send(event);
                    },
                )
            });
            let check = || -> Result<String> {
                let url = match receive.recv_timeout(Duration::from_secs(120)) {
                    Ok(Event::Ready(url)) => url,
                    Ok(Event::Unreachable(_)) => anyhow::bail!("PostgreSQL was unreachable"),
                    Err(_) => anyhow::bail!("pgAdmin did not become ready"),
                };
                let (origin, _) = url.split_once('?').context("key")?;
                ensure!(get(origin)?.0 == 401, "a missing key was accepted");
                ensure!(
                    get(&format!("{origin}?key=wrong"))?.0 == 401,
                    "a wrong key was accepted"
                );
                let (status, body) = get(&url)?;
                ensure!((200..400).contains(&status), "window got {status}: {body}");
                if start == 0 {
                    ensure!(postgres_answers(saved_port(&base)?)?, "no PostgreSQL");
                }
                Ok(url)
            };
            let result = check();
            stop.store(true, Ordering::Release);
            let ended = runner.join().expect("backend thread");
            let url = result.with_context(|| format!("backend ended with {ended:?}"))?;
            ended?;
            Ok(url)
        })?;
        let host = address
            .trim_start_matches("http://")
            .split('/')
            .next()
            .context("address")?;
        assert!(TcpStream::connect(host).is_err(), "backend still running");
        addresses.push(address);
    }
    assert_ne!(addresses[0], addresses[1]);
    assert_eq!(
        pgadmin::saved_connection(&base, &profile, &fingerprint),
        connection
    );
    Ok(())
}
