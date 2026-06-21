// justifies: INV-ALLOW-IS-DESIGN; refbat reference host intentionally emits machine-readable rendezvous lines and local status messages.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write as _;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use batpak::store::{Store, StoreConfig};
use clap::{Args, Parser, Subcommand};
use netbat::{serve_tcp_listener, IoTimeouts, ShutdownHandle, TcpServerConfig};

#[derive(Parser, Debug)]
#[command(
    name = "refbat",
    about = "Reference NETBAT/1 host for the batpak family. Not a product daemon.",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: RefbatCommand,
}

#[derive(Subcommand, Debug)]
enum RefbatCommand {
    /// Start a NETBAT/1 TCP listener with the bundled reference operations.
    Serve(ServeArgs),
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// Directory used as the BatPAK store. Store opening owns creation through
    /// the BatPAK platform filesystem boundary.
    #[arg(long, value_name = "PATH")]
    store: PathBuf,

    /// TCP socket address to bind. Use `127.0.0.1:0` to let the OS pick an
    /// ephemeral port and discover it via `--print-port`.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:0")]
    tcp: String,

    /// After binding, emit exactly one machine-readable ready line to
    /// stdout (prefix `REFBAT_READY ` followed by JSON), then enter the
    /// serve loop. Intended for CI rendezvous.
    #[arg(long)]
    print_port: bool,

    /// Allow binding to a non-loopback interface. Refused by default
    /// because this binary is a reference host, not a product daemon, and
    /// must not accidentally expose itself to the LAN.
    #[arg(long)]
    allow_non_loopback: bool,
}

fn main() -> Result<()> {
    install_tracing_subscriber();
    let cli = Cli::parse();
    match cli.command {
        RefbatCommand::Serve(args) => serve(&args),
    }
}

/// Install a fmt subscriber that respects `RUST_LOG`. Defaults to
/// `info` for syncbat/netbat/refbat when `RUST_LOG` is unset so live
/// operations always emit at least the dispatch + accept-loop spans.
fn install_tracing_subscriber() {
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::fmt;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,refbat=info,netbat=info,syncbat=info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}

