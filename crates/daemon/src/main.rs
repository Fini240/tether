//! `tether` — the daemon binary. Runs as a host or as a client.

use tether_daemon::{clientmode, host};

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tether_core::config::{Config, Role};
use tether_net::Identity;
use tether_platform::BackendKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BackendArg {
    /// The real operating system.
    Native,
    /// Synthetic screens and logged input. For testing two roles on one box.
    Headless,
}

impl From<BackendArg> for BackendKind {
    fn from(arg: BackendArg) -> BackendKind {
        match arg {
            BackendArg::Native => BackendKind::Native,
            BackendArg::Headless => BackendKind::Headless,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "tether",
    version,
    about = "Share one keyboard and mouse across machines"
)]
struct Cli {
    /// Override the config file location.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Which platform backend to use.
    #[arg(long, global = true, value_enum, default_value_t = BackendArg::Native)]
    backend: BackendArg,

    #[arg(long, global = true, default_value = "info")]
    log: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run as the host: own the keyboard and mouse, drive everyone else.
    Host {
        /// Address to listen on.
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        #[arg(long)]
        port: Option<u16>,
        /// Accept machines that have never paired with this one. Leave it off
        /// once your machines are set up.
        #[arg(long)]
        pair: bool,
    },

    /// Run as a client: receive input from a host.
    Client {
        /// `host:port`. Omit to find a host over mDNS.
        #[arg(long)]
        host: Option<String>,
        /// Trust whichever host answers, on first connection only.
        #[arg(long)]
        pair: bool,
    },

    /// List hosts advertising themselves on this network.
    Discover {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },

    /// Print this machine's displays as the layout engine sees them.
    Screens,

    /// Print this machine's identity and pairing fingerprint.
    Id,

    /// Print the config file path and current contents.
    Config,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(&cli.log);

    // A current_thread runtime is enough: this program is entirely I/O bound
    // and the busiest path is a few thousand small frames a second.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;

    runtime.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let config_path = match &cli.config {
        Some(path) => path.clone(),
        None => Config::default_path().context("could not determine the config path")?,
    };
    let mut config = match Config::load(&config_path) {
        Ok(config) => config,
        Err(err) => {
            tracing::debug!(%err, "no usable config; starting from defaults");
            Config::default()
        }
    };

    let state_dir = config_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let identity = Identity::load_or_generate(&state_dir)
        .context("could not load or create this machine's identity")?;

    match cli.command {
        Command::Host { bind, port, pair } => {
            config.role = Role::Host;
            let port = port.unwrap_or(config.port);
            let backend = tether_platform::Backend::new(cli.backend.into())?;
            host::run(
                host::Options {
                    bind: format!("{bind}:{port}"),
                    pairing: pair,
                    config_path,
                    advertise: true,
                    ready: None,
                },
                config,
                identity,
                backend,
            )
            .await
        }

        Command::Client { host: addr, pair } => {
            config.role = Role::Client;
            let backend = tether_platform::Backend::new(cli.backend.into())?;
            clientmode::run(
                clientmode::Options {
                    address: addr.or_else(|| config.address.clone()),
                    pairing: pair,
                    config_path,
                },
                config,
                identity,
                backend,
            )
            .await
        }

        Command::Discover { seconds } => {
            let hosts = tether_net::discovery::browse(Duration::from_secs(seconds)).await?;
            if hosts.is_empty() {
                println!("No hosts found. Check that a host is running with `tether host`,");
                println!("that both machines are on the same subnet, and that mDNS (UDP 5353)");
                println!("is not blocked. You can always connect directly with --host IP:PORT.");
                return Ok(());
            }
            for found in hosts {
                let paired = found
                    .fingerprint
                    .as_deref()
                    .map(|fp| config.peer_by_fingerprint(fp).is_some())
                    .unwrap_or(false);
                println!(
                    "{}  {}  {}  {}",
                    found.socket_addr().unwrap_or_else(|| "?".into()),
                    found.name,
                    found.platform.as_deref().unwrap_or("?"),
                    if paired { "(paired)" } else { "(not paired)" }
                );
            }
            Ok(())
        }

        Command::Screens => {
            let backend = tether_platform::Backend::new(cli.backend.into())?;
            for monitor in backend.monitors.enumerate()? {
                println!(
                    "{:>4}  {:<24} {:>6}x{:<6} at ({:>6},{:>6})  scale {:.1}{}",
                    monitor.id.0,
                    monitor.name,
                    monitor.bounds.width,
                    monitor.bounds.height,
                    monitor.bounds.x,
                    monitor.bounds.y,
                    monitor.scale,
                    if monitor.primary { "  primary" } else { "" }
                );
            }
            Ok(())
        }

        Command::Id => {
            println!("machine id:  {}", identity.machine_id);
            println!("fingerprint: {}", identity.display_fingerprint());
            println!("state dir:   {}", state_dir.display());
            println!();
            println!("Compare this fingerprint on both machines when pairing.");
            Ok(())
        }

        Command::Config => {
            println!("path: {}", config_path.display());
            println!("{}", serde_json::to_string_pretty(&config)?);
            Ok(())
        }
    }
}

fn init_logging(filter: &str) {
    use tracing_subscriber::EnvFilter;

    // mdns-sd logs an ERROR every second for each interface it cannot send on.
    // On macOS that includes `nan0` (Apple Wireless Direct Link), which is
    // normally down — so a working host would print a scary error per second
    // forever. Our own code already reports advertise and browse failures with
    // context, so the library's own logging is suppressed by default.
    // `RUST_LOG=mdns_sd=debug` brings it back when debugging discovery.
    let default = format!("{filter},mdns_sd=off");

    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&default))
        .unwrap_or_else(|_| EnvFilter::new("info,mdns_sd=off"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();
}
