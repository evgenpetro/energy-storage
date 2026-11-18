//! Графічний інтерфейс для моніторингу акумуляторів на JBD BMS (Bluetooth LE).
//!
//! Ліва панель — список знайдених/підключених акумуляторів,
//! права панель — параметри обраного акумулятора.

use std::borrow::Cow;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, ValueNotification,
    WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use uuid::Uuid;

use futures::channel::mpsc::Sender;
use futures::{SinkExt, Stream, StreamExt};

use iced::widget::{button, container, scrollable, text, Column, Row, Space};
use iced::{Element, Font, Length, Size, Subscription, Task as Command, stream};

// ----- НАЛАШТУВАННЯ ---------------------------------------------------------

/// UUID характеристик JBD BMS.
const TX_UUID: &str = "0000ff02-0000-1000-8000-00805f9b34fb";
const RX_UUID: &str = "0000ff01-0000-1000-8000-00805f9b34fb";

/// Префікс у назві пристрою, за яким фільтруємо акумулятори.
const TARGET_PREFIX: &str = "accum";

const SCAN_DURATION: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const RX_TIMEOUT: Duration = Duration::from_secs(6);

/// Шрифт із підтримкою кирилиці (DejaVu Sans, ліцензія Bitstream Vera / public domain).
const DEFAULT_FONT: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

type Notifications = Pin<Box<dyn Stream<Item = ValueNotification> + Send>>;
type MsgSender = Sender<Message>;

fn main() -> iced::Result {
    // На цій машині немає Vulkan-драйвера (є лише loader, без ICD), тому
    // backend Vulkan у wgpu падає з STATUS_ACCESS_VIOLATION. Примусово DX12.
    #[cfg(windows)]
    // SAFETY: викликається на самому початку main, до створення будь-яких потоків.
    unsafe {
        if std::env::var_os("WGPU_BACKEND").is_none() {
            std::env::set_var("WGPU_BACKEND", "dx12");
        }
    }
    iced::application(App::default, App::update, App::view)
        .subscription(App::subscription)
        .font(Cow::Borrowed(DEFAULT_FONT))
        .default_font(Font::with_name("DejaVu Sans"))
        .window(iced::window::Settings {
            size: Size::new(960.0, 560.0),
            min_size: Some(Size::new(640.0, 400.0)),
            ..Default::default()
    }).run()
}

// ----- СТАН ЗАСТОСУНКУ -------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct BatteryParams {
    voltage: f32,
    current: f32,
    soc: u8,
    remaining_ah: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionStatus {
    Connecting,
    Connected,
}

#[derive(Debug, Clone)]
struct Battery {
    name: String,
    address: String,
    status: ConnectionStatus,
    params: Option<BatteryParams>,
}

struct App {
    batteries: Vec<Battery>,
    selected: Option<usize>,
    log: String,
}

#[derive(Debug, Clone)]
enum Message {
    Ble(BleEvent),
    Select(usize),
}

#[derive(Debug, Clone)]
enum BleEvent {
    Found { name: String, address: String },
    Connected { address: String },
    Data { address: String, params: BatteryParams },
    Log(String),
}

impl App {

    fn default() -> Self {

            App {
                batteries: Vec::new(),
                selected: None,
                log: "Запуск сканування…".into(),
            }
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::Select(i) => {
                if i < self.batteries.len() {
                    self.selected = Some(i);
                }
            }
            Message::Ble(event) => match event {
                BleEvent::Found { name, address } => {
                    if !self.batteries.iter().any(|b| b.address == address) {
                        self.batteries.push(Battery {
                            name,
                            address,
                            status: ConnectionStatus::Connecting,
                            params: None,
                        });
                        if self.selected.is_none() {
                            self.selected = Some(self.batteries.len() - 1);
                        }
                    }
                }
                BleEvent::Connected { address } => {
                    for b in &mut self.batteries {
                        if b.address == address {
                            b.status = ConnectionStatus::Connected;
                        }
                    }
                }
                BleEvent::Data { address, params } => {
                    for b in &mut self.batteries {
                        if b.address == address {
                            b.params = Some(params);
                            b.status = ConnectionStatus::Connected;
                        }
                    }
                }
                BleEvent::Log(msg) => {
                    self.log = msg;
                }
            },
        }
        Command::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let list = battery_list(&self.batteries, self.selected);

        let detail: Element<'_, Message> =
            match self.selected.and_then(|i| self.batteries.get(i)) {
                Some(b) => battery_detail(b),
                None => container(text("Оберіть акумулятор зліва."))
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into(),
            };

