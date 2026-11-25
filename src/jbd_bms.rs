use crate::gui::{BatteryParams, BleEvent, Message, MsgSender, Notifications};
use btleplug::api::{Central, Characteristic, Manager, Peripheral, ScanFilter, WriteType};
use btleplug::platform::{Manager as OtherManager, Peripheral as OtherPeripheral};
use futures::{SinkExt, StreamExt};
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

/// Префікс у назві пристрою, за яким фільтруємо акумулятори.
const TARGET_PREFIX: &str = "accum";
const SCAN_DURATION: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const RX_TIMEOUT: Duration = Duration::from_secs(6);

/// UUID характеристик JBD BMS.
const TX_UUID: &str = "0000ff02-0000-1000-8000-00805f9b34fb";
const RX_UUID: &str = "0000ff01-0000-1000-8000-00805f9b34fb";

pub fn short_mac(address: &str) -> String {
    let parts: Vec<&str> = address.split(':').collect();
    if parts.len() >= 3 {
        let n = parts.len();
        format!("…{}:{}:{}", parts[n - 3], parts[n - 2], parts[n - 1])
    } else {
        address.to_string()
    }
}

pub async fn scan_and_connect(
    mut sender: MsgSender,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let manager = OtherManager::new().await?;
    let adapters = manager.adapters().await?;
    if adapters.is_empty() {
        return Err("Bluetooth-адаптер не знайдено".to_string().into());
    }
    let central = adapters.into_iter().next().unwrap();

    central.start_scan(ScanFilter::default()).await?;
    let _ = sender
        .send(Message::Ble(BleEvent::Log(format!(
            "Сканування {} с…",
            SCAN_DURATION.as_secs()
        ))))
        .await;
    tokio::time::sleep(SCAN_DURATION).await;
    let _ = central.stop_scan().await;

    let peripherals = central.peripherals().await?;
    let mut found = 0u32;

    for peripheral in peripherals {
        let props = match peripheral.properties().await {
            Ok(Some(props)) => props,
            _ => continue,
        };

        let name = props.local_name.unwrap_or_default();
        if name.is_empty() || !name.to_lowercase().contains(TARGET_PREFIX) {
            continue;
        }
        let address = props.address.to_string();
        found += 1;

        let _ = sender
            .send(Message::Ble(BleEvent::Found {
                name: name.clone(),
                address: address.clone(),
            }))
            .await;

        if let Err(e) = peripheral.connect().await {
            let _ = sender
                .send(Message::Ble(BleEvent::Log(format!(
                    "Не вдалося підключитися до «{name}»: {e}"
                ))))
                .await;
            continue;
        }

        if let Err(e) = peripheral.discover_services().await {
            let _ = sender
                .send(Message::Ble(BleEvent::Log(format!(
                    "«{name}»: помилка сервісів: {e}"
                ))))
                .await;
            continue;
        }

        let chars = peripheral.characteristics();
        let tx = chars
            .iter()
            .find(|c| c.uuid == Uuid::from_str(TX_UUID).unwrap())
            .cloned();
        let rx = chars
            .iter()
            .find(|c| c.uuid == Uuid::from_str(RX_UUID).unwrap())
            .cloned();

        let (Some(tx), Some(rx)) = (tx, rx) else {
            let _ = sender
                .send(Message::Ble(BleEvent::Log(format!(
                    "«{name}»: не знайдено характеристики ff01/ff02"
                ))))
                .await;
            continue;
        };

        if let Err(e) = peripheral.subscribe(&rx).await {
            let _ = sender
                .send(Message::Ble(BleEvent::Log(format!(
                    "«{name}»: помилка підписки: {e}"
                ))))
                .await;
            continue;
        }

        let notifications = match peripheral.notifications().await {
            Ok(n) => n,
            Err(e) => {
                let _ = sender
                    .send(Message::Ble(BleEvent::Log(format!(
                        "«{name}»: немає потоку даних: {e}"
                    ))))
                    .await;
                continue;
            }
        };

        let _ = sender
            .send(Message::Ble(BleEvent::Connected {
                address: address.clone(),
            }))
            .await;

        tokio::spawn(poll_battery(
            peripheral,
            tx,
            notifications,
            address,
            sender.clone(),
        ));
    }

    if found == 0 {
        let _ = sender
            .send(Message::Ble(BleEvent::Log(
                "Акумулятори не знайдено. Увімкніть BMS і перезапустіть застосунок.".into(),
            )))
            .await;
    }

    Ok(())
}

async fn poll_battery(
    peripheral: OtherPeripheral,
    tx: Characteristic,
    mut notifications: Notifications,
    address: String,
    mut sender: MsgSender,
) {
    let rx_uuid = Uuid::from_str(RX_UUID).unwrap();
    let request = [0xDDu8, 0xA5, 0x03, 0x00, 0xFF, 0xFD, 0x77];

    loop {
        if peripheral
            .write(&tx, &request, WriteType::WithoutResponse)
            .await
            .is_err()
        {
            break;
        }

        match tokio::time::timeout(RX_TIMEOUT, notifications.next()).await {
            Ok(Some(notification)) if notification.uuid == rx_uuid => {
                if let Some(params) = parse_jbd(&notification.value) {
                    eprintln!(
                        "RAW ({} bytes): {:02X?}",
                        notification.value.len(),
                        notification.value
                    );
                    let _ = sender
                        .send(Message::Ble(BleEvent::Data {
                            address: address.clone(),
                            params,
                        }))
                        .await;
                }
            }
            Ok(Some(_)) => {} // сповіщення від іншої характеристики
            Ok(None) => break,
            Err(_) => {} // таймаут — спробуємо ще раз
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let _ = sender
        .send(Message::Ble(BleEvent::Log(format!(
            "Втрачено з’єднання з {address}"
        ))))
        .await;
}

/// Розбір відповіді JBD BMS (basic info, код 0x03).
fn parse_jbd(data: &[u8]) -> Option<BatteryParams> {
    if data.len() < 12 || data[0] != 0xDD || data[1] != 0x03 {
        return None;
    }

    let voltage = u16::from_be_bytes([data[4], data[5]]) as f32 / 100.0;
    let current = i16::from_be_bytes([data[6], data[7]]) as f32 / 100.0;
    let remaining_ah = u16::from_be_bytes([data[8], data[9]]) as f32 / 100.0;

    // SOC: try direct byte first (some firmware versions), otherwise compute
    // from remaining / total capacity. A 20-byte frame puts 0x00 at byte 19,
    // which previously caused SOC to read 0% even when the battery was charged.
    let soc = if data.len() >= 12 {
        let total_ah = u16::from_be_bytes([data[10], data[11]]) as f32 / 100.0;
        if total_ah > 0.0 {
            (remaining_ah / total_ah * 100.0).clamp(0.0, 100.0) as u8
        } else {
            0
        }
    } else {
        0
    };

    Some(BatteryParams::new(voltage, current, soc, remaining_ah))
}
