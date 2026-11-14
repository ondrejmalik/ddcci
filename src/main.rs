#![windows_subsystem = "windows"]
#![allow(unsafe_op_in_unsafe_fn)]

mod ddc_manager;

use clap::Parser;
use ddc_manager::{apply_cli_commands, DdcManager, MonitorInfo};
use mimalloc::MiMalloc;
use std::time::{Duration, Instant};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{
            CreateFontW, CreateSolidBrush, DeleteObject, GetMonitorInfoW, MonitorFromPoint,
            SetBkColor, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
            DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, HBRUSH, HDC, HFONT, HGDIOBJ,
            MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS,
        },
        System::{
            Console::{AttachConsole, ATTACH_PARENT_PROCESS},
            LibraryLoader::GetModuleHandleW,
        },
        UI::{
            Controls::{
                InitCommonControlsEx, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, TBM_SETPOS,
                TBM_SETRANGEMAX, TBM_SETRANGEMIN, TBM_SETTICFREQ, TBS_AUTOTICKS, TBS_HORZ,
                TBS_TOOLTIPS, TRACKBAR_CLASSW,
            },
            HiDpi::{
                GetDpiForWindow, SetProcessDpiAwarenessContext,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NIM_MODIFY, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW,
                GetWindowLongPtrW, IsWindowVisible, KillTimer, LoadCursorW, LoadIconW,
                LoadImageW, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassExW,
                RegisterWindowMessageW, SendMessageW, SetForegroundWindow, SetTimer,
                SetWindowLongPtrW, SetWindowTextW, ShowWindow, TrackPopupMenu, TranslateMessage,
                BS_CENTER, BS_PUSHBUTTON, BS_VCENTER, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
                GWLP_USERDATA, HICON, HMENU, IDC_ARROW, IDI_APPLICATION, IMAGE_ICON,
                LR_DEFAULTCOLOR, MF_SEPARATOR, MF_STRING, MSG, SW_HIDE, SW_SHOW, TPM_BOTTOMALIGN,
                TPM_LEFTALIGN, TPM_RIGHTBUTTON, WA_INACTIVE, WINDOW_STYLE, WM_ACTIVATE, WM_APP,
                WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DEVICECHANGE,
                WM_DISPLAYCHANGE, WM_DPICHANGED, WM_HSCROLL, WM_KEYDOWN, WM_LBUTTONDBLCLK,
                WM_LBUTTONUP, WM_RBUTTONUP, WM_SETFONT, WM_TIMER, WM_USER, WNDCLASSEXW, WS_BORDER,
                WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
            },
        },
    },
};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const WM_TRAYICON: u32 = WM_APP + 1;
const WM_APP_DDC_UPDATED: u32 = WM_APP + 2;
const TRAY_ICON_ID: u32 = 1;

const TBM_GETPOS: u32 = WM_USER;
const TB_THUMBTRACK: u32 = 5;
const TB_ENDTRACK: u32 = 8;

const TIMER_DISPLAY_SETTLE_1: usize = 1;
const TIMER_DISPLAY_SETTLE_2: usize = 2;

const IDM_OPEN_SLIDERS: usize = 1001;
const IDM_REFRESH_MONITORS: usize = 1002;
const IDM_QUIT: usize = 1003;
const IDM_CLOSE_FLYOUT: usize = 1004;

#[derive(Parser, Debug)]
#[command(version, about = "DDC/CI multi-monitor brightness & contrast control", long_about = None)]
struct Args {
    /// Set brightness (0-100)
    #[arg(short, long)]
    brightness: Option<u32>,

    /// Set contrast (0-100)
    #[arg(short, long)]
    contrast: Option<u32>,

    /// Optional 0-based monitor index for CLI commands (defaults to all monitors)
    #[arg(short, long)]
    monitor: Option<usize>,

    /// Run in the system tray with native Windows sliders
    #[arg(long, short)]
    tray: bool,
}