#[tracing::instrument(name = "refbat.serve", skip_all, fields(store = %args.store.display(), tcp = %args.tcp))]
fn serve(args: &ServeArgs) -> Result<()> {
    let addr: SocketAddr = args
        .tcp
        .parse()
        .with_context(|| format!("parse --tcp address {:?}", args.tcp))?;

    if !args.allow_non_loopback && !is_loopback(addr.ip()) {
        return Err(anyhow!(
            "refbat refuses to bind to non-loopback address {addr}. Pass --allow-non-loopback to override (reference host only; not for production exposure)."
        ));
    }
    if args.allow_non_loopback && !is_loopback(addr.ip()) {
        eprintln!(
            "refbat: warning: binding non-loopback address {addr}; reference host only, not a product daemon"
        );
    }

    let store = Store::open(StoreConfig::new(&args.store)).context("open BatPAK store")?;
    let store = Arc::new(store);

    let mut core = build_core(&store)?;
    let listener =
        TcpListener::bind(addr).with_context(|| format!("bind TCP listener on {addr}"))?;
    let local_addr = listener
        .local_addr()
        .context("read local_addr from listener")?;

    if args.print_port {
        let ready = format!(
            "REFBAT_READY {{\"addr\":\"{}\",\"port\":{},\"protocol\":\"NETBAT/1\"}}\n",
            local_addr,
            local_addr.port()
        );
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(ready.as_bytes())
            .context("write REFBAT_READY line")?;
        stdout.flush().context("flush stdout after REFBAT_READY")?;
    }

    let shutdown = ShutdownHandle::new();
    install_signal_handler(&shutdown)?;

    // The reference host serves accepted connections sequentially on
    // the listener thread, so a single client that connects and
    // sends no bytes would otherwise block `read_line` indefinitely
    // and starve the accept loop — a trivial single-connection DoS.
    // Apply finite read/write timeouts so misbehaved peers can never
    // hold the listener.
    let config = TcpServerConfig::default().with_timeouts(
        IoTimeouts::default()
            .with_read(Some(Duration::from_secs(10)))
            .with_write(Some(Duration::from_secs(10))),
    );
    let stats = serve_tcp_listener(listener, &mut core, &config, &shutdown)
        .context("netbat::serve_tcp_listener")?;

    eprintln!(
        "refbat: shutdown — accepted={}, served={}, failed={}, shutdown_requested={}",
        stats.accepted_connections,
        stats.served_requests,
        stats.failed_requests,
        stats.shutdown_requested
    );
    Ok(())
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

fn build_core(store: &Arc<batpak::store::Store>) -> Result<syncbat::Core> {
    let mut builder = syncbat::Core::builder();
    builder
        .register(
            refbat::HEARTBEAT_DESCRIPTOR.clone(),
            refbat::HeartbeatHandler,
        )
        .map_err(|error| anyhow!("register system.heartbeat: {error}"))?;
    builder
        .register(
            refbat::BANK_COMMIT_DESCRIPTOR.clone(),
            refbat::BankCommitHandler {
                store: Arc::clone(store),
            },
        )
        .map_err(|error| anyhow!("register bank.commit: {error}"))?;
    builder
        .register(
            refbat::EVENT_GET_DESCRIPTOR.clone(),
            refbat::EventGetHandler {
                store: Arc::clone(store),
            },
        )
        .map_err(|error| anyhow!("register event.get: {error}"))?;
    builder
        .register(
            refbat::EVENT_QUERY_DESCRIPTOR.clone(),
            refbat::EventQueryHandler {
                store: Arc::clone(store),
            },
        )
        .map_err(|error| anyhow!("register event.query: {error}"))?;
    builder
        .register(
            refbat::RECEIPT_VERIFY_DESCRIPTOR.clone(),
            refbat::ReceiptVerifyHandler {
                store: Arc::clone(store),
            },
        )
        .map_err(|error| anyhow!("register receipt.verify: {error}"))?;
    builder
        .register(
            refbat::EVENT_WALK_DESCRIPTOR.clone(),
            refbat::EventWalkHandler {
                store: Arc::clone(store),
            },
        )
        .map_err(|error| anyhow!("register event.walk: {error}"))?;
    builder
        .register(
            refbat::EVIDENCE_CHAIN_WALK_DESCRIPTOR.clone(),
            refbat::ChainWalkEvidenceHandler {
                store: Arc::clone(store),
            },
        )
        .map_err(|error| anyhow!("register evidence.chain_walk: {error}"))?;
    builder
        .register(
            refbat::EVIDENCE_STORE_RESOURCE_DESCRIPTOR.clone(),
            refbat::StoreResourceEvidenceHandler {
                store: Arc::clone(store),
            },
        )
        .map_err(|error| anyhow!("register evidence.store_resource: {error}"))?;
    builder
        .register(
            refbat::EVIDENCE_READ_WALK_DESCRIPTOR.clone(),
            refbat::ReadWalkEvidenceHandler {
                store: Arc::clone(store),
            },
        )
        .map_err(|error| anyhow!("register evidence.read_walk: {error}"))?;
    // The reference host is domain-neutral, so it registers an empty projection
    // table: `evidence.projection_run` is advertised on the wire but resolves
    // every projection id to an `unknown projection` error. An embedder builds
    // its own host and calls `registry.register::<T>(..)` for each projection.
    let projection_registry = Arc::new(batpak::store::ProjectionEvidenceRegistry::new());
    builder
        .register(
            refbat::EVIDENCE_PROJECTION_RUN_DESCRIPTOR.clone(),
            refbat::ProjectionRunEvidenceHandler {
                store: Arc::clone(store),
                registry: Arc::clone(&projection_registry),
            },
        )
        .map_err(|error| anyhow!("register evidence.projection_run: {error}"))?;
    builder
        .build()
        .map_err(|error| anyhow!("build syncbat core: {error}"))
}

fn install_signal_handler(shutdown: &ShutdownHandle) -> Result<()> {
    let handle = shutdown.clone();
    ctrlc::set_handler(move || {
        handle.shutdown();
    })
    .context("install ctrlc handler")?;
    Ok(())
}
