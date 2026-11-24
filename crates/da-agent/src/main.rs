//! `da-agent`: command-line front end for the DisplayAnything core.
//! The tray app uses the same core; this binary is for headless machines,
//! scripting and testing.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use da_core::{discovery, AutoConnector, Config, Event, Identity, Node, TrustStore, DEFAULT_PORT};
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(name = "da-agent", version, about = "DisplayAnything LAN agent")]
struct Cli {
    /// Where the identity and trusted peers are stored.
    #[arg(long, global = true, env = "DA_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Name other devices see. Defaults to this computer's hostname.
    #[arg(long, global = true, env = "DA_NAME")]
    name: Option<String>,
    /// Accept or dial non-private addresses (not recommended).
    #[arg(long, global = true)]
    allow_public: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show this device's name and fingerprint.
    Id,
    /// Listen for pairing requests and sessions.
    Serve {
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Address to listen on.
        #[arg(long, default_value = "0.0.0.0")]
        bind: IpAddr,
        /// Do not advertise this device over mDNS.
        #[arg(long)]
        no_discovery: bool,
        /// Do not reconnect to saved peers automatically.
        #[arg(long)]
        no_auto_connect: bool,
    },
    /// Pair with a device by IP. You'll be asked for the code it shows.
    Pair { addr: String },
    /// Open a session with a paired device and measure latency.
    Connect {
        addr: String,
        #[arg(long, default_value_t = 3)]
        pings: u32,
    },
    /// List paired devices.
    Peers,
    /// Forget a paired device, by name or fingerprint prefix.
    Unpair { who: String },
    /// Look for DisplayAnything devices on the LAN.
    Discover {
        #[arg(long, default_value_t = 3)]
        secs: u64,
    },
}

fn default_data_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "DisplayAnything")
        .context("could not find a data directory for this user")?;
    Ok(dirs.data_dir().to_path_buf())
}

fn default_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "DisplayAnything device".into())
}