fn main() {
    // Attach to parent console if invoked from PowerShell/cmd so CLI flags (--help, -b, -c) print output
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }

    let args = Args::parse();

    if args.brightness.is_some() || args.contrast.is_some() {
        apply_cli_commands(args.brightness, args.contrast, args.monitor);
    }

    // Run tray mode if --tray is passed OR if no CLI brightness/contrast action was given
    if args.tray || (args.brightness.is_none() && args.contrast.is_none()) {
        if let Err(e) = run_tray_ui() {
            eprintln!("Tray UI error: {}", e);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlKind {
    Brightness,
    Contrast,
}

struct SliderBinding {
    monitor_id: String,
    kind: ControlKind,
    slider_hwnd: HWND,
    value_label_hwnd: HWND,
    last_value: u32,
    max_value: u32,
    is_dragging: bool,
}

struct MonitorHeaderBinding {
    monitor_id: String,
    monitor_index: usize,
    header_hwnd: HWND,
    base_name: String,
    available: bool,
}

struct FlyoutState {
    hinstance: HINSTANCE,
    tray_hicon: HICON,
    bg_brush: HBRUSH,
    font_regular: HFONT,
    font_bold: HFONT,
    font_close: HFONT,
    dpi: u32,
    taskbar_created_msg: u32,
    child_hwnds: Vec<HWND>,
    headers: Vec<MonitorHeaderBinding>,
    bindings: Vec<SliderBinding>,
    rendered_topology: Vec<String>,
    flyout_width: i32,
    flyout_height: i32,
    last_popup_pos: Option<(i32, i32)>,
    last_deactivated_at: Option<Instant>,
    ddc: DdcManager,
}

#[inline]
fn scale_dpi(px: i32, dpi: u32) -> i32 {
    (px * dpi as i32 + 48) / 96
}

unsafe fn create_fonts_for_dpi(dpi: u32) -> (HFONT, HFONT, HFONT) {
    let font_height = -scale_dpi(13, dpi);
    let close_height = -scale_dpi(18, dpi);
    let regular = CreateFontW(
        font_height,
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        w!("Segoe UI"),
    );
    let bold = CreateFontW(
        font_height,
        0,
        0,
        0,
        FW_SEMIBOLD.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        w!("Segoe UI"),
    );
    let close = CreateFontW(
        close_height,
        0,
        0,
        0,
        700,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        w!("Segoe UI"),
    );
    (regular, bold, close)
}

fn run_tray_ui() -> windows::core::Result<()> {
    unsafe {
        let class_name = w!("DdcciFlyoutWindowClass");

        // If an instance is already running, tell it to open the sliders and exit cleanly
        if let Ok(existing_hwnd) = FindWindowW(class_name, None) {
            if !existing_hwnd.is_invalid() && !existing_hwnd.0.is_null() {
                let _ = PostMessageW(
                    Some(existing_hwnd),
                    WM_COMMAND,
                    WPARAM(IDM_OPEN_SLIDERS),
                    LPARAM(0),
                );
                return Ok(());
            }
        }

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);

        let hinstance: HINSTANCE = GetModuleHandleW(None)?.into();

        // Clean light-gray surface matching standard Win32 Common Controls trackbar background
        let bg_brush = CreateSolidBrush(COLORREF(0x00F3F3F3));

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: bg_brush,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class_name,
            w!("DDC/CI Brightness & Contrast"),
            WS_POPUP | WS_BORDER,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            360,
            130,
            None,
            None,
            Some(hinstance),
            None,
        )?;

        let raw_dpi = GetDpiForWindow(hwnd);
        let dpi = if raw_dpi == 0 { 96 } else { raw_dpi };
        let (font_regular, font_bold, font_close) = create_fonts_for_dpi(dpi);
        let tray_hicon = load_tray_hicon(hinstance);
        let taskbar_created_msg = RegisterWindowMessageW(w!("TaskbarCreated"));

        let hwnd_raw = hwnd.0 as isize;
        let ddc = DdcManager::new(move || {
            let notify_hwnd = HWND(hwnd_raw as *mut std::ffi::c_void);
            let _ = PostMessageW(Some(notify_hwnd), WM_APP_DDC_UPDATED, WPARAM(0), LPARAM(0));
        });

        let state = Box::new(FlyoutState {
            hinstance,
            tray_hicon,
            bg_brush,
            font_regular,
            font_bold,
            font_close,
            dpi,
            taskbar_created_msg,
            child_hwnds: Vec::new(),
            headers: Vec::new(),
            bindings: Vec::new(),
            rendered_topology: Vec::new(),
            flyout_width: scale_dpi(360, dpi),
            flyout_height: scale_dpi(130, dpi),
            last_popup_pos: None,
            last_deactivated_at: None,
            ddc,
        });

        let initial_monitors = state.ddc.snapshots();
        let tray_hicon_copy = state.tray_hicon;
        let state_ptr = Box::into_raw(state);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);

        rebuild_monitor_controls(hwnd, &mut *state_ptr, &initial_monitors);
        add_or_update_tray_icon(
            hwnd,
            tray_hicon_copy,
            "DDC/CI Brightness & Contrast",
            false,
        )?;
        position_and_show_flyout(hwnd, &mut *state_ptr);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        Ok(())
    }
}

