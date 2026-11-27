//! Live loopback SSH + RESP2 integration tests for the Redis app.
//! Requires tools/sftp_fixture.py (with `redis-enabled`) and
//! ARCNIX_TEST_SSH_ADDR=127.0.0.1:<port>.
//! Run via: python3 tools/redis_e2e_verify.py
use std::collections::BTreeMap;
use std::net::SocketAddr;

use anyhow::{Context, Result};
use arcnix_core::redis::{self, Auth, AuthStatus, Control, WriteMode, WriteOp};
use arcnix_core::remote::{PendingConnection, RemoteSession};
use arcnix_core::{AuthMethod, ServerProfile, server_apps};
use zeroize::Zeroizing;

fn connect() -> Result<RemoteSession> {
    let addr: SocketAddr = std::env::var("ARCNIX_TEST_SSH_ADDR")
        .context("Start loopback fixture and set ARCNIX_TEST_SSH_ADDR")?
        .parse()?;
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "Integration fixture must use a loopback address"
    );
    let profile = ServerProfile {
        host: addr.ip().to_string(),
        port: addr.port(),
        username: "arcnix".into(),
        auth: AuthMethod::Password,
        ..Default::default()
    };
    let pending = PendingConnection::open(&profile)?;
    let fingerprint = pending.fingerprint().to_owned();
    pending.authenticate("arcnix-test", &fingerprint)
}

