// SPDX-License-Identifier: GPL-3.0-only
//! `illum`: one binary, every role (frontend, service, backend, control).
//!
//! M1 runs in a console: `illum --service` hosts, `illum connect <ip>` dials. The Windows
//! service, the backend and the app window arrive in later milestones.

mod prompt;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use illum_core::Role;
use illum_core::protocol::DEFAULT_PORT;
use illum_core::store::TrustStore;
use illum_net::session::{accept, connect, normalize_target, serve_host_session};
use illum_net::{Endpoint, Identity, Listener, Timeouts, bind};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug, Parser)]
#[command(
    name = "illum",
    version,
    about = "Illumination: direct, IP-only remote desktop"
)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// Folder for this device's key and trust store.
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,

    /// Show debug logging.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Run as the host service: listen for incoming connections.
    #[arg(long, group = "role")]
    service: bool,

    /// Per-session worker, started by the service (not implemented yet).
    #[arg(long, group = "role")]
    backend: bool,

    /// Per-user helper for clipboard and tray, started by the service (not implemented yet).
    #[arg(long, group = "role")]
    control: bool,

    /// Port to listen on (with --service).
    #[arg(long, default_value_t = DEFAULT_PORT, requires = "service")]
    port: u16,

    /// Listen on this address instead of the Tailscale addresses (repeatable).
    /// Use it to opt in to a LAN address, or 127.0.0.1 for local testing.
    #[arg(long = "bind", value_name = "IP", requires = "service")]
    bind: Vec<IpAddr>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Connect to a host by IP or name (port defaults to 7171) and measure round-trip time.
    Connect {
        /// e.g. 100.100.101.1, laptop, or laptop.tail1234.ts.net:7171
        target: String,
        /// How many pings to send before disconnecting.
        #[arg(long, default_value_t = 10)]
        pings: u32,
    },
    /// Show this device's key fingerprint.
    Fingerprint,
    /// List pinned hosts and paired devices.
    List,
    /// Remove the pinned key for a host address (after the host was reinstalled).
    Forget {
        /// The address exactly as you connected to it, e.g. 100.100.101.1:7171
        address: String,
    },
}

impl Cli {
    fn role(&self) -> Role {
        if self.service {
            Role::Service
        } else if self.backend {
            Role::Backend
        } else if self.control {
            Role::Control
        } else {
            Role::Frontend
        }
    }
}

/// Device-wide folder, shared by every role (AnyDesk keeps its key in ProgramData too).
fn default_home() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(|p| PathBuf::from(p).join("Illumination"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".illumination")))
        .unwrap_or_else(|| PathBuf::from(".illumination"))
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown device".into())
}

