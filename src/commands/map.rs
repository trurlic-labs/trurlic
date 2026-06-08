use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::Result;
use crate::store::STATE_DIR;
use crate::store::graph::Severity;

use super::discover_store;

/// Timeout waiting for the detached server to report its URL.
const DETACH_TIMEOUT: Duration = Duration::from_secs(10);

pub fn map(cwd: &Path, port: Option<u16>, no_open: bool, detach: bool) -> Result<()> {
    let store = discover_store(cwd)?;

    if detach {
        return detach_server(&store, port);
    }

    let state = store.load_state()?;

    let errors = state
        .validate()
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();
    if errors > 0 {
        eprintln!("warning: .trurl/ has {errors} consistency issue(s) — run `trurl check`");
    }

    eprintln!(
        "trurl: map for {} ({} components, {} decisions, {} patterns)",
        state.project.project.name,
        state.components.len(),
        state.decisions.len(),
        state.patterns.len(),
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| crate::Error::Io(std::io::Error::other(e)))?;

    rt.block_on(crate::map::start(store, state, port, no_open))
}

/// Spawn the map server as a detached child process.
///
/// Binds a port first to avoid TOCTOU races, then re-execs with that
/// port pinned. The child's stderr goes to `.trurl/.state/map.log`;
/// the parent polls the log until the URL appears, prints it, and exits.
fn detach_server(store: &crate::store::Store, port: Option<u16>) -> Result<()> {
    use std::fs;
    use std::process::{Command, Stdio};

    // Bind to discover the actual port, then release so the child can bind.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port.unwrap_or(0)));
    let listener = TcpListener::bind(addr).map_err(|e| {
        crate::Error::Io(std::io::Error::new(
            e.kind(),
            format!("failed to bind {addr}: {e}"),
        ))
    })?;
    let actual_port = listener.local_addr().map_err(crate::Error::Io)?.port();
    drop(listener);

    // Prepare log file.
    let log_dir = store.root().join(STATE_DIR);
    fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join("map.log");
    let log_file = fs::File::create(&log_path)?;

    let exe = std::env::current_exe().map_err(crate::Error::Io)?;
    let child = Command::new(exe)
        .arg("map")
        .arg("--port")
        .arg(actual_port.to_string())
        .arg("--no-open")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()
        .map_err(crate::Error::Io)?;

    // Poll the log file for the URL line.
    let start = Instant::now();
    let url = loop {
        if start.elapsed() > DETACH_TIMEOUT {
            return Err(crate::Error::Validation(
                "timed out waiting for map server to start".into(),
            ));
        }

        if let Ok(content) = fs::read_to_string(&log_path)
            && let Some(line) = content.lines().find(|l| l.contains("map \u{2192}")) {
                break line.to_string();
            }

        std::thread::sleep(Duration::from_millis(50));
    };

    println!("{url}");
    println!("pid: {}", child.id());
    println!("log: {}", log_path.display());

    // Intentionally leak the Child handle so the child process is not
    // waited on (and therefore not killed) when the parent exits.
    std::mem::forget(child);

    Ok(())
}
