//! Working out who arbitrates, so that nobody has to be told.
//!
//! One shared pointer needs one machine holding the canonical answer to where
//! it is. That is not a preference and it cannot be designed away: two
//! machines both believing they are driving is a torn cursor. What *can* go
//! away is the part where a person decides which machine that is, writes it
//! into a config file on both ends, and keeps it in step forever after.
//!
//! So the arbiter is elected rather than configured. Every machine advertises
//! its id over mDNS; the lowest id arbitrates and everyone else connects to
//! it. There is no negotiation, no handshake and no tie to break, because
//! comparing two numbers gives both machines the same answer independently —
//! including when they are switched on at the same instant and each sees the
//! other mid-decision.
//!
//! From the outside the roles are invisible. Input already flows in both
//! directions regardless of which machine arbitrates: whichever keyboard you
//! touch takes over, and the pointer crosses either way. Switch a machine on
//! and it joins; switch it off and the rest carry on.
//!
//! Two rules keep it from thrashing:
//!
//! * A machine only stands down while **no client is connected to it**. Once
//!   somebody is relying on it, being outranked stops being worth a dropped
//!   session; it hands over the next time it is idle.
//! * A machine that finds **nothing at all** takes the job rather than waiting
//!   for a peer that may be switched off for the night.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use tether_core::config::Config;
use tether_net::discovery;
use tether_net::Identity;
use tether_platform::Backend;

use crate::control::{Command, DaemonControl, StatusHandle};
use crate::{clientmode, host, Outcome};

/// How long to listen before concluding a machine is on its own.
///
/// mDNS answers arrive in milliseconds on a quiet LAN and in a second or so on
/// a busy one. Long enough not to declare solitude prematurely on startup,
/// short enough that switching a machine on still feels immediate.
const LOOK_FOR: Duration = Duration::from_secs(2);

/// How often an arbiter checks whether it should still be the one.
///
/// Only matters when two machines start close together and each briefly
/// believes it is alone. Once settled this finds nothing, forever, so it is
/// deliberately unhurried.
const RECHECK_EVERY: Duration = Duration::from_secs(5);

pub struct Options {
    pub bind: String,
    pub port: u16,
    pub pairing: bool,
    pub config_path: PathBuf,
    pub control: Option<DaemonControl>,
}

/// Keeps the one UI attached across however many roles this machine plays.
///
/// The status half clones freely, so each stint gets its own handle onto the
/// same snapshot. The command half deliberately does not — one receiver, one
/// consumer — so the receiver stays here and commands are forwarded into
/// whichever role is currently running. Without this, stopping from the window
/// would work until the first time the network changed its mind, and then
/// silently stop working.
struct Relay {
    status: Option<StatusHandle>,
    current: std::sync::Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Command>>>>,
}

impl Relay {
    fn new(control: Option<DaemonControl>) -> Self {
        let current: std::sync::Arc<
            std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Command>>>,
        > = Default::default();

        let status = control.map(|control| {
            let sink = std::sync::Arc::clone(&current);
            let mut commands = control.commands;
            tokio::spawn(async move {
                while let Some(command) = commands.recv().await {
                    let target = sink.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    match target {
                        Some(tx) => {
                            let _ = tx.send(command);
                        }
                        // Between roles. Dropping a command here beats queuing
                        // it for a role that may never start.
                        None => tracing::debug!("no role running; command dropped"),
                    }
                }
            });
            control.status
        });

        Self { status, current }
    }

    /// A control handle for one stint of one role.
    fn attach(&self) -> Option<DaemonControl> {
        let status = self.status.clone()?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        Some(DaemonControl {
            status,
            commands: rx,
        })
    }
}

/// What this machine should do about the network it just looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Election {
    /// Nobody outranks us. Listen, and let the others come.
    Arbitrate,
    /// Somebody does. Their name, purely for the log — a machine id is the
    /// right thing to decide on and the wrong thing to read.
    Follow(String),
}

/// Run whichever role the network calls for, and keep running the right one.
pub async fn run(
    mut options: Options,
    config: Config,
    identity: Identity,
    backend: &mut Backend,
) -> Result<()> {
    let relay = Relay::new(options.control.take());

    loop {
        let outcome = match elect(&identity).await {
            Election::Arbitrate => {
                tracing::info!("no other machine outranks this one; arbitrating");
                arbitrate(&options, &relay, &config, &identity, backend).await?
            }
            Election::Follow(peer) => {
                tracing::info!(%peer, "another machine is arbitrating; joining it");
                follow(&options, &relay, &config, &identity, backend).await?
            }
        };

        match outcome {
            Outcome::Stopped => return Ok(()),
            // The other role now. Straight round the loop rather than pausing:
            // the machine is unusable across the gap, and the gap is the whole
            // cost of getting this wrong.
            Outcome::Supersede => continue,
        }
    }
}

/// Look at who else is out there and decide.
///
/// Our own advertisement is not in the way here: a machine only advertises
/// while it is arbitrating, and one that is arbitrating is not calling this.
/// Filtering on our own id anyway costs nothing and means a stray echo of
/// ourselves can never be read as a rival.
async fn elect(identity: &Identity) -> Election {
    let mine = identity.machine_id.0;
    let peers = match discovery::browse(LOOK_FOR).await {
        Ok(peers) => peers,
        Err(err) => {
            // No mDNS is not no network. Take the job: a machine nobody can
            // find is still a machine somebody may connect to by address.
            tracing::warn!(%err, "could not browse for other machines; arbitrating");
            return Election::Arbitrate;
        }
    };

    match strongest_claim(&peers, mine) {
        Some(peer) => Election::Follow(peer.name.clone()),
        None => Election::Arbitrate,
    }
}