#[test]
#[ignore = "requires loopback SSH + Redis fixture"]
fn redis_full_e2e_cases() -> Result<()> {
    let session = connect()?;

    // TC-01: Application discovery detects Redis.
    let apps = server_apps::discover(&session)?;
    assert!(apps.redis, "TC-01 failed: Redis was not discovered");
    println!("[PASS] TC-01: SSH discovery detects Redis");

    // TC-02: Overview reads both host and password-protected Docker instances.
    let initial = redis::read(&session, &BTreeMap::new())?;
    assert_eq!(initial.instances.len(), 2, "TC-02 expected 2 instances");
    let host = initial
        .instances
        .iter()
        .find(|i| i.key == "host")
        .context("TC-02 missing host instance")?;
    let host_endpoint = host.endpoint.as_ref().context("missing host endpoint")?;
    assert!(host.running);
    assert_eq!(host.auth_status, AuthStatus::Ok);
    let host_ov = host
        .overview
        .as_ref()
        .context("TC-02 missing host overview")?;
    assert_eq!(host_ov.version, "7.2.5");
    assert_eq!(host_ov.total_keys, 125);
    assert!(host_ov.aof_enabled);
    assert_eq!(host_ov.rdb_last_bgsave_status, "ok");
    assert!((host_ov.hit_rate_percent().unwrap() - 94.0).abs() < 0.01);
    assert_eq!(host_ov.databases.len(), 2);

    let docker = initial
        .instances
        .iter()
        .find(|i| i.docker.is_some())
        .context("TC-02 missing docker instance")?;
    assert!(docker.running);
    assert_eq!(docker.title, "redis-auth");
    assert_eq!(docker.auth_status, AuthStatus::Required);
    assert!(docker.overview.is_none());
    println!("[PASS] TC-02: Overview reads host (7.2.5) and detects auth-locked Docker container");

    // TC-03: Wrong password fails cleanly; right password unlocks Docker Redis.
    let mut bad_auth = BTreeMap::new();
    bad_auth.insert(
        docker.key.clone(),
        Auth {
            username: String::new(),
            password: Zeroizing::new("wrong-pass".into()),
        },
    );
    let bad_read = redis::read(&session, &bad_auth)?;
    let bad_docker = bad_read
        .instances
        .iter()
        .find(|i| i.key == docker.key)
        .unwrap();
    assert!(
        matches!(&bad_docker.auth_status, AuthStatus::Failed(msg) if msg.contains("WRONGPASS")),
        "TC-03 expected WRONGPASS, got {:?}",
        bad_docker.auth_status
    );

    let mut good_auth = BTreeMap::new();
    good_auth.insert(
        docker.key.clone(),
        Auth {
            username: String::new(),
            password: Zeroizing::new("s3cr3t".into()),
        },
    );
    let unlocked = redis::read(&session, &good_auth)?;
    let good_docker = unlocked
        .instances
        .iter()
        .find(|i| i.key == docker.key)
        .unwrap();
    assert_eq!(good_docker.auth_status, AuthStatus::Ok);
    assert_eq!(good_docker.overview.as_ref().unwrap().version, "7.4.1");
    let docker_scan = redis::scan_keys(
        &session,
        good_docker
            .endpoint
            .as_ref()
            .context("missing Docker endpoint")?,
        good_auth.get(&docker.key),
        0,
        "0",
        "*",
    )?;
    assert!(docker_scan.keys.iter().any(|k| k.name == "app:config"));
    println!("[PASS] TC-03: Wrong password rejected (WRONGPASS); valid password unlocks via stdin");

    // TC-04: Multi-DB scan (db1 vs db0).
    let db1_scan = redis::scan_keys(&session, host_endpoint, None, 1, "0", "*")?;
    let db1_names: Vec<&str> = db1_scan.keys.iter().map(|k| k.name.as_str()).collect();
    assert!(db1_names.contains(&"cache:homepage"));
    assert!(db1_names.contains(&"user:2002"));
    assert_eq!(db1_names.len(), 2);
    println!("[PASS] TC-04: Multi-DB switching scans db1 cleanly");

    // TC-05: Cursor SCAN pagination & pattern filter on db0 (123 keys).
    let page1 = redis::scan_keys(&session, host_endpoint, None, 0, "0", "*")?;
    assert_eq!(page1.keys.len(), 100);
    assert_ne!(page1.cursor, "0");
    assert!(page1.truncated);
    let page2 = redis::scan_keys(&session, host_endpoint, None, 0, &page1.cursor, "*")?;
    assert_eq!(page2.cursor, "0");
    let mut all_names: Vec<String> = page1
        .keys
        .iter()
        .chain(page2.keys.iter())
        .map(|k| k.name.clone())
        .collect();
    all_names.sort();
    all_names.dedup();
    assert_eq!(all_names.len(), 123);

    let filtered = redis::scan_keys(&session, host_endpoint, None, 0, "0", "user:*")?;
    assert_eq!(filtered.keys.len(), 1);
    assert_eq!(filtered.keys[0].name, "user:1001");
    println!(
        "[PASS] TC-05: Bounded cursor SCAN pagination (100 + 23 = 123) and pattern filter work"
    );

    // TC-06: Inspect all 6 Redis types + binary + large truncated preview.
    let kv_str = redis::read_key(&session, host_endpoint, None, 0, "app:config")?;
    assert_eq!(kv_str.kind, "string");
    assert!(!kv_str.binary && !kv_str.truncated);
    assert!(
        kv_str
            .string_value
            .as_deref()
            .unwrap()
            .contains("redis_app")
    );

    let kv_hash = redis::read_key(&session, host_endpoint, None, 0, "user:1001")?;
    assert_eq!(kv_hash.kind, "hash");
    assert_eq!(kv_hash.entries.len(), 3);

    let kv_list = redis::read_key(&session, host_endpoint, None, 0, "jobs:pending")?;
    assert_eq!(kv_list.kind, "list");
    assert_eq!(kv_list.entries.len(), 3);

    let kv_set = redis::read_key(&session, host_endpoint, None, 0, "tags:featured")?;
    assert_eq!(kv_set.kind, "set");
    assert_eq!(kv_set.entries.len(), 3);

    let kv_zset = redis::read_key(&session, host_endpoint, None, 0, "leaderboard:daily")?;
    assert_eq!(kv_zset.kind, "zset");
    assert_eq!(kv_zset.entries.len(), 3);

    let kv_stream = redis::read_key(&session, host_endpoint, None, 0, "events:audit")?;
    assert_eq!(kv_stream.kind, "stream");
    assert_eq!(kv_stream.entries.len(), 2);

    let kv_bin = redis::read_key(&session, host_endpoint, None, 0, "blob:binary")?;
    assert!(kv_bin.binary);

    let kv_large = redis::read_key(&session, host_endpoint, None, 0, "blob:large")?;
    assert!(kv_large.truncated);
    println!(
        "[PASS] TC-06: All 6 types (string/hash/list/set/zset/stream) + binary + truncated previews work"
    );

    // TC-07: Edit string value with KEEPTTL preserves existing expiration.
    let before_ttl = kv_str.ttl;
    assert!(before_ttl > 0);
    let updated_json = "{\n  \"env\": \"production\",\n  \"redis_app\": true\n}";
    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::SetString {
            key: "app:config".into(),
            value: updated_json.into(),
            ttl_seconds: None,
            keep_ttl: true,
        },
    )?;
    let reread = redis::read_key(&session, host_endpoint, None, 0, "app:config")?;
    assert_eq!(reread.string_value.as_deref(), Some(updated_json));
    assert!(reread.ttl > 0 && reread.ttl <= before_ttl);
    println!("[PASS] TC-07: String update persisted with KEEPTTL intact");

    // TC-08: Hash / List / Set / ZSet mutations.
    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::SetHashField {
            key: "user:1001".into(),
            field: "team".into(),
            value: "platform".into(),
        },
    )?;
    let h_after_add = redis::read_key(&session, host_endpoint, None, 0, "user:1001")?;
    assert!(
        h_after_add
            .entries
            .iter()
            .any(|e| e.label == "team" && e.value == "platform")
    );
    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::DeleteHashField {
            key: "user:1001".into(),
            field: "team".into(),
        },
    )?;
    let h_after_del = redis::read_key(&session, host_endpoint, None, 0, "user:1001")?;
    assert!(!h_after_del.entries.iter().any(|e| e.label == "team"));

    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::PushList {
            key: "jobs:pending".into(),
            value: "verify-e2e".into(),
        },
    )?;
    assert_eq!(
        redis::read_key(&session, host_endpoint, None, 0, "jobs:pending")?
            .entries
            .len(),
        4
    );

    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::AddSetMember {
            key: "tags:featured".into(),
            member: "valkey".into(),
        },
    )?;
    assert_eq!(
        redis::read_key(&session, host_endpoint, None, 0, "tags:featured")?
            .entries
            .len(),
        4
    );
    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::RemoveSetMember {
            key: "tags:featured".into(),
            member: "valkey".into(),
        },
    )?;
    assert_eq!(
        redis::read_key(&session, host_endpoint, None, 0, "tags:featured")?
            .entries
            .len(),
        3
    );

    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::AddZSetMember {
            key: "leaderboard:daily".into(),
            score: 105.0,
            member: "dave".into(),
        },
    )?;
    assert_eq!(
        redis::read_key(&session, host_endpoint, None, 0, "leaderboard:daily")?
            .entries
            .len(),
        4
    );
    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::RemoveZSetMember {
            key: "leaderboard:daily".into(),
            member: "dave".into(),
        },
    )?;
    assert_eq!(
        redis::read_key(&session, host_endpoint, None, 0, "leaderboard:daily")?
            .entries
            .len(),
        3
    );
    println!("[PASS] TC-08: Hash, List, Set, and Sorted Set entry add/remove verified");

    // TC-09: Set TTL & Persist.
    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::SetTtl {
            key: "user:1001".into(),
            ttl_seconds: Some(300),
        },
    )?;
    let ttl_set = redis::read_key(&session, host_endpoint, None, 0, "user:1001")?;
    assert!(ttl_set.ttl > 0 && ttl_set.ttl <= 300);

    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::SetTtl {
            key: "user:1001".into(),
            ttl_seconds: None,
        },
    )?;
    let ttl_persisted = redis::read_key(&session, host_endpoint, None, 0, "user:1001")?;
    assert_eq!(ttl_persisted.ttl, -1);
    println!("[PASS] TC-09: Set TTL and Persist verified");

    // TC-10: Create new key & delete key.
    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Create,
        &WriteOp::SetString {
            key: "e2e:temp".into(),
            value: "created-over-ssh".into(),
            ttl_seconds: Some(120),
            keep_ttl: false,
        },
    )?;
    let created = redis::read_key(&session, host_endpoint, None, 0, "e2e:temp")?;
    assert_eq!(created.string_value.as_deref(), Some("created-over-ssh"));
    redis::write_key(
        &session,
        host_endpoint,
        None,
        0,
        WriteMode::Edit,
        &WriteOp::DeleteKey {
            key: "e2e:temp".into(),
        },
    )?;
    let missing = redis::read_key(&session, host_endpoint, None, 0, "e2e:temp");
    assert!(missing.is_err());
    println!("[PASS] TC-10: Key creation and deletion verified");

    // TC-11: Clients & Slowlog diagnostics.
    let diag = redis::read_diagnostics(&session, host_endpoint, None)?;
    assert!(!diag.clients_truncated);
    assert!(diag.clients.iter().any(|c| c.name == "api-worker"));
    assert!(diag.clients.iter().any(|c| c.name == "scheduler"));
    assert_eq!(diag.slowlog.len(), 2);
    assert_eq!(diag.slowlog[0].command, "HGETALL user:1001");
    assert_eq!(diag.slowlog[0].duration_us, 18400);
    println!("[PASS] TC-11: Connected clients and slowlog diagnostics verified");

    // TC-12: Audited service control templates (host & docker).
    let stop_host = redis::control(host, Control::Stop)?;
    assert_eq!(stop_host.snippet.id, "builtin:redis:stop");
    assert!(
        stop_host
            .snippet
            .command
            .contains("systemctl stop -- 'redis-server.service'")
    );
    let restart_docker = redis::control(docker, Control::Restart)?;
    assert_eq!(restart_docker.snippet.id, "builtin:redis:restart");
    assert!(restart_docker.snippet.command.contains("docker restart"));
    println!("[PASS] TC-12: Audited service control templates verified");

    Ok(())
}
