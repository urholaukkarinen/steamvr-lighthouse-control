use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::thread::spawn;
use std::time::Duration;

use bleasy::{BDAddr, Device, DeviceEvent, Error, ScanConfig, Scanner};
use egui::{Layout, Ui, Widget};
use futures::StreamExt;
use macroquad::prelude::*;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::sleep;
use uuid::Uuid;

use crate::widgets::Spinner;

mod widgets;

const POWER_UUID: Uuid = Uuid::from_u128(0x00001525_1212_EFDE_1523_785FEABCD124);
const SCAN_TIMEOUT: Duration = Duration::from_millis(10000);
const STATE_POLL_INTERVAL: Duration = Duration::from_millis(500);

fn window_conf() -> Conf {
    Conf {
        window_title: "SteamVR Lighthouse Control".to_owned(),
        window_width: 450,
        window_height: 300,
        icon: None,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    pretty_env_logger::init();

    let app_state = Arc::new(Mutex::new(AppState::new()));

    // Channel for sending commands to ble thread
    let (cmd_tx, cmd_rx) = channel::<Command>(16);

    {
        let app_state = app_state.clone();

        spawn(move || ble_thread(app_state, cmd_rx));
    }

    loop {
        clear_background(BLACK);

        egui_macroquad::ui(|egui_ctx| {
            let mut app_state = app_state.blocking_lock();

            egui::CentralPanel::default().show(egui_ctx, |ui| {
                ui_header(ui, &cmd_tx, &mut app_state);
                ui.separator();
                ui_device_list(ui, &cmd_tx, &mut app_state);
            });
        });

        egui_macroquad::draw();
        next_frame().await;
    }
}

#[derive(Default)]
struct AppState {
    scanner: Scanner,
    device_entries: HashMap<BDAddr, DeviceEntry>,
    ble_devices: HashMap<BDAddr, Device>,
    error_state: Option<ErrorState>
}

impl AppState {
    fn new() -> Self {
        Self {
            scanner: Scanner::new(),
            device_entries: HashMap::new(),
            ble_devices: HashMap::new(),
            error_state: None
        }
    }

    async fn start_scan(&mut self) -> Result<(), Error> {
        self.device_entries.clear();
        self.ble_devices.clear();
        self.scanner
            .start(
                ScanConfig::default()
                    .filter_by_characteristics(|uuids| uuids.contains(&POWER_UUID))
                    .stop_after_timeout(SCAN_TIMEOUT),
            )
            .await
    }

    fn update_power_state(&mut self, device_addr: BDAddr, power: PowerState) {
        if let Some(mut d) = self.device_entries.get_mut(&device_addr) {
            d.power_state = power;
        }
    }

    async fn insert_device(&mut self, device_addr: BDAddr, device: Device) {
        self.device_entries.insert(
            device_addr,
            DeviceEntry {
                name: device.local_name().await,
                power_state: PowerState::Unknown,
            },
        );

        self.ble_devices.insert(device_addr, device);
    }
}

enum ErrorState {
    StartFailed
}

#[derive(Default)]
struct DeviceEntry {
    name: Option<String>,
    power_state: PowerState,
}

async fn start_scan(app_state: Arc<Mutex<AppState>>) {
    if app_state.lock().await.start_scan().await.is_err() {
        app_state.lock().await.error_state = Some(ErrorState::StartFailed);
    } else {
        app_state.lock().await.error_state = None;
    }

    let mut event_stream = app_state.lock().await.scanner.device_event_stream();

    tokio::task::spawn(async move {
        while let Some(event) = event_stream.next().await {
            match event {
                DeviceEvent::Discovered(device) => {
                    app_state
                        .lock()
                        .await
                        .insert_device(device.address(), device.clone())
                        .await;
                }
                DeviceEvent::Updated(device) => {
                    if let Some(d) = app_state
                        .lock()
                        .await
                        .device_entries
                        .get_mut(&device.address())
                    {
                        d.name = device.local_name().await;
                    }
                }
                _ => {}
            }
        }
    });
}

#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
enum PowerState {
    On,
    Standby,
    Sleep,
    Starting,
    Unknown,
}

impl From<&[u8]> for PowerState {
    fn from(data: &[u8]) -> Self {
        match data {
            &[0x00] => PowerState::Sleep,
            &[0x01] | &[0x0B] => PowerState::On,
            &[0x02] => PowerState::Standby,
            &[0x09] => PowerState::Starting,
            _ => PowerState::Unknown,
        }
    }
}

impl Display for PowerState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PowerState::On => "On",
            PowerState::Standby => "Standby",
            PowerState::Sleep => "Sleep",
            PowerState::Starting => "Starting",
            PowerState::Unknown => "Unknown",
        })
    }
}

impl Default for PowerState {
    fn default() -> Self {
        PowerState::Unknown
    }
}

enum Command {
    StartScan,
    ChangePowerState(BDAddr, PowerStateCommand),
}