/// Accept `192.168.1.20`, `192.168.1.20:47800`, `fe80::1` or `[fe80::1]:47800`.
fn parse_addr(s: &str) -> Result<SocketAddr> {
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    if let Ok(ip) = s.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT));
    }
    bail!("'{s}' is not an IP address (try 192.168.1.20 or 192.168.1.20:{DEFAULT_PORT})")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let data_dir = match cli.data_dir {
        Some(d) => d,
        None => default_data_dir()?,
    };
    // The device name is remembered, so `--name` only needs to be given once.
    let name_file = data_dir.join("device_name.txt");
    let name = match cli.name {
        Some(n) => {
            std::fs::create_dir_all(&data_dir)?;
            std::fs::write(&name_file, &n)?;
            n
        }
        None => std::fs::read_to_string(&name_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(default_name),
    };
    let identity = Identity::load_or_create(&data_dir, &name)?;
    let trust = TrustStore::load(&data_dir)?;
    let config = Config {
        allow_public_addresses: cli.allow_public,
        ..Config::default()
    };
    let node = Node::new(identity, trust, config)?;

    match cli.cmd {
        Cmd::Id => {
            println!("Name:        {}", node.identity().name);
            println!("Fingerprint: {}", node.identity().fingerprint().short());
            println!("Full:        {}", node.identity().fingerprint());
            println!("Data dir:    {}", data_dir.display());
        }
        Cmd::Serve {
            port,
            bind,
            no_discovery,
            no_auto_connect,
        } => serve(node, bind, port, no_discovery, no_auto_connect).await?,
        Cmd::Pair { addr } => {
            let addr = parse_addr(&addr)?;
            println!("Asking {addr} to show a pairing code...");
            let peer = node
                .pair(addr, || async {
                    tokio::task::spawn_blocking(|| {
                        print!("Enter the 6-digit code shown on the other device: ");
                        std::io::stdout().flush()?;
                        let mut line = String::new();
                        std::io::stdin().read_line(&mut line)?;
                        Ok(line)
                    })
                    .await?
                })
                .await?;
            println!(
                "Paired with {} ({}).",
                peer.name,
                peer.fingerprint().map(|f| f.short()).unwrap_or_default()
            );
        }
        Cmd::Connect { addr, pings } => {
            let addr = parse_addr(&addr)?;
            let mut s = node.connect(addr).await?;
            println!(
                "Connected to {}. They allow: {:?}",
                s.peer.name, s.granted_to_us
            );
            for _ in 0..pings {
                let rtt = s.ping().await?;
                println!("  ping {:.2} ms", rtt.as_secs_f64() * 1000.0);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            s.close().await?;
        }
        Cmd::Peers => {
            let trust = node.trust();
            if trust.peers().is_empty() {
                println!("No paired devices. Run `da-agent pair <ip>` to add one.");
            }
            for p in trust.peers() {
                let fp = p
                    .fingerprint()
                    .map(|f| f.short())
                    .unwrap_or_else(|| "?".into());
                let perms = &p.permissions;
                println!(
                    "{:<24} {}  last seen {:<16} auto-connect:{} remote-control:{} display:{} input:{}",
                    p.name,
                    fp,
                    p.last_addr.as_deref().unwrap_or("-"),
                    p.auto_connect,
                    perms.remote_control,
                    perms.display_takeover,
                    perms.input_takeover
                );
            }
        }
        Cmd::Unpair { who } => {
            let n = node.trust().remove(&who)?;
            if n == 0 {
                bail!("no paired device matches '{who}'");
            }
            println!("Removed {n} device(s).");
        }
        Cmd::Discover { secs } => {
            println!("Listening for {secs}s...");
            let found = discovery::browse(Duration::from_secs(secs)).await?;
            if found.is_empty() {
                println!("Nothing found. You can still pair by IP.");
            }
            for f in found {
                let addrs: Vec<String> = f.addrs.iter().map(|a| a.to_string()).collect();
                println!("{:<24} {}:{}", f.name, addrs.join(", "), f.port);
            }
        }
    }
    Ok(())
}

async fn serve(
    node: Arc<Node>,
    bind: IpAddr,
    port: u16,
    no_discovery: bool,
    no_auto_connect: bool,
) -> Result<()> {
    let listener = TcpListener::bind(SocketAddr::new(bind, port))
        .await
        .with_context(|| format!("could not listen on port {port}"))?;
    let _adv = if no_discovery {
        None
    } else {
        match discovery::advertise(&node.identity().name, port, &node.identity().fingerprint()) {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("mDNS discovery unavailable ({e:#}); pairing by IP still works.");
                None
            }
        }
    };
    let mut events = node.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = events.recv().await {
            match ev {
                Event::Listening { addr } => println!("Listening on {addr}"),
                Event::PairCodeShown {
                    peer_name,
                    peer_addr,
                    code,
                    expires_in,
                } => {
                    println!();
                    println!("  {peer_name} ({}) wants to pair.", peer_addr.ip());
                    println!("  Pairing code:  {} {}", &code[..3], &code[3..]);
                    println!(
                        "  Type it on that device within {} seconds.",
                        expires_in.as_secs()
                    );
                    println!();
                }
                Event::Paired {
                    name,
                    fingerprint,
                    addr,
                } => {
                    println!(
                        "Paired with {name} at {} ({})",
                        addr.ip(),
                        fingerprint.short()
                    )
                }
                Event::PairFailed { peer_addr, reason } => {
                    println!("Pairing from {} failed: {reason}", peer_addr.ip())
                }
                Event::SessionOpened { name, addr, .. } => {
                    println!("Session opened: {name} ({})", addr.ip())
                }
                Event::SessionClosed { name, .. } => println!("Session closed: {name}"),
                Event::Rejected { addr, reason } => println!("Rejected {}: {reason}", addr.ip()),
                Event::PeerOnline { name, addr, .. } => {
                    println!("{name} is online at {}", addr.ip())
                }
                Event::PeerOffline { name, .. } => {
                    println!("{name} went offline; will reconnect automatically")
                }
            }
        }
    });
    if !no_auto_connect {
        tokio::spawn(AutoConnector::new(node.clone(), !no_discovery).run());
    }
    println!(
        "{} is ready. Fingerprint {}. Press Ctrl+C to stop.",
        node.identity().name,
        node.identity().fingerprint().short()
    );
    tokio::select! {
        r = node.clone().serve(listener) => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
