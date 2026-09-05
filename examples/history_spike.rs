//! History-protocol spike: explore the 0x2A2F characteristic step by step.
//!
//! Usage: cargo run --example history_spike -- <MAC> [device-id-hex]
//!
//! 1. Connects, lists services/characteristics under 0x181B (confirms 0x2A2F
//!    presence + NOTIFY flag).
//! 2. Subscribes to notifications on 0x2A2F.
//! 3. Writes the size probe (0x01 + u32 device-id LE) and prints the raw
//!    response (expect 0x01 hi lo ...).
//! 4. Writes 0x02, prints every notification until 0x03 or timeout.
//! 5. Does NOT send 0x03/0x04 ack (spike only — leaves scale position
//!    untouched). Disconnects always.

use bluer::Address;
use futures::StreamExt;
use std::time::Duration;

use grammatic::parser::BODY_COMPOSITION_HISTORY_UUID;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let target: Address = args
        .next()
        .expect("usage: history_spike <MAC> [device-id-hex]")
        .parse()
        .expect("invalid MAC address");
    let device_id: u32 = args
        .next()
        .map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).expect("bad device-id"))
        .unwrap_or(0x1234_5678);

    let (_session, adapter) = grammatic::ble::default_adapter().await?;
    let device = grammatic::ble::find_device(&adapter, target).await?;
    println!("connecting to {target} ...");
    tokio::time::timeout(Duration::from_secs(20), device.connect())
        .await
        .map_err(|_| anyhow::anyhow!("connect timed out"))??;
    println!("connected. services under 0x181B:");

    let mut history_char = None;
    for service in device.services().await? {
        let uuid = service.uuid().await?;
        if uuid.to_string().starts_with("0000181b") {
            println!("  service {uuid}");
            for ch in service.characteristics().await? {
                let cuuid = ch.uuid().await?;
                let flags = ch.flags().await.unwrap_or_default();
                println!("    char {cuuid} flags={flags:?}");
                if cuuid == BODY_COMPOSITION_HISTORY_UUID {
                    history_char = Some(ch);
                }
            }
        }
    }
    let history_char = history_char.expect("no 0x2A2F history characteristic found");
    println!("found history characteristic, subscribing ...");
    let stream = history_char.notify().await?;
    let mut stream = Box::pin(stream);

    let mut probe = vec![0x01];
    probe.extend_from_slice(&device_id.to_le_bytes());
    println!(">> write {}", hex::encode(&probe));
    history_char.write(&probe).await?;
    match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(resp)) => println!("<< size response: {}", hex::encode(&resp)),
        Ok(None) => println!("<< stream ended (no size response)"),
        Err(_) => println!("<< TIMEOUT waiting for size response"),
    }

    println!(">> write 02");
    history_char.write(&[0x02]).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(notif)) => {
                println!("<< notif ({} bytes): {}", notif.len(), hex::encode(&notif));
                if notif == vec![0x03] {
                    println!("got 0x03 stop");
                    break;
                }
            }
            Ok(None) => {
                println!("<< stream ended");
                break;
            }
            Err(_) => {
                println!("<< TIMEOUT/DEADLINE waiting for entries (no 0x03 yet)");
                break;
            }
        }
    }

    println!("disconnecting (no ack sent — scale position untouched)");
    let _ = device.disconnect().await;
    Ok(())
}
