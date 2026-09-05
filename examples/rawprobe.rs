//! Raw frame probe: log every 0x181B payload from the scale with flags
//! decoded, bypassing FrameFilter and the capture pipeline.
//!
//! Usage: cargo run --example rawprobe -- <MAC> [seconds]

use bluer::AdapterEvent;
use futures::StreamExt;
use std::time::Duration;

use grammatic::parser::{BODY_COMPOSITION_SERVICE_UUID, parse_body_composition_frame};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let target: bluer::Address = args
        .next()
        .expect("usage: rawprobe <MAC> [seconds]")
        .parse()
        .expect("invalid MAC address");
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(120);

    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;

    println!("rawprobe watching {target} for {seconds}s (every 0x181B payload, no filtering) ...");
    let mut events = adapter.discover_devices_with_changes().await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut seen = 0u64;

    loop {
        let event = match tokio::time::timeout_at(deadline, events.next()).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                println!("discovery stream ended");
                break;
            }
            Err(_) => {
                println!("probe done ({seen} payloads)");
                break;
            }
        };
        let AdapterEvent::DeviceAdded(address) = event else {
            continue;
        };
        if address != target {
            continue;
        };
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
        seen += 1;
        let hex = hex::encode(payload);
        match parse_body_composition_frame(payload) {
            Some(m) => println!(
                "#{seen} rssi={rssi:?} flags=0x{:02x} stabilized={} weight={:.2}kg imp={:?} ts={:?} hex={hex}",
                payload.get(1).copied().unwrap_or(0),
                m.stabilized,
                m.weight_kg,
                m.impedance_ohm,
                m.timestamp,
            ),
            None => println!(
                "#{seen} rssi={rssi:?} UNPARSEABLE len={} hex={hex}",
                payload.len()
            ),
        }
    }
    Ok(())
}