unsafe fn load_tray_hicon(hinstance: HINSTANCE) -> HICON {
    if let Ok(h) = LoadImageW(
        Some(hinstance),
        w!("app-icon"),
        IMAGE_ICON,
        64,
        64,
        LR_DEFAULTCOLOR,
    ) {
        if !h.is_invalid() && !h.0.is_null() {
            return HICON(h.0);
        }
    }
    if let Ok(icon) = LoadIconW(Some(hinstance), w!("app-icon")) {
        if !icon.is_invalid() && !icon.0.is_null() {
            return icon;
        }
    }
    LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
}

unsafe fn add_or_update_tray_icon(
    hwnd: HWND,
    hicon: HICON,
    tooltip: &str,
    modify: bool,
) -> windows::core::Result<()> {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: hicon,
        ..Default::default()
    };

    let max_len = nid.szTip.len() - 1;
    for (dst, src) in nid.szTip.iter_mut().take(max_len).zip(tooltip.encode_utf16()) {
        *dst = src;
    }

    if modify {
        if !Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        }
    } else if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
    Ok(())
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
}

fn format_tray_tooltip(monitors: &[MonitorInfo]) -> String {
    if monitors.is_empty() {
        return "DDC/CI: No monitors detected".to_string();
    }
    let parts: Vec<String> = monitors
        .iter()
        .map(|m| {
            if m.available {
                format!("{}: B {}% C {}%", m.index + 1, m.brightness, m.contrast)
            } else {
                format!("{}: Syncing...", m.index + 1)
            }
        })
        .collect();
    parts.join(" | ")
}

unsafe fn position_and_show_flyout(hwnd: HWND, state: &mut FlyoutState) {
    let width = state.flyout_width;
    let height = state.flyout_height;

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(hmon, &mut mi);
    let work = mi.rcWork;

    let margin = scale_dpi(10, state.dpi);
    let mut x = pt.x - (width / 2);
    if x + width > work.right - margin {
        x = work.right - width - margin;
    }
    if x < work.left + margin {
        x = work.left + margin;
    }

    let mut y = if pt.y > (work.top + work.bottom) / 2 {
        work.bottom - height - margin
    } else {
        work.top + margin
    };
    if y + height > work.bottom - margin {
        y = work.bottom - height - margin;
    }
    if y < work.top + margin {
        y = work.top + margin;
    }

    state.last_popup_pos = Some((x, y));
    let _ = MoveWindow(hwnd, x, y, width, height, true);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
}

unsafe fn resize_visible_flyout(hwnd: HWND, state: &mut FlyoutState) {
    if !IsWindowVisible(hwnd).as_bool() {
        return;
    }
    let width = state.flyout_width;
    let height = state.flyout_height;

    if let Some((x, y)) = state.last_popup_pos {
        let pt = POINT { x, y };
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(hmon, &mut mi);
        let work = mi.rcWork;
        let margin = scale_dpi(10, state.dpi);

        let mut new_y = y;
        if new_y + height > work.bottom - margin {
            new_y = (work.bottom - height - margin).max(work.top + margin);
        }
        state.last_popup_pos = Some((x, new_y));
        let _ = MoveWindow(hwnd, x, new_y, width, height, true);
    } else {
        position_and_show_flyout(hwnd, state);
    }
}

unsafe fn show_tray_context_menu(hwnd: HWND) {
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    if let Ok(hmenu) = CreatePopupMenu() {
        let _ = AppendMenuW(hmenu, MF_STRING, IDM_OPEN_SLIDERS, w!("Open Sliders"));
        let _ = AppendMenuW(hmenu, MF_STRING, IDM_REFRESH_MONITORS, w!("Refresh Monitors"));
        let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(hmenu, MF_STRING, IDM_QUIT, w!("Quit"));

        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            hmenu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(hmenu);
    }
}