        let body = Row::new()
            .spacing(14)
            .padding(14)
            .width(Length::Fill)
            .height(Length::Fill)
            .push(
                container(list)
                    .width(Length::Fixed(260.0))
                    .height(Length::Fill)
                    .padding(10)
                    .style(container::rounded_box),
            )
            .push(container(detail).width(Length::Fill).height(Length::Fill));

        let footer = container(
            Row::new()
                .spacing(10)
                .width(Length::Fill)
                .push(text(self.log.clone()).size(13))
                .push(Space::new().width(Length::Fill))
                .push(text(format!("Акумуляторів: {}", self.batteries.len())).size(13)),
        )
        .width(Length::Fill)
        .padding(8)
        .style(container::rounded_box);

        container(Column::new().push(body).push(footer))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run(App::my_worker)
    }
    fn my_worker() -> Pin<Box<dyn Stream<Item = Message> + Send + 'static>> {
        Box::pin(stream::channel(
             256, |mut sender: Sender<Message>| async move {
                if let Err(e) = scan_and_connect(sender.clone()).await {
                    let _ = sender
                        .send(Message::Ble(BleEvent::Log(format!("Помилка: {e}"))))
                        .await;
                }

                // Тримаємо підписку живою; опитування працює в окремих задачах.
                loop {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
            }
        ))
    }
}

// ----- ІНТЕРФЕЙС -------------------------------------------------------------

fn battery_list(batteries: &[Battery], selected: Option<usize>) -> Element<'_, Message> {
    if batteries.is_empty() {
        return container(text("Немає акумуляторів…\nІде сканування."))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    let mut col = Column::new().spacing(6).width(Length::Fill);

    for (i, b) in batteries.iter().enumerate() {
        let marker = if selected == Some(i) { "» " } else { "  " };
        let label = format!("{marker}{}\n{}", b.name, short_mac(&b.address));
        let style = if selected == Some(i) {
            button::primary
        } else {
            button::secondary
        };

        let btn = button(text(label))
            .width(Length::Fill)
            .padding(10)
            .style(style)
            .on_press(Message::Select(i));

        col = col.push(btn);
    }

    scrollable(col).width(Length::Fill).height(Length::Fill).into()
}

fn battery_detail(battery: &Battery) -> Element<'_, Message> {
    let status_txt = match battery.status {
        ConnectionStatus::Connecting => "Підключення…",
        ConnectionStatus::Connected => "Підключено",
    };

    let metrics: Element<'_, Message> = match &battery.params {
        Some(p) => Row::new()
            .spacing(12)
            .push(metric("Напруга", format!("{:.2} V", p.voltage)))
            .push(metric("Струм", format!("{:.2} A", p.current)))
            .push(metric("Заряд (SOC)", format!("{} %", p.soc)))
            .push(metric("Залишок", format!("{:.2} Ah", p.remaining_ah)))
            .into(),
        None => text("Очікування даних…").size(16).into(),
    };

    let mut col = Column::new().spacing(10).width(Length::Fill);
    col = col.push(text(battery.name.clone()).size(26));
    col = col.push(text(format!("MAC: {}", battery.address)).size(13));
    col = col.push(text(format!("Статус: {status_txt}")).size(18));
    col = col.push(Space::new().height(Length::Fixed(8.0)));
    col = col.push(metrics);

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .into()
}

fn metric(label: impl Into<String>, value: impl Into<String>) -> Element<'static, Message> {
    container(
        Column::new()
            .spacing(2)
            .push(text(value.into()).size(24))
            .push(text(label.into()).size(13)),
    )
    .width(Length::Fill)
    .padding(12)
    .style(container::rounded_box)
    .into()
}

fn short_mac(address: &str) -> String {
    let parts: Vec<&str> = address.split(':').collect();
    if parts.len() >= 3 {
        let n = parts.len();
        format!("…{}:{}:{}", parts[n - 3], parts[n - 2], parts[n - 1])
    } else {
        address.to_string()
    }
}

// ----- BLE -------------------------------------------------------------------

async fn scan_and_connect(
    mut sender: MsgSender,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let manager = Manager::new().await?;
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
    peripheral: Peripheral,
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
    if data.len() < 10 || data[0] != 0xDD {
        return None;
    }

    let voltage = u16::from_be_bytes([data[4], data[5]]) as f32 / 100.0;
    let current = i16::from_be_bytes([data[6], data[7]]) as f32 / 100.0;
    let remaining_ah = u16::from_be_bytes([data[8], data[9]]) as f32 / 100.0;

    let soc = if data.len() > 23 {
        data[23]
    } else if data.len() > 19 {
        data[19]
    } else {
        0
    };

    Some(BatteryParams {
        voltage,
        current,
        soc,
        remaining_ah,
    })
}
