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

    /// Show or change where machines sit relative to each other.
    ///
    /// With no arguments, prints the current arrangement.
    Layout {
        /// Machine to move, by name or id prefix.
        machine: Option<String>,
        /// Where to put it: left, right, above or below.
        side: Option<String>,
        /// The machine to place it against.
        anchor: Option<String>,
    },

    /// Show or set this machine's name, as other machines see it.
    Name {
        /// The new name. Omit to print the current one.
        name: Option<String>,
    },

    /// Check that this machine can capture, inject, and tell its own injected
    /// events apart from real ones.
    Doctor,
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

        Command::Layout {
            machine,
            side,
            anchor,
        } => {
            use tether_core::layout::Side;

            let given = machine.is_some();
            if let (Some(machine), Some(side), Some(anchor)) = (machine, side, anchor) {
                let target = config
                    .layout
                    .resolve(&machine)
                    .map_err(anyhow::Error::msg)?;
                let anchor_id = config.layout.resolve(&anchor).map_err(anyhow::Error::msg)?;
                let side: Side = side.parse().map_err(anyhow::Error::msg)?;

                config
                    .layout
                    .place_relative(target, side, anchor_id)
                    .map_err(anyhow::Error::msg)?;
                config.save(&config_path)?;

                let name = |id| {
                    config
                        .layout
                        .get(id)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| id.to_string())
                };
                println!("{} is now {} {}", name(target), side, name(anchor_id));
                println!();
            } else if given {
                anyhow::bail!(
                    "usage: tether layout <machine> <left|right|above|below> <other-machine>\n\
                     for example: tether layout mac left pc"
                );
            }

            print_layout(&config, identity.machine_id);
            Ok(())
        }

        Command::Name { name } => {
            match name {
                Some(name) if name.trim().is_empty() => {
                    anyhow::bail!("a machine name cannot be empty")
                }
                Some(name) => {
                    let previous = config.name.clone();
                    config.name = name.trim().to_string();
                    // Keep the canvas entry in step, or `tether layout` would
                    // still be showing the old name for this machine.
                    if let Some(placement) = config.layout.get_mut(identity.machine_id) {
                        placement.name = config.name.clone();
                    }
                    config.save(&config_path)?;
                    println!("renamed {previous:?} to {:?}", config.name);
                    println!("Restart tether on this machine, and on its peers, for it to show.");
                }
                None => println!("{}", config.name),
            }
            Ok(())
        }

        Command::Doctor => run_doctor(cli.backend.into()),

        Command::Config => {
            println!("path: {}", config_path.display());
            println!("{}", serde_json::to_string_pretty(&config)?);
            Ok(())
        }
    }
}

/// Print the canvas as a table, plus which machine each edge leads to.
fn print_layout(config: &Config, this: tether_core::layout::MachineId) {
    if config.layout.machines.is_empty() {
        println!("No machines on the canvas yet.");
        println!("Connect a client once — it is added automatically — then run this again.");
        return;
    }

    println!("{:<18} {:<10} {:>20}", "MACHINE", "ID", "POSITION");
    let mut machines: Vec<_> = config.layout.machines.iter().collect();
    machines.sort_by_key(|p| (p.global_bounds().x, p.global_bounds().y));

    for placement in &machines {
        let b = placement.global_bounds();
        // The id prefix is shown because names are not guaranteed unique —
        // two machines can genuinely share a hostname, and then the prefix is
        // the only way to say which one you mean.
        println!(
            "{:<18} {:<10} {:>6},{:<6} {:>5}x{:<6}  {}{}",
            placement.name,
            &placement.machine.to_string()[..8],
            b.x,
            b.y,
            b.width,
            b.height,
            placement.platform,
            if placement.machine == this {
                "  <- this machine"
            } else {
                ""
            }
        );
    }

    // Spell the edges out. A coordinate table is precise but does not answer
    // the question people actually have, which is "which way do I move?".
    println!();
    for placement in &machines {
        if placement.machine != this {
            continue;
        }
        let mine = placement.global_bounds();
        for other in &machines {
            if other.machine == this {
                continue;
            }
            let theirs = other.global_bounds();
            let direction = if theirs.right() <= mine.left() {
                "left"
            } else if theirs.left() >= mine.right() {
                "right"
            } else if theirs.bottom() <= mine.top() {
                "up"
            } else if theirs.top() >= mine.bottom() {
                "down"
            } else {
                "overlapping — fix this, the cursor will behave oddly"
            };
            println!("move {:<8} to reach {}", direction, other.name);
        }
    }

    println!();
    println!("Change it with:  tether layout <machine> <left|right|above|below> <other>");
}

/// Prove the pieces input handoff depends on, on this machine, right now.
fn run_doctor(kind: BackendKind) -> Result<()> {
    use std::time::Duration;
    use tether_proto::InputEvent;

    println!("Platform            {}", tether_proto::Platform::current());

    match tether_platform::check_capture_permission() {
        Ok(()) => println!("Input permission    granted"),
        Err(err) => {
            println!("Input permission    DENIED");
            println!("                    {err}");
        }
    }

    let mut backend = tether_platform::Backend::new(kind)?;

    match backend.monitors.enumerate() {
        Ok(monitors) => println!("Displays            {} found", monitors.len()),
        Err(err) => println!("Displays            FAILED: {err}"),
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    match backend.capture.start(tx) {
        Ok(()) => println!("Input capture       started"),
        Err(err) => {
            println!("Input capture       FAILED: {err}");
            return Ok(());
        }
    }

    // The load-bearing check. Inject events into ourselves and confirm the
    // capture layer recognises them as ours: if it reports them as user input
    // instead, two machines driving each other would fight over control
    // forever, each seeing the other's injections as somebody typing.
    let before = backend.capture.injected_filtered();
    let at = backend
        .pointer
        .position()
        .unwrap_or(tether_proto::Point::new(400, 400));
    for i in 0..5 {
        let _ = backend.inject.inject(&InputEvent::MouseMove {
            x: at.x + i,
            y: at.y,
        });
    }
    std::thread::sleep(Duration::from_millis(400));

    let filtered = backend.capture.injected_filtered() - before;
    let leaked = std::iter::from_fn(|| rx.try_recv().ok()).count();

    if kind == BackendKind::Headless {
        println!("Injection marking   n/a (headless backend injects nowhere)");
    } else if filtered >= 5 && leaked == 0 {
        println!("Injection marking   working ({filtered} of our own events filtered)");
        println!();
        println!("Automatic input handoff is safe to use on this machine.");
    } else {
        println!("Injection marking   BROKEN ({filtered} filtered, {leaked} leaked back)");
        println!();
        println!("Turn off auto_input_handoff in the config: without this filter,");
        println!("two machines would each read the other's injected events as a");
        println!("local touch and fight over which one is driving.");
    }

    backend.capture.stop();
    Ok(())
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
