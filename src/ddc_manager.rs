use ddc_winapi::Monitor;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const VCP_BRIGHTNESS: u8 = 0x10;
pub const VCP_CONTRAST: u8 = 0x12;

/// UI-safe snapshot of a physical monitor's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub id: String,
    pub index: usize,
    pub display_name: String,
    pub brightness: u32,
    pub max_brightness: u32,
    pub contrast: u32,
    pub max_contrast: u32,
    /// False if the monitor is currently powering on/restarting or failed DDC/CI query.
    pub available: bool,
}

#[derive(Debug, Clone)]
pub enum DdcCommand {
    /// Re-enumerate physical monitors and read current VCP brightness/contrast.
    RefreshAll,
    /// Coalesced brightness update for a specific monitor ID.
    SetBrightness { id: String, value: u32 },
    /// Coalesced contrast update for a specific monitor ID.
    SetContrast { id: String, value: u32 },
    /// Shut down the background worker cleanly.
    Shutdown,
}

/// Thread-safe handle held by the Win32 UI thread.
#[derive(Clone)]
pub struct DdcManager {
    tx: Sender<DdcCommand>,
    state: Arc<Mutex<Vec<MonitorInfo>>>,
}

impl DdcManager {
    /// Spawns the background DDC worker and triggers an initial enumeration.
    pub fn new<F>(on_state_updated: F) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(Vec::new()));
        let worker_state = Arc::clone(&state);

        thread::Builder::new()
            .name("ddcci-worker".into())
            .spawn(move || {
                worker_loop(rx, worker_state, on_state_updated);
            })
            .expect("failed to spawn ddcci-worker thread");

        let _ = tx.send(DdcCommand::RefreshAll);
        Self { tx, state }
    }

    pub fn snapshots(&self) -> Vec<MonitorInfo> {
        self.state.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn request_refresh(&self) {
        let _ = self.tx.send(DdcCommand::RefreshAll);
    }

    pub fn set_brightness(&self, id: String, value: u32) {
        let _ = self.tx.send(DdcCommand::SetBrightness { id, value });
    }

    pub fn set_contrast(&self, id: String, value: u32) {
        let _ = self.tx.send(DdcCommand::SetContrast { id, value });
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(DdcCommand::Shutdown);
    }
}

/// Applies brightness/contrast from CLI arguments across all (or a selected) monitor safely without panicking.
pub fn apply_cli_commands(
    brightness: Option<u32>,
    contrast: Option<u32>,
    target_monitor: Option<usize>,
) {
    let monitors = match Monitor::enumerate() {
        Ok(list) => list,
        Err(e) => {
            eprintln!("Failed to enumerate DDC/CI monitors: {}", e);
            return;
        }
    };

    if monitors.is_empty() {
        eprintln!("No DDC/CI monitors detected.");
        return;
    }

    for (i, m) in monitors.iter().enumerate() {
        if let Some(target) = target_monitor {
            if i != target {
                continue;
            }
        }
        let desc = m.description();
        if let Some(b) = brightness {
            if let Err(e) = m.winapi_set_vcp_feature(VCP_BRIGHTNESS, b) {
                eprintln!(
                    "Monitor {} ({}): failed to set brightness to {}: {}",
                    i, desc, b, e
                );
            }
        }
        if let Some(c) = contrast {
            if let Err(e) = m.winapi_set_vcp_feature(VCP_CONTRAST, c) {
                eprintln!(
                    "Monitor {} ({}): failed to set contrast to {}: {}",
                    i, desc, c, e
                );
            }
        }
    }
}

struct ManagedMonitor {
    handle: Monitor,
    info: MonitorInfo,
}

#[derive(Default, Clone)]
struct DesiredState {
    brightness: Option<u32>,
    contrast: Option<u32>,
}