fn format_header_text(index: usize, name: &str, available: bool) -> String {
    if available {
        format!("Monitor {}: {}", index + 1, name)
    } else {
        format!("Monitor {}: {} (Syncing...)", index + 1, name)
    }
}

unsafe fn sync_or_rebuild_controls(hwnd: HWND, state: &mut FlyoutState, monitors: &[MonitorInfo]) {
    let new_topology: Vec<String> = monitors.iter().map(|m| m.id.clone()).collect();

    if state.rendered_topology != new_topology || (monitors.is_empty() && state.bindings.is_empty()) {
        rebuild_monitor_controls(hwnd, state, monitors);
        resize_visible_flyout(hwnd, state);
        return;
    }

    // Update headers and sliders in-place so active mouse drags are never interrupted
    for hdr in &mut state.headers {
        if let Some(mon) = monitors.iter().find(|m| m.id == hdr.monitor_id) {
            if hdr.available != mon.available || hdr.monitor_index != mon.index {
                hdr.available = mon.available;
                hdr.monitor_index = mon.index;
                let text = format_header_text(mon.index, &hdr.base_name, mon.available);
                set_window_text(hdr.header_hwnd, &text);
            }
        }
    }

    for binding in &mut state.bindings {
        if binding.is_dragging {
            continue;
        }
        if let Some(mon) = monitors.iter().find(|m| m.id == binding.monitor_id) {
            let (target_val, target_max) = match binding.kind {
                ControlKind::Brightness => (mon.brightness, mon.max_brightness.max(1)),
                ControlKind::Contrast => (mon.contrast, mon.max_contrast.max(1)),
            };

            if binding.max_value != target_max {
                binding.max_value = target_max;
                SendMessageW(
                    binding.slider_hwnd,
                    TBM_SETRANGEMAX,
                    Some(WPARAM(1)),
                    Some(LPARAM(target_max as isize)),
                );
            }

            if binding.last_value != target_val {
                binding.last_value = target_val;
                SendMessageW(
                    binding.slider_hwnd,
                    TBM_SETPOS,
                    Some(WPARAM(1)),
                    Some(LPARAM(target_val as isize)),
                );
                set_window_text(binding.value_label_hwnd, &format!("{}%", target_val));
            }
        }
    }
}

