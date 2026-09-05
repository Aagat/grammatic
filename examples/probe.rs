//! Diagnostic probe: watch BLE discovery + device events for a given address.
//!
//! Usage: cargo run --release --example probe -- <MAC> [seconds]

use bluer::{AdapterEvent, DeviceEvent, DeviceProperty};
use futures::StreamExt;
use std::time::Duration;

#[tokio::main]
async fn main() -> bluer::Result<()> {
    let mut args = std::env::args().skip(1);
    let target: bluer::Address = args
        .next()
        .expect("usage: probe <MAC> [seconds]")
        .parse()
        .expect("invalid MAC address");
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);

    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;

    let mut discovery = adapter.discover_devices().await?;
    let device = adapter.device(target)?;
    let mut changes = device.events().await?;
    println!("probe running for {seconds} s; discovery + device events selected ...");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let discovery_events = tokio::time::timeout_at(deadline, async {
        while let Some(event) = discovery.next().await {
            match event {
                AdapterEvent::DeviceAdded(addr) => println!("discovery: DeviceAdded {addr}"),
                AdapterEvent::DeviceRemoved(addr) => println!("discovery: DeviceRemoved {addr}"),
                AdapterEvent::PropertyChanged(_) => {}
            }
        }
    });
    let device_events = tokio::time::timeout_at(deadline, async {
        while let Some(event) = changes.next().await {
            match event {
                DeviceEvent::PropertyChanged(DeviceProperty::ServiceData(data)) => {
                    println!("device: ServiceData {data:?}");
                }
                DeviceEvent::PropertyChanged(DeviceProperty::Rssi(rssi)) => {
                    println!("device: Rssi {rssi}");
                }
                DeviceEvent::PropertyChanged(_) => {}
            }
        }
        println!("device: STREAM ENDED");
    });

    let _ = tokio::try_join!(discovery_events, device_events);
    println!("probe done");
    Ok(())
}