fn enumerate_monitors_safe(
    previous: &[MonitorInfo],
    desired_map: &mut HashMap<String, DesiredState>,
) -> Vec<ManagedMonitor> {
    let raw_monitors = match Monitor::enumerate() {
        Ok(list) => list,
        Err(_) => return Vec::new(),
    };

    let mut name_totals: HashMap<String, usize> = HashMap::new();
    let raw_names: Vec<String> = raw_monitors
        .iter()
        .map(|m| {
            let desc = m.description().trim().to_string();
            let base = if desc.is_empty() {
                "Generic Monitor".to_string()
            } else {
                desc
            };
            *name_totals.entry(base.clone()).or_insert(0) += 1;
            base
        })
        .collect();

    let mut seen_counts: HashMap<String, usize> = HashMap::new();
    let mut managed = Vec::with_capacity(raw_monitors.len());

    for (index, (monitor, base_name)) in raw_monitors.into_iter().zip(raw_names).enumerate() {
        let occurrence = {
            let count = seen_counts.entry(base_name.clone()).or_insert(0);
            *count += 1;
            *count
        };

        // Stable ID per model + occurrence index so identical monitors stay distinct
        let id = format!("{}#{}", base_name, occurrence);
        let display_name = if name_totals.get(&base_name).copied().unwrap_or(1) > 1 {
            format!("{} #{}", base_name, occurrence)
        } else {
            base_name
        };

        let prev = previous
            .iter()
            .find(|p| p.id == id)
            .or_else(|| previous.get(index));

        let (mut brightness, max_brightness, mut b_ok) =
            match monitor.winapi_get_vcp_feature_and_vcp_feature_reply(VCP_BRIGHTNESS) {
                Ok((_, cur, max)) => {
                    let safe_max = if max == 0 { 100 } else { max };
                    (cur.min(safe_max), safe_max, true)
                }
                Err(_) => (
                    prev.map(|p| p.brightness).unwrap_or(75),
                    prev.map(|p| p.max_brightness).unwrap_or(100),
                    false,
                ),
            };

        let (mut contrast, max_contrast, mut c_ok) =
            match monitor.winapi_get_vcp_feature_and_vcp_feature_reply(VCP_CONTRAST) {
                Ok((_, cur, max)) => {
                    let safe_max = if max == 0 { 100 } else { max };
                    (cur.min(safe_max), safe_max, true)
                }
                Err(_) => (
                    prev.map(|p| p.contrast).unwrap_or(75),
                    prev.map(|p| p.max_contrast).unwrap_or(100),
                    false,
                ),
            };

        // If user moved a slider while the monitor was restarting/offline, apply the desired value now
        if let Some(desired) = desired_map.get_mut(&id) {
            if let Some(target_b) = desired.brightness {
                let clamped = target_b.min(max_brightness);
                brightness = clamped;
                if b_ok {
                    if monitor.winapi_set_vcp_feature(VCP_BRIGHTNESS, clamped).is_ok() {
                        desired.brightness = None;
                    } else {
                        b_ok = false;
                    }
                }
            }
            if let Some(target_c) = desired.contrast {
                let clamped = target_c.min(max_contrast);
                contrast = clamped;
                if c_ok {
                    if monitor.winapi_set_vcp_feature(VCP_CONTRAST, clamped).is_ok() {
                        desired.contrast = None;
                    } else {
                        c_ok = false;
                    }
                }
            }
        }

        managed.push(ManagedMonitor {
            handle: monitor,
            info: MonitorInfo {
                id,
                index,
                display_name,
                brightness,
                max_brightness,
                contrast,
                max_contrast,
                available: b_ok || c_ok,
            },
        });
    }

    managed
}

fn next_backoff_duration(step: u8) -> Duration {
    match step {
        0 => Duration::from_millis(800),
        1 => Duration::from_millis(1500),
        2 => Duration::from_millis(2500),
        3 => Duration::from_millis(4000),
        4 => Duration::from_millis(8000),
        _ => Duration::from_millis(15000),
    }
}