/// The id of the peer that should be arbitrating instead of us, if any.
///
/// Split out from [`elect`] so the rule is testable without a network: the
/// lowest id wins, and a peer too old to advertise an id is deferred to rather
/// than argued with — it cannot participate, so the only way two such machines
/// agree is for the one that *can* choose to give way.
fn strongest_claim(
    peers: &[discovery::DiscoveredHost],
    mine: u64,
) -> Option<&discovery::DiscoveredHost> {
    peers
        .iter()
        .filter(|peer| peer.machine_id != Some(mine))
        .filter(|peer| peer.machine_id.is_none_or(|id| id < mine))
        .min_by_key(|peer| peer.machine_id.unwrap_or(0))
}

/// Be the arbiter until somebody better turns up, or the user stops us.
async fn arbitrate(
    options: &Options,
    relay: &Relay,
    config: &Config,
    identity: &Identity,
    backend: &mut Backend,
) -> Result<Outcome> {
    let (supersede, watcher) = watch_for_stronger(identity.machine_id.0);

    let outcome = host::run(
        host::Options {
            bind: format!("{}:{}", options.bind, options.port),
            pairing: options.pairing,
            config_path: options.config_path.clone(),
            advertise: true,
            ready: None,
            control: relay.attach(),
            supersede: Some(supersede),
        },
        config.clone(),
        identity.clone(),
        backend,
    )
    .await;

    watcher.abort();
    outcome
}

/// Follow whoever is arbitrating, until they go away and nobody replaces them.
async fn follow(
    options: &Options,
    relay: &Relay,
    config: &Config,
    identity: &Identity,
    backend: &mut Backend,
) -> Result<Outcome> {
    clientmode::run(
        clientmode::Options {
            // Deliberately not the address we just found: the client does its
            // own discovery, and by the time it looks the picture may have
            // changed. Handing it a stale address would only make it fail
            // slower.
            address: None,
            pairing: options.pairing,
            config_path: options.config_path.clone(),
            control: relay.attach(),
            auto: true,
        },
        config.clone(),
        identity.clone(),
        backend,
    )
    .await
}

/// Watch for a machine that should be arbitrating instead of us.
///
/// Fires once and stops. Standing down is a one-way trip for this stint of the
/// role — the supervisor loops round, re-runs the election from scratch, and
/// starts a fresh watcher if it ends up arbitrating again.
fn watch_for_stronger(
    mine: u64,
) -> (
    tokio::sync::watch::Receiver<bool>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(RECHECK_EVERY).await;
            let peers = match discovery::browse(LOOK_FOR).await {
                Ok(peers) => peers,
                Err(err) => {
                    tracing::debug!(%err, "could not re-check who should arbitrate");
                    continue;
                }
            };
            if let Some(peer) = strongest_claim(&peers, mine) {
                tracing::info!(peer = %peer.name, "a machine that outranks this one has appeared");
                let _ = tx.send(true);
                return;
            }
        }
    });
    (rx, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(machine_id: Option<u64>) -> discovery::DiscoveredHost {
        discovery::DiscoveredHost {
            name: "peer".into(),
            addresses: Vec::new(),
            port: 24800,
            fingerprint: None,
            platform: None,
            protocol_version: None,
            machine_id,
        }
    }

    /// The id of whoever should arbitrate instead of us, for terser tests.
    fn claim(peers: &[discovery::DiscoveredHost], mine: u64) -> Option<Option<u64>> {
        strongest_claim(peers, mine).map(|peer| peer.machine_id)
    }

    #[test]
    fn alone_on_the_network_means_arbitrating() {
        assert_eq!(claim(&[], 100), None);
    }

    #[test]
    fn the_lowest_id_arbitrates() {
        assert_eq!(claim(&[peer(Some(50))], 100), Some(Some(50)));
        assert_eq!(claim(&[peer(Some(150))], 100), None);
    }

    #[test]
    fn both_machines_reach_the_same_answer() {
        // The property the whole scheme rests on: run the rule on each machine
        // with the other in view, and exactly one of them stands down.
        let (low, high) = (7u64, 9u64);
        let low_sees_high = claim(&[peer(Some(high))], low);
        let high_sees_low = claim(&[peer(Some(low))], high);
        assert_eq!(low_sees_high, None, "the lower id should arbitrate");
        assert_eq!(
            high_sees_low,
            Some(Some(low)),
            "the higher id should follow"
        );
    }

    #[test]
    fn our_own_advertisement_is_not_a_rival() {
        // Seeing an echo of ourselves must not read as somebody outranking us,
        // however low our id happens to be.
        assert_eq!(claim(&[peer(Some(5))], 5), None);
    }

    #[test]
    fn the_lowest_of_several_wins() {
        let peers = [peer(Some(80)), peer(Some(20)), peer(Some(60))];
        assert_eq!(claim(&peers, 100), Some(Some(20)));
    }

    #[test]
    fn a_machine_too_old_to_advertise_an_id_is_deferred_to() {
        // It cannot run this rule, so it will never stand down. The only way
        // to agree is to let it have the job.
        assert_eq!(claim(&[peer(None)], 1), Some(None));
    }
}