fn endpoint(home: &Path) -> Result<Endpoint> {
    let identity = Identity::load_or_create(home).context("loading the device key")?;
    let store = TrustStore::load(home.join("trust.json")).context("loading the trust store")?;
    Ok(Endpoint {
        identity: Arc::new(identity),
        store: Arc::new(Mutex::new(store)),
        device_name: device_name(),
        timeouts: Timeouts::default(),
        prompt: prompt::console_prompt(),
    })
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_target(true)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_max_level(if cli.verbose {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .init();
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("illum: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let home = cli.home.clone().unwrap_or_else(default_home);
    let role = cli.role();
    match (role, cli.command) {
        (Role::Service, _) => run_service(&home, cli.port, &cli.bind).await,
        (Role::Backend | Role::Control, _) => bail!("role {role:?} is not implemented yet"),
        (Role::Frontend, None) => {
            println!("The app window isn't built yet. Try `illum connect <ip>` or `illum --help`.");
            Ok(())
        }
        (Role::Frontend, Some(Command::Connect { target, pings })) => {
            run_connect(&home, &target, pings).await
        }
        (Role::Frontend, Some(Command::Fingerprint)) => {
            let ep = endpoint(&home)?;
            println!("{}", ep.identity.fingerprint);
            Ok(())
        }
        (Role::Frontend, Some(Command::List)) => {
            let ep = endpoint(&home)?;
            let store = ep.store.lock().await;
            println!("Pinned hosts (this device connects to):");
            for (address, host) in store.known_hosts() {
                println!(
                    "  {address:<28} {:<20} {}",
                    host.name,
                    &host.fingerprint[..16]
                );
            }
            println!("Paired devices (may connect to this device):");
            for (fingerprint, peer) in store.paired_peers() {
                println!(
                    "  {:<20} last from {:<24} {}",
                    peer.name,
                    peer.last_address,
                    &fingerprint[..16]
                );
            }
            Ok(())
        }
        (Role::Frontend, Some(Command::Forget { address })) => {
            let ep = endpoint(&home)?;
            let address = normalize_target(&address);
            if ep.store.lock().await.forget_host(&address)? {
                println!("Forgot the pinned key for {address}.");
            } else {
                println!("No pinned key for {address}.");
            }
            Ok(())
        }
    }
}

async fn run_service(home: &Path, port: u16, bind_to: &[IpAddr]) -> Result<()> {
    let ep = endpoint(home)?;
    let addrs = if bind_to.is_empty() {
        bind::tailscale_addresses()
    } else {
        bind_to.to_vec()
    };
    if addrs.is_empty() {
        bail!(
            "no Tailscale address found on this machine. Start Tailscale, or listen elsewhere with --bind <IP>"
        );
    }
    let mut listener = Listener::bind(&ep, &addrs, port)
        .await
        .context("opening the listening port")?;
    println!(
        "Device \"{}\" fingerprint: {}",
        ep.device_name, ep.identity.fingerprint
    );
    for addr in listener.local_addrs() {
        println!("Listening on {addr}");
    }
    while let Some((tcp, peer)) = listener.next().await {
        let ep = ep.clone();
        let acceptor = listener.acceptor();
        tokio::spawn(async move {
            tracing::info!(target: "service", "[{peer}] Incoming connection.");
            match accept(&ep, &acceptor, tcp, peer).await {
                Ok(conn) => {
                    let name = conn
                        .peer_hello
                        .as_ref()
                        .map(|h| h.device_name.clone())
                        .unwrap_or_default();
                    tracing::info!(target: "service", "[{peer}] Session with \"{name}\" started.");
                    match serve_host_session(conn, ep.timeouts.idle).await {
                        Ok(()) => tracing::info!(target: "service", "[{peer}] Session ended."),
                        Err(err) => {
                            tracing::warn!(target: "service", "[{peer}] Session ended: {err}")
                        }
                    }
                }
                Err(err) => tracing::warn!(target: "service", "[{peer}] Not connected: {err}"),
            }
        });
    }
    Ok(())
}

async fn run_connect(home: &Path, target: &str, pings: u32) -> Result<()> {
    let ep = endpoint(home)?;
    println!("This device's fingerprint: {}", ep.identity.fingerprint);
    println!("Connecting to {}…", normalize_target(target));
    let mut conn = connect(&ep, target).await?;
    let name = conn
        .peer_hello
        .as_ref()
        .map(|h| h.device_name.clone())
        .unwrap_or_default();
    println!(
        "Connected to \"{name}\" at {} (key {}).",
        conn.peer_address, conn.peer_fingerprint
    );

    let mut samples = Vec::new();
    for seq in 1..=u64::from(pings) {
        let rtt = conn.ping(seq, Duration::from_secs(5)).await?;
        println!("  ping {seq}: {:.2} ms", rtt.as_secs_f64() * 1000.0);
        samples.push(rtt.as_secs_f64() * 1000.0);
        if seq < u64::from(pings) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    if !samples.is_empty() {
        let min = samples.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = samples.iter().cloned().fold(0.0, f64::max);
        let avg = samples.iter().sum::<f64>() / samples.len() as f64;
        println!("RTT min/avg/max: {min:.2} / {avg:.2} / {max:.2} ms");
    }
    conn.close(Duration::from_secs(1)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("illum").chain(args.iter().copied()))
    }

    #[test]
    fn no_flag_is_frontend() {
        assert_eq!(parse(&[]).unwrap().role(), Role::Frontend);
    }

    #[test]
    fn role_flags() {
        assert_eq!(parse(&["--service"]).unwrap().role(), Role::Service);
        assert_eq!(parse(&["--backend"]).unwrap().role(), Role::Backend);
        assert_eq!(parse(&["--control"]).unwrap().role(), Role::Control);
    }

    #[test]
    fn roles_are_exclusive_and_unknown_flags_rejected() {
        assert!(parse(&["--service", "--backend"]).is_err());
        assert!(parse(&["--nope"]).is_err());
    }

    #[test]
    fn service_options_need_service() {
        assert!(parse(&["--port", "9000"]).is_err());
        let cli = parse(&["--service", "--port", "9000", "--bind", "127.0.0.1"]).unwrap();
        assert_eq!(cli.port, 9000);
        assert_eq!(cli.bind, vec!["127.0.0.1".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn connect_subcommand() {
        let cli = parse(&["connect", "100.100.101.1", "--pings", "3"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Connect { ref target, pings: 3 }) if target == "100.100.101.1")
        );
    }
}
