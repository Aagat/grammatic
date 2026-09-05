//! BLE adapter (thin seam over BlueZ via bluer).
//!
//! All reconnect/scan/property-stream knowledge lives here; callers see a
//! channel of service-data frames and a couple of helpers. Which observed
//! payloads become frames is decided by [`FrameFilter`] — a pure decision
//! core, testable without hardware.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use bluer::{Adapter, AdapterEvent, Address, Device, Session, Uuid};
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::warn;

use crate::parser::BODY_COMPOSITION_SERVICE_UUID;

/// One advertisement payload for the body-composition service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub payload: Vec<u8>,
    pub rssi: Option<i16>,
}

/// One raw advertisement sighting from the target scale — emitted for every
/// advertisement, including filter-suppressed re-broadcasts. The history
/// fallback's quiet detection consumes these: filtered frames alone cannot
/// tell scale silence apart from a user standing still on one weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sighting {
    /// Filter-suppressed (identical to the last emitted frame).
    pub suppressed: bool,
}

#[derive(Debug, Clone)]
pub struct ScaleInfo {
    pub address: Address,
    pub name: String,
    pub rssi: Option<i16>,
    pub services: Vec<Uuid>,
}

pub async fn default_adapter() -> anyhow::Result<(Session, Adapter)> {
    let session = Session::new()
        .await
        .context("opening BlueZ session (is bluetoothd running?)")?;
    let adapter = session
        .default_adapter()
        .await
        .context("no default Bluetooth adapter")?;
    adapter
        .set_powered(true)
        .await
        .context("powering the adapter")?;
    Ok((session, adapter))
}

/// Find the target device, scanning until it appears.
pub async fn find_device(adapter: &Adapter, target: Address) -> anyhow::Result<Device> {
    if let Ok(device) = adapter.device(target) {
        return Ok(device);
    }
    let mut events = adapter.discover_devices().await?;
    while let Some(event) = events.next().await {
        if let AdapterEvent::DeviceAdded(address) = event
            && address == target
        {
            return Ok(adapter.device(address)?);
        }
    }
    anyhow::bail!("device discovery ended unexpectedly")
}

/// Pure decision core of the advertisement loop: which observed payloads
/// become frames. Owns the re-broadcast suppression (the scale re-sends the
/// identical final frame after a measurement) and RSSI stickiness — the
/// event-delivery logic that once broke — so it is testable without
/// hardware.
#[derive(Default)]
pub struct FrameFilter {
    last_sent: Option<Vec<u8>>,
    last_rssi: Option<i16>,
}

impl FrameFilter {
    pub fn new() -> FrameFilter {
        FrameFilter::default()
    }

    /// Observe one service-data payload from the target scale. Returns the
    /// frame to emit, or `None` when the payload is an identical re-broadcast
    /// of the last emitted frame.
    pub fn observe(&mut self, rssi: Option<i16>, payload: &[u8]) -> Option<Frame> {
        // RSSI is sticky: property updates don't always carry it, and the
        // last seen value is better than none.
        if let Some(rssi) = rssi {
            self.last_rssi = Some(rssi);
        }
        if self.last_sent.as_deref() == Some(payload) {
            return None;
        }
        self.last_sent = Some(payload.to_vec());
        Some(Frame {
            payload: payload.to_vec(),
            rssi: self.last_rssi,
        })
    }
}

