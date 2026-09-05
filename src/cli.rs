//! CLI definitions and command implementations (thin orchestration on top of
//! the capture pipeline).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use bluer::Address;
use chrono::{Local, NaiveDate};
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::ble;
use crate::capture::Capture;
use crate::clocksync::{ClockSync, GattClockWriter, set_scale_clock};
use crate::config::Config;
use crate::history::{GattHistoryReader, HistoryReader, SessionTracker, load_or_create_device_id};
use crate::metrics::Sex;
use crate::spool::{self, Spool, SpooledFrame};
use crate::store;

#[derive(Parser)]
#[command(
    name = "grammatic",
    version,
    about = "Xiaomi Mi Body Composition Scale 2 capture agent"
)]
pub struct Cli {
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "config.toml")]
    pub config: PathBuf,
    /// Verbose logging.
    #[arg(short, long)]
    pub debug: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Serve the integrated dashboard and HTTP API (behind your authenticated proxy).
    Serve {
        #[arg(long, default_value = "127.0.0.1:8090")]
        bind: String,
        #[arg(long, default_value = "frontend/dist/client")]
        frontend: PathBuf,
    },
    /// Passively read scale advertisements and record measurements
    Listen {
        /// Exit after the first recorded measurement (for debugging).
        #[arg(long)]
        once: bool,
    },
    /// Scan for nearby Xiaomi scales and print them
    Find {
        #[arg(long, default_value_t = 15.0)]
        scan_seconds: f64,
    },
    /// Manage profiles stored in the database
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Recompute stored metric columns from current profiles
    Recompute {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        id: Option<i64>,
    },
    /// Set the scale's clock to this machine's time
    SyncClock,
    /// Pull stored weigh-ins from the scale over GATT and record them
    FetchHistory,
    /// Dev tool: replay frame hex lines (spool format or bare hex) through the pipeline
    Replay { file: PathBuf },
}

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// Add a profile
    Add {
        name: String,
        #[arg(long)]
        sex: String,
        #[arg(long)]
        height_cm: f64,
        #[arg(long)]
        dob: String,
        /// Exclusive lower weight bound (cm/kg windows); omit for unbounded
        #[arg(long)]
        weight_min: Option<f64>,
        /// Exclusive upper weight bound
        #[arg(long)]
        weight_max: Option<f64>,
    },
    /// List profiles
    List,
    /// Remove a profile (its measurements become guest rows)
    Remove { name: String },
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let Cli {
        config: config_path,
        debug: _,
        command,
    } = cli;
    let config = Config::load(&config_path)?;
    match command {
        Command::Serve { bind, frontend } => {
            crate::web::serve(
                connect_db(&config).await?,
                config.metrics.store,
                &bind,
                frontend,
            )
            .await
        }
        Command::Listen { once } => cmd_listen(&config, once).await,
        Command::Find { scan_seconds } => cmd_find(&config, scan_seconds).await,
        Command::Profile { command } => cmd_profile(&config, command).await,
        Command::Recompute { all, id } => cmd_recompute(&config, all, id).await,
        Command::SyncClock => cmd_sync_clock(&config).await,
        Command::FetchHistory => cmd_fetch_history(&config).await,
        Command::Replay { file } => cmd_replay(&config, file).await,
    }
}

fn target_address(config: &Config) -> anyhow::Result<Address> {
    let mac = config.scale_mac()?;
    mac.parse::<Address>()
        .with_context(|| format!("parsing scale_mac {mac:?}"))
}

async fn connect_db(config: &Config) -> anyhow::Result<sqlx::PgPool> {
    store::connect(&config.database_url()?).await
}

