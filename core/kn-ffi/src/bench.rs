//! Resource measurement of a full-mode wallet against the regtest devnet:
//! peak memory, CPU, traffic and requests of a full sync, wallet file
//! sizes and timings, and what an idle wallet at the tip costs.
//!
//! ```sh
//! cd tools/devnet && docker compose --profile regtest up -d
//! cd core && CARGO_INCREMENTAL=0 cargo test --release -p kn_ffi --lib bench \
//!     -- --ignored --nocapture
//! ```
//!
//! `KN_BENCH_MINE=<n>` first mines `n` blocks to the bench wallet, so it
//! owns many outputs; `KN_BENCH_IDLE=<seconds>` sets the idle window (60).
//! Traffic is counted by a forwarding proxy between the wallet and the
//! node, so the measured code is the app's own.

use std::io::{Read as _, Write as _};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::network::Network;
use crate::api::nodes::{RUNTIME, select_node};
use crate::api::sync::{SyncEvent, SyncPhase};
use crate::api::wallets::{SyncMode, create_wallet_from_spend_key, store, unlock_wallet};

const NODE: &str = "127.0.0.1:18181";
/// A fixed spend key, so mined outputs accumulate across runs.
const SPEND_KEY: &str = "0b00000000000000000000000000000000000000000000000000000000000000";

#[derive(Default)]
struct Traffic {
    connections: AtomicU64,
    requests: AtomicU64,
    received: AtomicU64,
}

impl Traffic {
    fn take(&self) -> (u64, u64, u64) {
        (
            self.connections.swap(0, Ordering::Relaxed),
            self.requests.swap(0, Ordering::Relaxed),
            self.received.swap(0, Ordering::Relaxed),
        )
    }
}

/// Forwards connections on a local port to the node, counting them.
fn counting_proxy(traffic: Arc<Traffic>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let spawn = |f: Box<dyn FnOnce() + Send>| {
        std::thread::Builder::new()
            .name("bench-proxy".into())
            .spawn(f)
            .unwrap();
    };
    spawn(Box::new(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { continue };
            let Ok(node) = TcpStream::connect(NODE) else {
                continue;
            };
            traffic.connections.fetch_add(1, Ordering::Relaxed);
            let (mut c_in, mut n_out) = (client.try_clone().unwrap(), node.try_clone().unwrap());
            let (mut n_in, mut c_out) = (node, client);
            let up = traffic.clone();
            spawn(Box::new(move || {
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n) = c_in.read(&mut buf) {
                    if n == 0 || n_out.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let posts = buf[..n].windows(5).filter(|w| w == b"POST ").count();
                    up.requests.fetch_add(posts as u64, Ordering::Relaxed);
                    if std::env::var_os("KN_BENCH_ROUTES").is_some() && posts > 0 {
                        let line = String::from_utf8_lossy(&buf[..n.min(400)]);
                        let route = line.split_whitespace().nth(1).unwrap_or("?").to_owned();
                        let method = line
                            .split("\"method\"")
                            .nth(1)
                            .and_then(|m| m.split('"').nth(1))
                            .unwrap_or("")
                            .to_owned();
                        eprintln!("ROUTE {route} {method}");
                    }
                }
                let _ = n_out.shutdown(Shutdown::Write);
            }));
            let down = traffic.clone();
            spawn(Box::new(move || {
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n) = n_in.read(&mut buf) {
                    if n == 0 || c_out.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    down.received.fetch_add(n as u64, Ordering::Relaxed);
                }
                let _ = c_out.shutdown(Shutdown::Write);
            }));
        }
    }));
    port
}

fn status_kib(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix(field))
        .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse().ok())
        .unwrap()
}

/// Resets the peak resident set size (`VmHWM`).
fn reset_peak() {
    std::fs::write("/proc/self/clear_refs", "5").unwrap();
}

/// User plus system CPU time of the process.
fn cpu() -> Duration {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..].split(' ').collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    Duration::from_millis(ticks * 10)
}

/// Bytes written to storage by the process.
fn written() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("write_bytes: "))
        .unwrap()
        .parse()
        .unwrap()
}

/// Context switches of the network runtime's threads: how often they wake.
fn runtime_wakeups() -> u64 {
    let mut total = 0;
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let comm = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        if comm.trim() != "kn-net" {
            continue;
        }
        let status = std::fs::read_to_string(task.path().join("status")).unwrap_or_default();
        for line in status.lines() {
            if let Some(v) = line
                .strip_prefix("voluntary_ctxt_switches:")
                .or_else(|| line.strip_prefix("nonvoluntary_ctxt_switches:"))
            {
                total += v.trim().parse::<u64>().unwrap();
            }
        }
    }
    total
}