/// Why one discovery pass ended.
enum ListenerError {
    /// The consumer dropped the frame channel: the only deliberate exit.
    Closed,
    /// Anything else — log it and rescan.
    Transient(anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
#[error("frame channel closed (consumer gone)")]
struct ChannelClosed;

/// Run the advertisement listener until the consumer drops the frame
/// channel. Transient BLE errors (stale device objects, discovery ending)
/// are logged and retried after 2 s; nothing but a closed channel stops it.
///
/// `sightings` receives one [`Sighting`] per advertisement from the target
/// scale — including filter-suppressed re-broadcasts — so the history
/// fallback can tell scale silence apart from filter silence. Pass `None`
/// when nobody consumes sightings (e.g. `--once` probes).
pub async fn run_listener(
    target: Address,
    tx: mpsc::Sender<Frame>,
    sightings: Option<mpsc::Sender<Sighting>>,
) {
    loop {
        match listen_once(target, &tx, &sightings).await {
            ListenerError::Closed => return,
            ListenerError::Transient(error) => {
                warn!("BLE listener: {error:#}; retrying in 2 s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

/// One discovery pass. Never returns success — it either hits the closed
/// channel or ends in something that warrants a rescan.
async fn listen_once(
    target: Address,
    tx: &mpsc::Sender<Frame>,
    sightings: &Option<mpsc::Sender<Sighting>>,
) -> ListenerError {
    match listen(target, tx, sightings).await {
        Ok(()) => ListenerError::Transient(anyhow::anyhow!("advertisement stream ended")),
        Err(error) if error.downcast_ref::<ChannelClosed>().is_some() => ListenerError::Closed,
        Err(error) => ListenerError::Transient(error),
    }
}

async fn listen(
    target: Address,
    tx: &mpsc::Sender<Frame>,
    sightings: &Option<mpsc::Sender<Sighting>>,
) -> anyhow::Result<()> {
    let (_session, adapter) = default_adapter().await?;

    // With `discover_devices_with_changes`, every property change of a device
    // during discovery re-emits DeviceAdded — including fresh ServiceData
    // from each new advertisement. This avoids subscribing to per-device
    // event streams, which end as soon as BlueZ replaces a stale device
    // object (that failure mode: subscribing to a cached handle yields no
    // events at all).
    let mut events = adapter.discover_devices_with_changes().await?;
    let mut filter = FrameFilter::new();

    while let Some(event) = events.next().await {
        let AdapterEvent::DeviceAdded(address) = event else {
            continue;
        };
        if address != target {
            continue;
        }
        let Ok(device) = adapter.device(address) else {
            continue;
        };
        let rssi = device.rssi().await.ok().flatten();
        let Some(data) = device.service_data().await.ok().flatten() else {
            continue;
        };
        let Some(payload) = data.get(&BODY_COMPOSITION_SERVICE_UUID) else {
            continue;
        };
        let frame = filter.observe(rssi, payload);
        if let Some(sightings) = sightings {
            // Best-effort: a full sighting channel must never stall the
            // frame pipeline — the fallback just misses one heartbeat.
            let _ = sightings.try_send(Sighting {
                suppressed: frame.is_none(),
            });
        }
        let Some(frame) = frame else {
            continue;
        };
        tx.send(frame)
            .await
            .map_err(|_| anyhow::Error::new(ChannelClosed))?;
    }
    anyhow::bail!("discovery ended; rescanning")
}

/// Scan for a while and collect candidate Xiaomi scales.
pub async fn scan_scales(seconds: f64, target: Option<Address>) -> anyhow::Result<Vec<ScaleInfo>> {
    let (_session, adapter) = default_adapter().await?;
    let mut events = adapter.discover_devices().await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(seconds.max(0.1));
    let mut found: HashMap<Address, (Device, ScaleInfo)> = HashMap::new();

    loop {
        let event = match tokio::time::timeout_at(deadline, events.next()).await {
            Ok(Some(event)) => event,
            Ok(None) | Err(_) => break,
        };
        let AdapterEvent::DeviceAdded(address) = event else {
            continue;
        };
        let device = adapter.device(address)?;
        let services: Vec<Uuid> = device
            .service_data()
            .await
            .ok()
            .flatten()
            .map(|data| {
                data.keys()
                    .copied()
                    .filter(|uuid| *uuid == BODY_COMPOSITION_SERVICE_UUID)
                    .collect()
            })
            .unwrap_or_default();
        let name = device.name().await.ok().flatten().unwrap_or_default();
        let is_target = target == Some(address);
        let is_scale = !services.is_empty()
            || matches!(name.to_uppercase().as_str(), "MIBFS" | "MI_SCALE")
            || is_target;
        if !is_scale {
            continue;
        }
        let rssi = device.rssi().await.ok().flatten();
        let entry = found.entry(address).or_insert_with(|| {
            (
                device.clone(),
                ScaleInfo {
                    address,
                    name: String::new(),
                    rssi: None,
                    services: Vec::new(),
                },
            )
        });
        if !name.is_empty() {
            entry.1.name = name;
        }
        entry.1.services = services;
        entry.1.rssi = rssi.max(entry.1.rssi);
    }

    let mut scales: Vec<ScaleInfo> = found.into_values().map(|(_, info)| info).collect();
    scales.sort_by_key(|info| std::cmp::Reverse(info.rssi));
    Ok(scales)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_payload_is_emitted() {
        let mut filter = FrameFilter::new();
        let frame = filter.observe(Some(-55), &[0x02, 0x22]).unwrap();
        assert_eq!(frame.payload, vec![0x02, 0x22]);
        assert_eq!(frame.rssi, Some(-55));
    }

    #[test]
    fn identical_rebroadcasts_are_suppressed() {
        let mut filter = FrameFilter::new();
        assert!(filter.observe(Some(-55), &[0x02, 0x22]).is_some());
        // the scale re-broadcasts the identical final frame
        assert!(filter.observe(None, &[0x02, 0x22]).is_none());
    }

    #[test]
    fn a_changed_payload_is_emitted_again() {
        let mut filter = FrameFilter::new();
        filter.observe(Some(-55), &[0x02, 0x22]);
        assert!(filter.observe(None, &[0x02, 0x23]).is_some());
    }

    #[test]
    fn rssi_is_sticky_across_updates() {
        let mut filter = FrameFilter::new();
        filter.observe(Some(-55), &[0x01]);
        // property updates don't always carry an RSSI
        let frame = filter.observe(None, &[0x02]).unwrap();
        assert_eq!(frame.rssi, Some(-55));
        // a fresh value wins
        let frame = filter.observe(Some(-70), &[0x03]).unwrap();
        assert_eq!(frame.rssi, Some(-70));
    }

    #[test]
    fn suppression_never_hides_a_new_measurement() {
        let mut filter = FrameFilter::new();
        let final_frame = [0x02u8, 0x22];
        filter.observe(Some(-55), &final_frame);
        filter.observe(None, &final_frame); // suppressed re-broadcast
        filter.observe(None, &final_frame); // still suppressed
        // a genuinely new frame after the re-broadcasts must come through
        assert!(filter.observe(None, &[0x02, 0x24]).is_some());
    }
}