unsafe fn rebuild_monitor_controls(
    hwnd: HWND,
    state: &mut FlyoutState,
    monitors: &[MonitorInfo],
) {
    for child in state.child_hwnds.drain(..) {
        let _ = DestroyWindow(child);
    }
    state.headers.clear();
    state.bindings.clear();
    state.rendered_topology = monitors.iter().map(|m| m.id.clone()).collect();

    let dpi = state.dpi;
    let width = scale_dpi(360, dpi);
    let pad = scale_dpi(16, dpi);
    let close_w = scale_dpi(34, dpi);
    let close_h = scale_dpi(28, dpi);
    let mut y = pad;

    if let Some(close_btn) = create_close_button(
        hwnd,
        state.hinstance,
        width - pad - close_w,
        scale_dpi(10, dpi),
        close_w,
        close_h,
        state.font_close,
    ) {
        state.child_hwnds.push(close_btn);
    }

    if monitors.is_empty() {
        let empty_h = scale_dpi(44, dpi);
        if let Some(lbl) = create_static(
            hwnd,
            state.hinstance,
            "Waiting for DDC/CI monitor...\n(If monitor is restarting, it will reconnect automatically)",
            pad,
            y,
            width - pad * 2 - close_w - scale_dpi(8, dpi),
            empty_h,
            state.font_regular,
        ) {
            state.child_hwnds.push(lbl);
        }
        state.flyout_width = width;
        state.flyout_height = y + empty_h + pad;
        return;
    }

    let hdr_h = scale_dpi(22, dpi);
    let hdr_step = scale_dpi(28, dpi);
    let row_step = scale_dpi(34, dpi);
    let monitor_gap = scale_dpi(10, dpi);

    for (idx, mon) in monitors.iter().enumerate() {
        let header_text = format_header_text(mon.index, &mon.display_name, mon.available);
        let hdr_w = if idx == 0 {
            width - pad * 2 - close_w - scale_dpi(8, dpi)
        } else {
            width - pad * 2
        };
        if let Some(header) = create_static(
            hwnd,
            state.hinstance,
            &header_text,
            pad,
            y,
            hdr_w,
            hdr_h,
            state.font_bold,
        ) {
            state.child_hwnds.push(header);
            state.headers.push(MonitorHeaderBinding {
                monitor_id: mon.id.clone(),
                monitor_index: mon.index,
                header_hwnd: header,
                base_name: mon.display_name.clone(),
                available: mon.available,
            });
        }
        y += hdr_step;

        let max_b = mon.max_brightness.max(1);
        let b_val = mon.brightness.min(max_b);
        if let Some((b_slider, b_val_lbl)) = create_slider_row(
            hwnd,
            state,
            "Brightness",
            b_val,
            max_b,
            pad,
            y,
            width,
        ) {
            state.bindings.push(SliderBinding {
                monitor_id: mon.id.clone(),
                kind: ControlKind::Brightness,
                slider_hwnd: b_slider,
                value_label_hwnd: b_val_lbl,
                last_value: b_val,
                max_value: max_b,
                is_dragging: false,
            });
        }
        y += row_step;

        let max_c = mon.max_contrast.max(1);
        let c_val = mon.contrast.min(max_c);
        if let Some((c_slider, c_val_lbl)) = create_slider_row(
            hwnd,
            state,
            "Contrast",
            c_val,
            max_c,
            pad,
            y,
            width,
        ) {
            state.bindings.push(SliderBinding {
                monitor_id: mon.id.clone(),
                kind: ControlKind::Contrast,
                slider_hwnd: c_slider,
                value_label_hwnd: c_val_lbl,
                last_value: c_val,
                max_value: max_c,
                is_dragging: false,
            });
        }
        y += row_step;

        if idx + 1 < monitors.len() {
            y += monitor_gap;
        }
    }

    state.flyout_width = width;
    state.flyout_height = y + pad;
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_slider_row(
    parent: HWND,
    state: &mut FlyoutState,
    label: &str,
    initial_val: u32,
    max_val: u32,
    pad: i32,
    y: i32,
    total_width: i32,
) -> Option<(HWND, HWND)> {
    let dpi = state.dpi;
    let label_w = scale_dpi(74, dpi);
    let val_w = scale_dpi(42, dpi);
    let gap = scale_dpi(4, dpi);
    let slider_w = total_width - pad * 2 - label_w - val_w - gap * 2;
    let text_y_offset = scale_dpi(5, dpi);
    let text_h = scale_dpi(20, dpi);
    let slider_h = scale_dpi(30, dpi);

    let name_hwnd = create_static(
        parent,
        state.hinstance,
        label,
        pad,
        y + text_y_offset,
        label_w,
        text_h,
        state.font_regular,
    )?;
    state.child_hwnds.push(name_hwnd);

    let slider_hwnd = CreateWindowExW(
        Default::default(),
        TRACKBAR_CLASSW,
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(TBS_HORZ | TBS_AUTOTICKS | TBS_TOOLTIPS),
        pad + label_w + gap,
        y,
        slider_w,
        slider_h,
        Some(parent),
        None,
        Some(state.hinstance),
        None,
    )
    .ok()?;

    SendMessageW(
        slider_hwnd,
        TBM_SETRANGEMIN,
        Some(WPARAM(0)),
        Some(LPARAM(0)),
    );
    SendMessageW(
        slider_hwnd,
        TBM_SETRANGEMAX,
        Some(WPARAM(1)),
        Some(LPARAM(max_val as isize)),
    );
    SendMessageW(
        slider_hwnd,
        TBM_SETTICFREQ,
        Some(WPARAM(10)),
        Some(LPARAM(0)),
    );
    SendMessageW(
        slider_hwnd,
        TBM_SETPOS,
        Some(WPARAM(1)),
        Some(LPARAM(initial_val as isize)),
    );
    state.child_hwnds.push(slider_hwnd);

    let val_text = format!("{}%", initial_val);
    let val_hwnd = create_static(
        parent,
        state.hinstance,
        &val_text,
        pad + label_w + slider_w + gap * 2,
        y + text_y_offset,
        val_w,
        text_h,
        state.font_regular,
    )?;
    state.child_hwnds.push(val_hwnd);

    Some((slider_hwnd, val_hwnd))
}

unsafe fn create_close_button(
    parent: HWND,
    hinstance: HINSTANCE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    font: HFONT,
) -> Option<HWND> {
    let hwnd = CreateWindowExW(
        Default::default(),
        w!("BUTTON"),
        w!("✕"),
        WS_CHILD
            | WS_VISIBLE
            | WINDOW_STYLE((BS_PUSHBUTTON | BS_CENTER | BS_VCENTER) as u32),
        x,
        y,
        w,
        h,
        Some(parent),
        Some(HMENU(IDM_CLOSE_FLYOUT as *mut std::ffi::c_void)),
        Some(hinstance),
        None,
    )
    .ok()?;

    SendMessageW(
        hwnd,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
    Some(hwnd)
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_static(
    parent: HWND,
    hinstance: HINSTANCE,
    text: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    font: HFONT,
) -> Option<HWND> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = CreateWindowExW(
        Default::default(),
        w!("STATIC"),
        PCWSTR(wide.as_ptr()),
        WS_CHILD | WS_VISIBLE,
        x,
        y,
        w,
        h,
        Some(parent),
        None,
        Some(hinstance),
        None,
    )
    .ok()?;

    SendMessageW(
        hwnd,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
    Some(hwnd)
}

unsafe fn set_window_text(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FlyoutState;

    if !state_ptr.is_null() {
        let state = &*state_ptr;
        if state.taskbar_created_msg != 0 && msg == state.taskbar_created_msg {
            let monitors = state.ddc.snapshots();
            let tip = format_tray_tooltip(&monitors);
            let _ = add_or_update_tray_icon(hwnd, state.tray_hicon, &tip, false);
            return LRESULT(0);
        }
    }

    match msg {
        WM_ACTIVATE => {
            let active_state = (wparam.0 & 0xFFFF) as u32;
            if active_state == WA_INACTIVE {
                if !state_ptr.is_null() {
                    let state = &mut *state_ptr;
                    for b in &mut state.bindings {
                        b.is_dragging = false;
                    }
                    if IsWindowVisible(hwnd).as_bool() {
                        state.last_deactivated_at = Some(Instant::now());
                    }
                }
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }

        WM_DPICHANGED => {
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                let new_dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
                if new_dpi > 0 && new_dpi != state.dpi {
                    state.dpi = new_dpi;
                    let old_reg = state.font_regular;
                    let old_bold = state.font_bold;
                    let old_close = state.font_close;
                    let (new_reg, new_bold, new_close) = create_fonts_for_dpi(new_dpi);
                    state.font_regular = new_reg;
                    state.font_bold = new_bold;
                    state.font_close = new_close;
                    let monitors = state.ddc.snapshots();
                    rebuild_monitor_controls(hwnd, state, &monitors);
                    resize_visible_flyout(hwnd, state);
                    let _ = DeleteObject(HGDIOBJ(old_reg.0));
                    let _ = DeleteObject(HGDIOBJ(old_bold.0));
                    let _ = DeleteObject(HGDIOBJ(old_close.0));
                }
            }
            LRESULT(0)
        }

        WM_KEYDOWN => {
            // Close on Escape key
            if wparam.0 == 0x1B {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }

        WM_HSCROLL => {
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                let scroll_code = (wparam.0 & 0xFFFF) as u32;
                let slider_hwnd = HWND(lparam.0 as *mut std::ffi::c_void);

                if let Some(binding) = state
                    .bindings
                    .iter_mut()
                    .find(|b| b.slider_hwnd == slider_hwnd)
                {
                    if scroll_code == TB_THUMBTRACK {
                        binding.is_dragging = true;
                    } else if scroll_code == TB_ENDTRACK {
                        binding.is_dragging = false;
                    }

                    let pos = SendMessageW(slider_hwnd, TBM_GETPOS, None, None).0 as u32;
                    if pos != binding.last_value {
                        binding.last_value = pos;
                        set_window_text(binding.value_label_hwnd, &format!("{}%", pos));

                        let id = binding.monitor_id.clone();
                        match binding.kind {
                            ControlKind::Brightness => {
                                state.ddc.set_brightness(id, pos);
                            }
                            ControlKind::Contrast => {
                                state.ddc.set_contrast(id, pos);
                            }
                        }
                    }
                }
            }
            LRESULT(0)
        }

        WM_APP_DDC_UPDATED => {
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                let monitors = state.ddc.snapshots();
                let tip = format_tray_tooltip(&monitors);
                let _ = add_or_update_tray_icon(hwnd, state.tray_hicon, &tip, true);
                sync_or_rebuild_controls(hwnd, state, &monitors);
            }
            LRESULT(0)
        }

        WM_DISPLAYCHANGE | WM_DEVICECHANGE => {
            if !state_ptr.is_null() {
                let _ = SetTimer(Some(hwnd), TIMER_DISPLAY_SETTLE_1, 600, None);
                let _ = SetTimer(Some(hwnd), TIMER_DISPLAY_SETTLE_2, 2500, None);
            }
            LRESULT(0)
        }

        WM_TIMER => {
            if !state_ptr.is_null() {
                let timer_id = wparam.0;
                if timer_id == TIMER_DISPLAY_SETTLE_1 || timer_id == TIMER_DISPLAY_SETTLE_2 {
                    let _ = KillTimer(Some(hwnd), timer_id);
                    let state = &mut *state_ptr;
                    state.ddc.request_refresh();
                }
            }
            LRESULT(0)
        }

        WM_CTLCOLORSTATIC => {
            if !state_ptr.is_null() {
                let state = &*state_ptr;
                let hdc = HDC(wparam.0 as *mut std::ffi::c_void);
                SetBkColor(hdc, COLORREF(0x00F3F3F3));
                SetTextColor(hdc, COLORREF(0x001A1A1A));
                return LRESULT(state.bg_brush.0 as isize);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        WM_TRAYICON => {
            let event = (lparam.0 & 0xFFFF) as u32;
            match event {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    if !state_ptr.is_null() {
                        let state = &mut *state_ptr;
                        if IsWindowVisible(hwnd).as_bool() {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        } else {
                            // Prevent immediate reopen if clicking the tray icon just deactivated the open flyout
                            let recently_deactivated = state
                                .last_deactivated_at
                                .map(|t| t.elapsed() < Duration::from_millis(250))
                                .unwrap_or(false);
                            state.last_deactivated_at = None;

                            if !recently_deactivated {
                                let snapshots = state.ddc.snapshots();
                                sync_or_rebuild_controls(hwnd, state, &snapshots);
                                position_and_show_flyout(hwnd, state);
                                state.ddc.request_refresh();
                            }
                        }
                    }
                }
                WM_RBUTTONUP => {
                    show_tray_context_menu(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }

        WM_COMMAND => {
            let cmd_id = wparam.0 & 0xFFFF;
            match cmd_id {
                IDM_OPEN_SLIDERS => {
                    if !state_ptr.is_null() {
                        let state = &mut *state_ptr;
                        let snapshots = state.ddc.snapshots();
                        sync_or_rebuild_controls(hwnd, state, &snapshots);
                        position_and_show_flyout(hwnd, state);
                        state.ddc.request_refresh();
                    }
                }
                IDM_REFRESH_MONITORS => {
                    if !state_ptr.is_null() {
                        let state = &mut *state_ptr;
                        state.ddc.request_refresh();
                    }
                }
                IDM_CLOSE_FLYOUT => {
                    if !state_ptr.is_null() {
                        let state = &mut *state_ptr;
                        for b in &mut state.bindings {
                            b.is_dragging = false;
                        }
                    }
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                IDM_QUIT => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }

        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }

        WM_DESTROY => {
            remove_tray_icon(hwnd);
            if !state_ptr.is_null() {
                let state = Box::from_raw(state_ptr);
                state.ddc.shutdown();
                let _ = DeleteObject(HGDIOBJ(state.font_regular.0));
                let _ = DeleteObject(HGDIOBJ(state.font_bold.0));
                let _ = DeleteObject(HGDIOBJ(state.font_close.0));
                let _ = DeleteObject(HGDIOBJ(state.bg_brush.0));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