fn worker_loop<F>(
    rx: Receiver<DdcCommand>,
    shared_state: Arc<Mutex<Vec<MonitorInfo>>>,
    notify_ui: F,
) where
    F: Fn(),
{
    let mut monitors: Vec<ManagedMonitor> = Vec::new();
    let mut desired_map: HashMap<String, DesiredState> = HashMap::new();
    let mut needs_recovery_poll = true;
    let mut backoff_step: u8 = 0;

    loop {
        let first_cmd = if needs_recovery_poll {
            let timeout = next_backoff_duration(backoff_step);
            match rx.recv_timeout(timeout) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Timeout) => {
                    backoff_step = backoff_step.saturating_add(1);
                    DdcCommand::RefreshAll
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            }
        };

        let mut do_refresh = false;
        let mut pending_brightness: HashMap<String, u32> = HashMap::new();
        let mut pending_contrast: HashMap<String, u32> = HashMap::new();

        let mut process_cmd = |cmd: DdcCommand| -> bool {
            match cmd {
                DdcCommand::Shutdown => return false,
                DdcCommand::RefreshAll => do_refresh = true,
                DdcCommand::SetBrightness { id, value } => {
                    pending_brightness.insert(id, value);
                }
                DdcCommand::SetContrast { id, value } => {
                    pending_contrast.insert(id, value);
                }
            }
            true
        };

        if !process_cmd(first_cmd) {
            break;
        }
        while let Ok(cmd) = rx.try_recv() {
            if !process_cmd(cmd) {
                return;
            }
        }

        let mut state_changed = false;

        // Record desired values and optimistically update snapshots so UI stays responsive
        if !pending_brightness.is_empty() || !pending_contrast.is_empty() {
            for (id, &val) in &pending_brightness {
                desired_map.entry(id.clone()).or_default().brightness = Some(val);
                if let Some(m) = monitors.iter_mut().find(|m| &m.info.id == id) {
                    m.info.brightness = val.min(m.info.max_brightness);
                }
            }
            for (id, &val) in &pending_contrast {
                desired_map.entry(id.clone()).or_default().contrast = Some(val);
                if let Some(m) = monitors.iter_mut().find(|m| &m.info.id == id) {
                    m.info.contrast = val.min(m.info.max_contrast);
                }
            }
            if let Ok(mut guard) = shared_state.lock() {
                *guard = monitors.iter().map(|m| m.info.clone()).collect();
            }
        }

        if do_refresh || monitors.is_empty() {
            let prev_infos: Vec<MonitorInfo> = monitors.iter().map(|m| m.info.clone()).collect();
            monitors = enumerate_monitors_safe(&prev_infos, &mut desired_map);
            state_changed = true;
        }

        // Apply latest coalesced brightness values to currently available monitors without blocking healthy ones
        let mut any_write_failed = false;
        for (id, val) in pending_brightness {
            if let Some(m) = monitors.iter_mut().find(|m| m.info.id == id) {
                let clamped = val.min(m.info.max_brightness);
                m.info.brightness = clamped;
                if m.info.available {
                    if m.handle.winapi_set_vcp_feature(VCP_BRIGHTNESS, clamped).is_ok() {
                        if let Some(d) = desired_map.get_mut(&id) {
                            d.brightness = None;
                        }
                    } else {
                        m.info.available = false;
                        any_write_failed = true;
                    }
                    state_changed = true;
                } else {
                    any_write_failed = true;
                }
            } else {
                any_write_failed = true;
            }
        }

        // Apply latest coalesced contrast values to currently available monitors
        for (id, val) in pending_contrast {
            if let Some(m) = monitors.iter_mut().find(|m| m.info.id == id) {
                let clamped = val.min(m.info.max_contrast);
                m.info.contrast = clamped;
                if m.info.available {
                    if m.handle.winapi_set_vcp_feature(VCP_CONTRAST, clamped).is_ok() {
                        if let Some(d) = desired_map.get_mut(&id) {
                            d.contrast = None;
                        }
                    } else {
                        m.info.available = false;
                        any_write_failed = true;
                    }
                    state_changed = true;
                } else {
                    any_write_failed = true;
                }
            } else {
                any_write_failed = true;
            }
        }

        if any_write_failed {
            backoff_step = 0;
        }

        needs_recovery_poll = monitors.is_empty() || monitors.iter().any(|m| !m.info.available);
        if !needs_recovery_poll {
            backoff_step = 0;
        }

        if state_changed {
            let new_snapshots: Vec<MonitorInfo> =
                monitors.iter().map(|m| m.info.clone()).collect();
            let mut should_notify = true;
            if let Ok(mut guard) = shared_state.lock() {
                if *guard == new_snapshots {
                    should_notify = do_refresh;
                } else {
                    *guard = new_snapshots;
                }
            }
            if should_notify {
                notify_ui();
            }
        }
    }
}