enum PowerStateCommand {
    On,
    Sleep,
    Standby,
}

impl From<PowerStateCommand> for u8 {
    fn from(cmd: PowerStateCommand) -> u8 {
        match cmd {
            PowerStateCommand::On => 0x01,
            PowerStateCommand::Sleep => 0x00,
            PowerStateCommand::Standby => 0x02,
        }
    }
}

#[tokio::main]
async fn ble_thread(app_state: Arc<Mutex<AppState>>, mut cmd_rx: Receiver<Command>) {
    start_scan(app_state.clone()).await;

    let poll_task = {
        let app_state = app_state.clone();
        tokio::task::spawn(async move {
            loop {
                let devices = app_state.lock().await.ble_devices.clone();

                for (addr, device) in devices {
                    if let Ok(Some(power)) = device.characteristic(POWER_UUID).await {
                        if let Ok(data) = power.read().await {
                            let state = data.as_slice().into();

                            if state != PowerState::Unknown {
                                if let Some(mut d) = app_state.lock().await.device_entries.get_mut(&addr) {
                                    d.power_state = state;
                                }
                            }
                        }
                    }
                }

                sleep(STATE_POLL_INTERVAL).await;
            }
        })
    };

    let cmd_task = {
        let app_state = app_state.clone();

        tokio::task::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    Command::StartScan => {
                        start_scan(app_state.clone()).await;
                    }
                    Command::ChangePowerState(addr, state) => {
                        if let Some(device) = app_state.lock().await.ble_devices.get(&addr) {
                            if let Ok(Some(power)) = device.characteristic(POWER_UUID).await {
                                if let Err(e) = power.write_command(&[state.into()]).await {
                                    println!("Could not send command to device: {:?}", e);
                                }
                            }
                        }
                    }
                }
            }
        })
    };

    poll_task.await.unwrap();
    cmd_task.await.unwrap();
}

fn ui_device_list(ui: &mut Ui, cmd_tx: &Sender<Command>, app_state: &mut MutexGuard<AppState>) {
    egui::Grid::new("grid")
        .num_columns(3)
        .striped(true)
        .spacing([15.0, 4.0])
        .show(ui, |ui| {
            for (addr, device) in &mut app_state.device_entries {
                ui_device_entry(ui, cmd_tx, addr, device);
            }
        });
}

fn ui_device_entry(ui: &mut Ui, cmd_tx: &Sender<Command>, addr: &BDAddr, device: &mut DeviceEntry) {
    let power_state = device.power_state;

    ui.horizontal(|ui| {
        ui.label("Name: ");
        if let Some(name) = device.name.as_ref() {
            ui.label(name);
        } else {
            ui.label("?");
        }
    });

    ui.horizontal(|ui| {
        ui.label("State: ");
        ui.label(power_state.to_string());
    });

    ui.allocate_ui(ui.available_size(), |ui| {
        ui.with_layout(Layout::right_to_left(), |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        ![PowerState::Standby, PowerState::Unknown].contains(&power_state),
                        egui::Button::new("stand by"),
                    )
                    .clicked()
                {
                    device.power_state = PowerState::Standby;
                    cmd_tx
                        .blocking_send(Command::ChangePowerState(*addr, PowerStateCommand::Standby))
                        .ok();
                }

                if ui
                    .add_enabled(
                        ![PowerState::Sleep, PowerState::Unknown].contains(&power_state),
                        egui::Button::new("sleep"),
                    )
                    .clicked()
                {
                    device.power_state = PowerState::Sleep;
                    cmd_tx
                        .blocking_send(Command::ChangePowerState(*addr, PowerStateCommand::Sleep))
                        .ok();
                }

                if ui
                    .add_enabled(
                        [PowerState::Sleep, PowerState::Standby].contains(&power_state),
                        egui::Button::new("on"),
                    )
                    .clicked()
                {
                    device.power_state = PowerState::Starting;
                    cmd_tx
                        .blocking_send(Command::ChangePowerState(*addr, PowerStateCommand::On))
                        .ok();
                }
            });
        });
    });

    ui.end_row();
}

fn ui_header(ui: &mut Ui, cmd_tx: &Sender<Command>, app_state: &mut MutexGuard<AppState>) {
    ui.horizontal(|ui| {
        match app_state.error_state {
            Some(ErrorState::StartFailed) => {
                ui.label("Scan failed. Is bluetooth enabled?");
            },
            None => if app_state.scanner.is_active() {
                Spinner::default().ui(ui);
                ui.label("Scanning for base stations");
            } else {
                ui.label(format!("Found {} devices", app_state.device_entries.len()));
            }
        }

        ui.allocate_ui(ui.available_size(), |ui| {
            ui.with_layout(Layout::right_to_left(), |ui| {
                if ui
                    .add_enabled(!app_state.scanner.is_active(), egui::Button::new("🔃"))
                    .clicked()
                {
                    cmd_tx.blocking_send(Command::StartScan).ok();
                }
            });
        });
    });
}
