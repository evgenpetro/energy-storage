use crate::{jbd_bms};

use btleplug::api::ValueNotification;
use futures::{SinkExt, Stream};
use futures::channel::mpsc::Sender;
use iced::widget::{Column, Row, Space, button, container, scrollable, text};
use std::borrow::Cow;
use std::pin::Pin;
use std::time::Duration;
use iced::{Element, Font, Size, Length, Subscription, Task as Command, stream};



/// Шрифт із підтримкою кирилиці (DejaVu Sans, ліцензія Bitstream Vera / public domain).
const DEFAULT_FONT: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

pub type Notifications = Pin<Box<dyn Stream<Item = ValueNotification> + Send>>;
pub type MsgSender = Sender<Message>;

pub fn init_app() -> iced::Result {
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

#[derive(Debug, Clone, Copy, Default)]
pub struct BatteryParams {
    voltage: f32,
    current: f32,
    soc: u8,
    remaining_ah: f32,
}

impl BatteryParams {
    pub(crate) fn new(voltage: f32, current: f32, soc: u8, remaining_ah: f32) -> Self {
        BatteryParams {
            voltage,
            current,
            soc,
            remaining_ah
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
}

#[derive(Debug, Clone)]
pub struct Battery {
    name: String,
    address: String,
    status: ConnectionStatus,
    params: Option<BatteryParams>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Ble(BleEvent),
    Select(usize),
}

#[derive(Debug, Clone)]
pub enum BleEvent {
    Found { name: String, address: String },
    Connected { address: String },
    Data { address: String, params: BatteryParams },
    Log(String),
}

struct App {
    batteries: Vec<Battery>,
    selected: Option<usize>,
    log: String,
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
                if let Err(e) = jbd_bms::scan_and_connect(sender.clone()).await {
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
pub fn battery_list(batteries: &[Battery], selected: Option<usize>) -> Element<'_, Message> {
    if batteries.is_empty() {
        return container(text("Іде сканування..."))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    let mut col = Column::new().spacing(6).width(Length::Fill);

    for (i, b) in batteries.iter().enumerate() {
        let marker = if selected == Some(i) { "» " } else { "  " };
        let label = format!("{marker}{}\n{}", b.name, jbd_bms::short_mac(&b.address));
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

pub fn battery_detail(battery: &Battery) -> Element<'_, Message> {
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