#[test]
#[ignore = "needs the regtest devnet and a release build; see the file header"]
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
fn bench() {
    crate::test_store::init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let keys = kn_keys::WalletKeys::from_spend_key(SPEND_KEY).unwrap();
    if let Some(n) = std::env::var("KN_BENCH_MINE")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
    {
        let address = keys.primary_address(kn_keys::Network::Mainnet);
        RUNTIME.block_on(async {
            let (daemon, _) = kn_sync::connect(
                &kn_sync::NodeUrl::parse(NODE).unwrap(),
                kn_keys::Network::Mainnet,
            )
            .await
            .unwrap();
            let address = monero_wallet::address::MoneroAddress::from_str(
                monero_wallet::address::Network::Mainnet,
                &address,
            )
            .unwrap();
            for _ in 0..n.div_ceil(500) {
                daemon.generate_blocks(&address, 500).await.unwrap();
            }
        });
    }
    let idle_secs: u64 = std::env::var("KN_BENCH_IDLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let traffic = Arc::new(Traffic::default());
    let port = counting_proxy(traffic.clone());
    select_node(Network::Mainnet, format!("127.0.0.1:{port}")).unwrap();

    let created = create_wallet_from_spend_key(
        "Bench".into(),
        Network::Mainnet,
        SyncMode::Full,
        SPEND_KEY.into(),
        "pw".into(),
        Some(0),
    )
    .unwrap();
    let id = created.summary().unwrap().id;
    created.lock();
    drop(created);

    let started = Instant::now();
    let wallet = unlock_wallet(id.clone(), "pw".into()).unwrap();
    let unlock_time = started.elapsed();

    // Full sync from genesis.
    let events: Arc<Mutex<Vec<SyncEvent>>> = Arc::default();
    let synced = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let sink = {
        let (events, synced) = (events.clone(), synced.clone());
        move |e: SyncEvent| {
            if e.phase == SyncPhase::Synced {
                *synced.0.lock().unwrap() = true;
                synced.1.notify_all();
            }
            events.lock().unwrap().push(e);
        }
    };
    traffic.take();
    let (written_before, wakeups_before) = (written(), runtime_wakeups());
    reset_peak();
    let rss_before = status_kib("VmRSS:");
    let (cpu_before, started) = (cpu(), Instant::now());
    wallet.start_sync_with(sink);
    {
        let mut done = synced.0.lock().unwrap();
        while !*done {
            done = synced.1.wait(done).unwrap();
        }
    }
    let sync_time = started.elapsed();
    let sync_cpu = cpu().saturating_sub(cpu_before);
    let peak = status_kib("VmHWM:");
    let (connections, requests, received) = traffic.take();
    let sync_written = written() - written_before;
    let sync_wakeups = runtime_wakeups() - wakeups_before;
    let tip = events.lock().unwrap().last().unwrap().tip;
    let outputs = wallet.history().len();
    let balance = wallet.balance();

    // Idle at the tip.
    let (written_before, wakeups_before, cpu_before) = (written(), runtime_wakeups(), cpu());
    let events_before = events.lock().unwrap().len();
    std::thread::sleep(Duration::from_secs(idle_secs));
    let (idle_connections, idle_requests, idle_received) = traffic.take();
    let idle_wakeups = runtime_wakeups() - wakeups_before;
    let idle_written = written() - written_before;
    let idle_cpu = cpu().saturating_sub(cpu_before);
    let idle_events = events.lock().unwrap().len() - events_before;
    wallet.stop_sync();

    // The files and what reading and writing them costs.
    let dir = crate::test_store::dir();
    let size =
        |ext: &str| std::fs::metadata(dir.join(format!("{id}.{ext}"))).map_or(0, |m| m.len());
    let state = wallet.inner.sync.snapshot();
    let bytes = state.to_bytes();
    let started = Instant::now();
    for _ in 0..10 {
        wallet
            .inner
            .with(|w| Ok(store()?.save_cache(w, &state.to_bytes())?))
            .unwrap();
    }
    let save_time = started.elapsed() / 10;
    let started = Instant::now();
    for _ in 0..10 {
        let cached = wallet
            .inner
            .with(|w| Ok(store()?.load_cache(w)?))
            .unwrap()
            .unwrap();
        kn_sync::SyncState::from_bytes(&cached).unwrap();
    }
    let load_time = started.elapsed() / 10;

    println!(
        "\n== bench: {tip} blocks, {} owned outputs, {outputs} history rows, balance {}",
        state.outputs.len(),
        balance.total
    );
    println!("unlock (Argon2id + decrypt)  {unlock_time:?}");
    println!(
        "full sync                    wall {sync_time:.2?}, cpu {sync_cpu:.2?}, peak RSS {} MiB (from {} MiB)",
        peak / 1024,
        rss_before / 1024
    );
    println!(
        "  network                    {requests} requests, {connections} connections, {:.1} MiB received",
        received as f64 / 1_048_576.0
    );
    println!(
        "  storage                    {:.1} MiB written, runtime wakeups {sync_wakeups}",
        sync_written as f64 / 1_048_576.0
    );
    println!(
        "idle {idle_secs}s                     {idle_requests} requests, {idle_connections} connections, {idle_received} B received, {idle_wakeups} wakeups, cpu {idle_cpu:.2?}, {idle_written} B written, {idle_events} events"
    );
    println!(
        "files                        wallet {} B, cache {} B (state JSON {} B)",
        size("knw"),
        size("knc"),
        bytes.len()
    );
    println!("cache save / load            {save_time:.2?} / {load_time:.2?}");
}