async fn cmd_listen(config: &Config, once: bool) -> anyhow::Result<()> {
    let target = target_address(config)?;
    let pool = connect_db(config).await?;
    let capture = Capture::with_metrics_policy(
        pool,
        Arc::new(Spool::new(
            config.listen.spool_path.clone(),
            config.listen.spool_max_bytes,
        )),
        config.metrics.store,
    );

    // Replay anything captured while the DB was down.
    match capture.replay_spool().await {
        Ok((replayed, malformed)) if replayed > 0 || malformed > 0 => {
            info!(
                "replayed {replayed} spooled measurement(s), dropped {malformed} malformed line(s)"
            );
        }
        Ok(_) => {}
        Err(error) => warn!("spool replay failed: {error:#}"),
    }

    // The automatic history fallback needs raw advertisement sightings
    // (not filtered frames) to tell scale silence apart from filter
    // silence — see SessionTracker. Only service mode with auto_fetch
    // consumes them.
    let auto_fallback = !once && config.history.enabled && config.history.auto_fetch;
    let (sighting_tx, mut sighting_rx) = if auto_fallback {
        let (tx, rx) = mpsc::channel::<ble::Sighting>(128);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (tx, mut rx) = mpsc::channel::<ble::Frame>(64);
    let listener = tokio::spawn(ble::run_listener(target, tx, sighting_tx));
    let mut clock_sync = ClockSync::new(
        config.clock_sync.enabled,
        config.clock_sync.drift_threshold_secs,
        GattClockWriter::new(target),
    );
    // Automatic fallback: armed by live frames, fired after quiet + idle.
    // Default off pending the 24 h soak (ADR-0002); manual `fetch-history`
    // is validated and works regardless.
    let mut tracker = auto_fallback.then(|| {
        SessionTracker::new(std::time::Duration::from_secs(
            config.history.quiet_timeout_secs,
        ))
    });
    let mode = if once {
        "exits after the first recorded measurement"
    } else {
        "press Ctrl+C to stop"
    };
    info!("listening for scale {target} ({mode})");

    // Ticks drive the fallback poll below; sightings + frames arrive on
    // their own channels. The pull runs inline (never concurrent with a
    // clock write — both are awaited here, one at a time).
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
    // First tick fires immediately; skip it so a fresh start never pulls.
    tick.tick().await;

    let mut finished = false;
    loop {
        tokio::select! {
            frame = rx.recv() => {
                let Some(frame) = frame else { break };
                let now = std::time::Instant::now();
                clock_sync.on_frame(now);
                if let (Some(tracker), Some(sightings)) = (tracker.as_mut(), sighting_rx.as_mut()) {
                    // Drain sightings that arrived with this frame batch so
                    // the quiet clock reflects the latest advertisement.
                    tracker.note_sighting(now);
                    while sightings.try_recv().is_ok() {
                        tracker.note_sighting(now);
                    }
                }
                let Some(result) = capture
                    .handle_frame(&frame.payload, Local::now(), frame.rssi)
                    .await
                else {
                    // Live (not-yet-stabilized) frame: a weigh-in may be in
                    // progress — arm the fallback.
                    if let Some(tracker) = tracker.as_mut() {
                        tracker.note_live_frame();
                    }
                    continue;
                };
                if once && result.recorded {
                    finished = true;
                    break;
                }
                if let Some(tracker) = tracker.as_mut() {
                    // Any stabilized outcome (recorded or re-broadcast
                    // collapse) means nothing is missing.
                    tracker.note_stabilized();
                }
                clock_sync.on_capture(&result, std::time::Instant::now()).await;
            }
            sighting = async {
                match sighting_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Sightings between frames: user standing still (filter
                // silence) still resets the quiet clock.
                if let (Some(_), Some(tracker)) = (sighting, tracker.as_mut()) {
                    tracker.note_sighting(std::time::Instant::now());
                }
            }
            _ = tick.tick(), if tracker.is_some() && !once => {
                let Some(tracker) = tracker.as_mut() else { continue };
                // Fire only when armed, quiet on raw sightings, and idle.
                // Clock-idle is approximated here by quiet: the quiet
                // timeout (default 75 s) strictly exceeds the 30 s
                // clock-sync idle window, so a quiet scale is an idle
                // scale; the pull itself fails fast (log + backoff) when
                // the scale is mid-weigh-in and unreachable.
                let now = std::time::Instant::now();
                if !tracker.ready(now, true) {
                    continue;
                }
                tracker.note_fire(now);
                match run_history_fallback(config, target, &capture).await {
                    Ok((fetched, recorded)) => {
                        tracker.note_attempt(true, now);
                        if recorded > 0 {
                            info!("history fallback recovered {recorded} measurement(s)");
                            tracker.note_recovered();
                        } else if fetched == 0 {
                            info!("history fallback: scale has no new entries");
                            tracker.note_empty_pull();
                        } else {
                            // Fetched but all collapsed as re-broadcasts:
                            // nothing missing either.
                            info!(
                                "history fallback: {fetched} entr(y/ies) already recorded, nothing new"
                            );
                            tracker.note_recovered();
                        }
                    }
                    Err(error) => {
                        warn!("history fallback failed: {error:#}");
                        tracker.note_attempt(false, now);
                        // Stay armed: the next quiet window retries (backed
                        // off). Reset the single-fire latch so backoff, not
                        // the latch, gates the retry.
                        tracker.last_fire_clear();
                    }
                }
            }
        }
    }
    listener.abort();
    if !finished && !once {
        // Receiving ended because the BLE listener task died; make the
        // process fail so systemd restarts it.
        anyhow::bail!("BLE listener stopped unexpectedly");
    }
    if !finished && once {
        // `--once` with the channel closed and no measurement: same failure.
        anyhow::bail!("BLE listener stopped unexpectedly");
    }
    Ok(())
}

/// One automatic history pull inside `listen`: connect, fetch, feed through
/// capture. Returns (fetched, recorded). Errors are for the caller to log +
/// back off — never spool live frames, never synthesize.
async fn run_history_fallback(
    config: &Config,
    target: Address,
    capture: &Capture<sqlx::PgPool>,
) -> anyhow::Result<(usize, usize)> {
    let device_id = load_or_create_device_id(&config.history.device_id_file)?;
    let reader = GattHistoryReader::new(target, device_id);
    let entries = reader.fetch_history().await?;
    let fetched = entries.len();
    let (recorded, malformed) =
        crate::history::record_fetched(capture, &entries, Local::now(), None).await;
    if malformed > 0 {
        warn!("dropped {malformed} malformed history entr(y/ies)");
    }
    Ok((fetched, recorded))
}

async fn cmd_find(config: &Config, scan_seconds: f64) -> anyhow::Result<()> {
    let target = config
        .scale_mac()
        .ok()
        .and_then(|mac| mac.parse::<Address>().ok());
    println!("scanning for {scan_seconds} seconds ...");
    let scales = ble::scan_scales(scan_seconds, target).await?;
    if scales.is_empty() {
        println!("no scales found; step on the scale briefly to wake it up and try again");
        return Ok(());
    }
    println!(
        "{:17}  {:>4}  {:8}  services",
        "MAC address", "RSSI", "name"
    );
    for scale in &scales {
        let marker = if target == Some(scale.address) {
            "   <- configured scale"
        } else {
            ""
        };
        println!(
            "{:17}  {:>4}  {:8}  {}{}",
            scale.address.to_string(),
            scale
                .rssi
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            if scale.name.is_empty() {
                "-"
            } else {
                &scale.name
            },
            scale
                .services
                .iter()
                .map(|uuid| uuid.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            marker,
        );
    }
    Ok(())
}

async fn cmd_profile(config: &Config, command: ProfileCommand) -> anyhow::Result<()> {
    let pool = connect_db(config).await?;
    match command {
        ProfileCommand::Add {
            name,
            sex,
            height_cm,
            dob,
            weight_min,
            weight_max,
        } => {
            let sex = Sex::parse(&sex).context("sex must be 'male' or 'female'")?;
            let dob =
                NaiveDate::parse_from_str(&dob, "%Y-%m-%d").context("dob must be YYYY-MM-DD")?;
            let id = store::add_profile(&pool, &name, sex, height_cm, dob, weight_min, weight_max)
                .await?;
            println!("added profile {name} (id {id})");
        }
        ProfileCommand::List => {
            let profiles = store::list_profiles(&pool).await?;
            if profiles.is_empty() {
                println!("no profiles; add one with 'grammatic profile add'");
                return Ok(());
            }
            println!(
                "{:>3}  {:12}  {:6}  {:>5}  {:10}  {:>7}  {:>7}",
                "id", "name", "sex", "height", "dob", "min kg", "max kg"
            );
            for profile in profiles {
                println!(
                    "{:>3}  {:12}  {:6}  {:>5}  {:10}  {:>7}  {:>7}",
                    profile.id,
                    profile.name,
                    profile.sex,
                    profile.height_cm,
                    profile.dob,
                    profile
                        .weight_min
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into()),
                    profile
                        .weight_max
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into()),
                );
            }
        }
        ProfileCommand::Remove { name } => {
            if store::remove_profile(&pool, &name).await? {
                println!("removed profile {name}");
            } else {
                println!("no profile named {name}");
            }
        }
    }
    Ok(())
}

async fn cmd_recompute(config: &Config, all: bool, id: Option<i64>) -> anyhow::Result<()> {
    if !all && id.is_none() {
        anyhow::bail!("specify --all or --id <measurement id>");
    }
    if all && id.is_some() {
        anyhow::bail!("--all and --id are mutually exclusive");
    }
    let pool = connect_db(config).await?;
    let updated = store::recompute_metrics(&pool, id, &config.metrics.store).await?;
    println!("recomputed metrics for {updated} measurement(s)");
    Ok(())
}

async fn cmd_sync_clock(config: &Config) -> anyhow::Result<()> {
    let target = target_address(config)?;
    set_scale_clock(target).await?;
    println!("scale clock synchronized");
    Ok(())
}

/// Pull stored weigh-ins over GATT and feed them through capture.
///
/// History-only: entries already captured live collapse via the dedup key,
/// an empty history records nothing, and a GATT failure is an error here
/// (the automatic fallback in `listen` logs + backs off instead).
async fn cmd_fetch_history(config: &Config) -> anyhow::Result<()> {
    use crate::history::{GattHistoryReader, HistoryReader, load_or_create_device_id};

    let target = target_address(config)?;
    let pool = connect_db(config).await?;
    let capture = Capture::with_metrics_policy(
        pool,
        Arc::new(Spool::new(
            config.listen.spool_path.clone(),
            config.listen.spool_max_bytes,
        )),
        config.metrics.store,
    );

    let device_id = load_or_create_device_id(&config.history.device_id_file)?;
    let reader = GattHistoryReader::new(target, device_id);
    let entries = reader.fetch_history().await?;
    let fetched = entries.len();
    let (recorded, malformed) =
        crate::history::record_fetched(&capture, &entries, Local::now(), None).await;
    if malformed > 0 {
        warn!("dropped {malformed} malformed history entr(y/ies)");
    }
    println!("fetched {fetched} / recorded {recorded} measurement(s) from history");
    Ok(())
}

async fn cmd_replay(config: &Config, file: PathBuf) -> anyhow::Result<()> {
    let pool = connect_db(config).await?;
    let spool = Arc::new(Spool::new(
        config.listen.spool_path.clone(),
        config.listen.spool_max_bytes,
    ));
    let capture = Capture::with_metrics_policy(pool, spool, config.metrics.store);

    let content =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let mut recorded = 0;
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        // Spool format (received_at\trssi\thex), or bare frame hex with
        // receive time standing in as now.
        let frame = match spool::parse_line(line) {
            Some(frame) => frame,
            None => SpooledFrame {
                received_at: Local::now().fixed_offset(),
                rssi: None,
                payload: hex::decode(line.trim())
                    .with_context(|| format!("parsing hex line: {line:?}"))?,
            },
        };
        if let Some(result) = capture
            .handle_frame(
                &frame.payload,
                frame.received_at.with_timezone(&Local),
                frame.rssi,
            )
            .await
            && result.recorded
        {
            recorded += 1;
        }
    }
    println!(
        "replayed {} measurement(s) from {}",
        recorded,
        file.display()
    );
    Ok(())
}
