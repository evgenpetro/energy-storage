use std::time::Duration;
use std::str::FromStr;
use std::io::{self, Write};
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Manager, Peripheral}; // Додали Peripheral сюди
use uuid::Uuid;
use futures::stream::StreamExt;

// --- НАЛАШТУВАННЯ ---
const TARGET_NAME: &str = "accum100ah12v";

// UUID для JBD BMS
const TX_UUID: &str = "0000ff02-0000-1000-8000-00805f9b34fb";
const RX_UUID: &str = "0000ff01-0000-1000-8000-00805f9b34fb";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Налаштування кольорів для Windows консолі
    #[cfg(windows)]
    let _ = ansi_term::enable_ansi_support();

    println!("--- BMS Reader for Windows 11 ---");
    println!("Цільовий пристрій: {}", TARGET_NAME);

    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;

    if adapters.is_empty() {
        eprintln!("ПОМИЛКА: Bluetooth адаптер не знайдено!");
        pause();
        return Ok(());
    }

    let central = adapters.into_iter().nth(0).unwrap();

    println!("Запуск сканування ефіру...");
    central.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(Duration::from_secs(30)).await;

    let peripherals = central.peripherals().await?;

    // --- ВИПРАВЛЕНА ЧАСТИНА: Пошук пристрою ---
    let mut found_device: Option<Peripheral> = None;

    for p in peripherals {
        // Тут ми безпечно використовуємо await, бо ми всередині async fn
        if let Ok(Some(props)) = p.properties().await {
            let local_name = props.local_name.unwrap_or_default();
            let mac_addr = props.address;
            println!("LOCAL_NAME {}; MAC {}", local_name, mac_addr);
            if local_name.contains(TARGET_NAME) {
                found_device = Some(p);
                break; // Знайшли - зупиняємо цикл
            }
        }
    }
    // ------------------------------------------

    match found_device {
        Some(bms) => {
            println!("✅ Батарею знайдено! Підключення...");

            if let Err(e) = bms.connect().await {
                eprintln!("Не вдалося підключитися: {}", e);
                pause();
                return Ok(());
            }

            println!("Підключено. Отримання сервісів...");
            bms.discover_services().await?;

            let chars = bms.characteristics();
            let tx_char = chars.iter().find(|c| c.uuid == Uuid::from_str(TX_UUID).unwrap());
            let rx_char = chars.iter().find(|c| c.uuid == Uuid::from_str(RX_UUID).unwrap());

            if let (Some(tx), Some(rx)) = (tx_char, rx_char) {
                println!("Канали даних готові. Читання...");

                bms.subscribe(rx).await?;
                let mut notification_stream = bms.notifications().await?;

                // Команда запиту (JBD Basic Info)
                let cmd: [u8; 7] = [0xDD, 0xA5, 0x03, 0x00, 0xFF, 0xFD, 0x77];
                bms.write(tx, &cmd, WriteType::WithoutResponse).await?;

                // Чекаємо відповідь (3 секунди)
                match tokio::time::timeout(Duration::from_secs(20), notification_stream.next()).await {
                    Ok(Some(data)) => {
                        parse_bms_data(&data.value);
                    }
                    Ok(None) => println!("BMS розірвала з'єднання."),
                    Err(_) => println!("Таймаут: BMS не відповіла вчасно."),
                }

                let _ = bms.disconnect().await;
            } else {
                println!("❌ Помилка: Не знайдено потрібні UUID (ff01/ff02).");
            }

        }
        None => {
            println!("❌ Пристрій '{}' не знайдено.", TARGET_NAME);
        }
    }

    pause();
    Ok(())
}

fn parse_bms_data(data: &[u8]) {
    if data.len() < 10 || data[0] != 0xDD {
        println!("Отримано RAW байти: {:02X?}", data);
        return;
    }

    let voltage = u16::from_be_bytes([data[4], data[5]]) as f32 / 100.0;
    let current = i16::from_be_bytes([data[6], data[7]]) as f32 / 100.0;
    let capacity_rem = u16::from_be_bytes([data[8], data[9]]) as f32 / 100.0;
    let real_soc_on_phone = 98;

    println!("RAW Data: {:02X?}", data);

    // Шукаємо, у якому байті сховалося число 89
    let mut found_index;
    for (index, &byte) in data.iter().enumerate() {
        println!("✅ ЗНАЙДЕНО! Число {} знаходиться у байті №{}", byte, index);
        if byte == real_soc_on_phone {
            found_index = Some(index);
        }
    }
    // Спробуємо взяти SOC з 23-го байту, якщо ні - з 19-го
    let soc = if data.len() > 23 { data[23] } else if data.len() > 19 { data[19] } else { 0 };

    println!("\n================================");
    println!("🔋 СТАН БАТАРЕЇ KEPWORTH");
    println!("================================");
    println!(" Напруга:      {:.2} V", voltage);
    println!(" Струм:        {:.2} A", current);
    println!(" Заряд (SOC):  {} %", soc);
    println!(" Залишок AH:   {:.2} Ah", capacity_rem);
    println!("================================\n");
}

fn pause() {
    print!("Натисніть Enter щоб вийти...");
    io::stdout().flush().unwrap();
    let mut buffer = String::new();
    io::stdin().read_line(&mut buffer).unwrap();
}