#![windows_subsystem = "windows"]
#![allow(unsafe_op_in_unsafe_fn)]

mod ddc_manager;

use clap::Parser;
use ddc_manager::{DdcManager, MonitorInfo, apply_cli_commands};
use mimalloc::MiMalloc;
use std::time::{Duration, Instant};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::{
                DWM_WINDOW_CORNER_PREFERENCE, DWMWA_USE_IMMERSIVE_DARK_MODE,
                DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
            },
            Gdi::{
                CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
                DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD,
                FillRect, GetMonitorInfoW, HBRUSH, HDC, HFONT, HGDIOBJ, InvalidateRect,
                MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, OUT_DEFAULT_PRECIS,
                SetBkColor, SetTextColor,
            },
        },
        System::{
            Console::{ATTACH_PARENT_PROCESS, AttachConsole},
            LibraryLoader::GetModuleHandleW,
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, RegCloseKey, RegCreateKeyW,
                RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
            },
        },
        UI::{
            Controls::{
                ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TBM_SETPOS,
                TBM_SETRANGEMAX, TBM_SETRANGEMIN, TBS_HORZ, TBS_NOTICKS, TRACKBAR_CLASSW,
            },
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                SetProcessDpiAwarenessContext,
            },
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreatePopupMenu,
                CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
                FindWindowW, GWLP_USERDATA, GetClientRect, GetCursorPos, GetMessageW,
                GetWindowLongPtrW, HICON, IDC_ARROW, IDI_APPLICATION, IMAGE_ICON, IsWindowVisible,
                KillTimer, LR_DEFAULTCOLOR, LoadCursorW, LoadIconW, LoadImageW, MENU_ITEM_FLAGS,
                MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, MoveWindow, PostMessageW,
                PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SW_HIDE, SW_SHOW,
                SendMessageW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowTextW,
                ShowWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
                TranslateMessage, WA_INACTIVE, WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_CLOSE,
                WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DEVICECHANGE, WM_DISPLAYCHANGE,
                WM_DPICHANGED, WM_ERASEBKGND, WM_HSCROLL, WM_KEYDOWN, WM_LBUTTONDBLCLK,
                WM_LBUTTONUP, WM_RBUTTONUP, WM_SETFONT, WM_SETTINGCHANGE, WM_TIMER, WM_USER,
                WNDCLASSEXW, WS_BORDER, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
                WS_VISIBLE,
            },
        },
    },
    core::{PCWSTR, w},
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
const IDM_TOGGLE_CONTRAST: usize = 1004;
const IDM_TOGGLE_DARK_THEME: usize = 1005;

// SS_RIGHT and SS_ETCHEDHORZ for native Win32 static control alignment and subtle dividers
const SS_RIGHT_STYLE: u32 = 0x0000_0002;
const SS_ETCHEDHORZ_STYLE: u32 = 0x0000_0010;

// Clean light & dark surface palettes (BGR COLORREF)
const LIGHT_SURFACE_BG: COLORREF = COLORREF(0x00F3F3F3);
const LIGHT_TEXT_PRIMARY: COLORREF = COLORREF(0x001A1A1A);
const LIGHT_TEXT_MUTED: COLORREF = COLORREF(0x006E6E6E);

const DARK_SURFACE_BG: COLORREF = COLORREF(0x00202020);
const DARK_TEXT_PRIMARY: COLORREF = COLORREF(0x00F5F5F5);
const DARK_TEXT_MUTED: COLORREF = COLORREF(0x00A0A0A0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeMode {
    System = 0,
    Dark = 1,
    Light = 2,
}

impl ThemeMode {
    fn from_u32(val: u32) -> Self {
        match val {
            1 => ThemeMode::Dark,
            2 => ThemeMode::Light,
            _ => ThemeMode::System,
        }
    }
}

#[derive(Clone, Copy)]
struct ThemePalette {
    is_dark: bool,
    surface_bg: COLORREF,
    text_primary: COLORREF,
    text_muted: COLORREF,
}

impl ThemePalette {
    fn for_dark(is_dark: bool) -> Self {
        if is_dark {
            Self {
                is_dark: true,
                surface_bg: DARK_SURFACE_BG,
                text_primary: DARK_TEXT_PRIMARY,
                text_muted: DARK_TEXT_MUTED,
            }
        } else {
            Self {
                is_dark: false,
                surface_bg: LIGHT_SURFACE_BG,
                text_primary: LIGHT_TEXT_PRIMARY,
                text_muted: LIGHT_TEXT_MUTED,
            }
        }
    }
}

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
    available: bool,
    is_dragging: bool,
}

struct MonitorHeaderBinding {
    monitor_id: String,
    header_hwnd: HWND,
    base_name: String,
    available: bool,
}

struct FlyoutState {
    hinstance: HINSTANCE,
    tray_hicon: HICON,
    bg_brush: HBRUSH,
    theme_mode: ThemeMode,
    palette: ThemePalette,
    font_regular: HFONT,
    font_bold: HFONT,
    dpi: u32,
    taskbar_created_msg: u32,
    show_contrast: bool,
    child_hwnds: Vec<HWND>,
    muted_hwnds: Vec<HWND>,
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

/// Checks Windows Personalize registry to see if Dark Mode is enabled (AppsUseLightTheme == 0 or SystemUsesLightTheme == 0).
unsafe fn is_windows_dark_theme_enabled() -> bool {
    let mut hkey = HKEY::default();
    let subkey = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    if RegOpenKeyExW(HKEY_CURRENT_USER, subkey, Some(0), KEY_READ, &mut hkey).is_ok() {
        let mut data: u32 = 1;
        let mut data_len: u32 = std::mem::size_of::<u32>() as u32;
        let res = RegQueryValueExW(
            hkey,
            w!("AppsUseLightTheme"),
            None,
            None,
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut data_len),
        );
        let _ = RegCloseKey(hkey);
        if res.is_ok() {
            return data == 0;
        }
    }
    false
}

/// Loads saved user preferences (`ThemeMode` and `ShowContrast`) from `HKCU\Software\ddcci`.
unsafe fn load_user_preferences() -> (ThemeMode, bool) {
    let mut hkey = HKEY::default();
    let mut theme_mode = ThemeMode::System;
    let mut show_contrast = true;

    if RegOpenKeyExW(
        HKEY_CURRENT_USER,
        w!("Software\\ddcci"),
        Some(0),
        KEY_READ,
        &mut hkey,
    )
    .is_ok()
    {
        let mut data: u32 = 0;
        let mut data_len = std::mem::size_of::<u32>() as u32;
        if RegQueryValueExW(
            hkey,
            w!("ThemeMode"),
            None,
            None,
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut data_len),
        )
        .is_ok()
        {
            theme_mode = ThemeMode::from_u32(data);
        }

        let mut contrast_data: u32 = 1;
        let mut contrast_len = std::mem::size_of::<u32>() as u32;
        if RegQueryValueExW(
            hkey,
            w!("ShowContrast"),
            None,
            None,
            Some(&mut contrast_data as *mut u32 as *mut u8),
            Some(&mut contrast_len),
        )
        .is_ok()
        {
            show_contrast = contrast_data != 0;
        }

        let _ = RegCloseKey(hkey);
    }

    (theme_mode, show_contrast)
}

/// Persists user preferences (`ThemeMode` and `ShowContrast`) to `HKCU\Software\ddcci`.
unsafe fn save_user_preferences(theme_mode: ThemeMode, show_contrast: bool) {
    let mut hkey = HKEY::default();
    if RegCreateKeyW(HKEY_CURRENT_USER, w!("Software\\ddcci"), &mut hkey).is_ok() {
        let theme_val: u32 = theme_mode as u32;
        let theme_bytes = std::slice::from_raw_parts(
            &theme_val as *const u32 as *const u8,
            std::mem::size_of::<u32>(),
        );
        let _ = RegSetValueExW(hkey, w!("ThemeMode"), Some(0), REG_DWORD, Some(theme_bytes));

        let contrast_val: u32 = u32::from(show_contrast);
        let contrast_bytes = std::slice::from_raw_parts(
            &contrast_val as *const u32 as *const u8,
            std::mem::size_of::<u32>(),
        );
        let _ = RegSetValueExW(
            hkey,
            w!("ShowContrast"),
            Some(0),
            REG_DWORD,
            Some(contrast_bytes),
        );

        let _ = RegCloseKey(hkey);
    }
}

unsafe fn resolve_palette(mode: ThemeMode) -> ThemePalette {
    let is_dark = match mode {
        ThemeMode::System => is_windows_dark_theme_enabled(),
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
    };
    ThemePalette::for_dark(is_dark)
}

unsafe fn create_fonts_for_dpi(dpi: u32) -> (HFONT, HFONT) {
    let font_height = -scale_dpi(13, dpi);
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
    (regular, bold)
}

unsafe fn apply_dwm_window_theme(hwnd: HWND, is_dark: bool) {
    let preference: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &preference as *const _ as *const std::ffi::c_void,
        std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    );

    let dark_mode: i32 = i32::from(is_dark);
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &dark_mode as *const _ as *const std::ffi::c_void,
        std::mem::size_of::<i32>() as u32,
    );
}

unsafe fn apply_theme_to_flyout(hwnd: HWND, state: &mut FlyoutState) {
    let new_palette = resolve_palette(state.theme_mode);
    let old_brush = state.bg_brush;
    state.bg_brush = CreateSolidBrush(new_palette.surface_bg);
    state.palette = new_palette;
    let _ = DeleteObject(HGDIOBJ(old_brush.0));

    apply_dwm_window_theme(hwnd, new_palette.is_dark);
    let monitors = state.ddc.snapshots();
    rebuild_monitor_controls(hwnd, state, &monitors);
    let _ = InvalidateRect(Some(hwnd), None, true);
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
        let (theme_mode, show_contrast) = load_user_preferences();
        let palette = resolve_palette(theme_mode);
        let bg_brush = CreateSolidBrush(palette.surface_bg);

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
            312,
            118,
            None,
            None,
            Some(hinstance),
            None,
        )?;

        apply_dwm_window_theme(hwnd, palette.is_dark);

        let raw_dpi = GetDpiForWindow(hwnd);
        let dpi = if raw_dpi == 0 { 96 } else { raw_dpi };
        let (font_regular, font_bold) = create_fonts_for_dpi(dpi);
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
            theme_mode,
            palette,
            font_regular,
            font_bold,
            dpi,
            taskbar_created_msg,
            show_contrast,
            child_hwnds: Vec::new(),
            muted_hwnds: Vec::new(),
            headers: Vec::new(),
            bindings: Vec::new(),
            rendered_topology: Vec::new(),
            flyout_width: scale_dpi(312, dpi),
            flyout_height: scale_dpi(118, dpi),
            last_popup_pos: None,
            last_deactivated_at: None,
            ddc,
        });

        let initial_monitors = state.ddc.snapshots();
        let tray_hicon_copy = state.tray_hicon;
        let state_ptr = Box::into_raw(state);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);

        rebuild_monitor_controls(hwnd, &mut *state_ptr, &initial_monitors);
        add_or_update_tray_icon(hwnd, tray_hicon_copy, "DDC/CI Brightness & Contrast", false)?;

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
    for (dst, src) in nid
        .szTip
        .iter_mut()
        .take(max_len)
        .zip(tooltip.encode_utf16())
    {
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
                format!("{}: {}%", m.display_name, m.brightness)
            } else {
                format!("{}: ···", m.display_name)
            }
        })
        .collect();
    parts.join("  •  ")
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

    // 12px edge offset matches native Windows 11 Quick Settings tray flyout margin
    let margin = scale_dpi(12, state.dpi);
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
        let margin = scale_dpi(12, state.dpi);

        let new_y = if y > (work.top + work.bottom) / 2 {
            (work.bottom - height - margin).max(work.top + margin)
        } else {
            y.min(work.bottom - height - margin).max(work.top + margin)
        };
        state.last_popup_pos = Some((x, new_y));
        let _ = MoveWindow(hwnd, x, new_y, width, height, true);
    } else {
        position_and_show_flyout(hwnd, state);
    }
}

unsafe fn show_tray_context_menu(hwnd: HWND, show_contrast: bool, is_dark: bool) {
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    if let Ok(hmenu) = CreatePopupMenu() {
        let contrast_flags: MENU_ITEM_FLAGS = if show_contrast {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING | MF_UNCHECKED
        };
        let dark_toggle_flags: MENU_ITEM_FLAGS = if is_dark {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING | MF_UNCHECKED
        };

        let _ = AppendMenuW(hmenu, MF_STRING, IDM_OPEN_SLIDERS, w!("Open Sliders"));
        let _ = AppendMenuW(
            hmenu,
            contrast_flags,
            IDM_TOGGLE_CONTRAST,
            w!("Show Contrast"),
        );
        let _ = AppendMenuW(
            hmenu,
            dark_toggle_flags,
            IDM_TOGGLE_DARK_THEME,
            w!("Dark Theme"),
        );
        let _ = AppendMenuW(
            hmenu,
            MF_STRING,
            IDM_REFRESH_MONITORS,
            w!("Refresh Monitors"),
        );
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

fn format_value_text(val: u32, available: bool) -> String {
    if available {
        format!("{}", val)
    } else {
        "···".to_string()
    }
}

unsafe fn sync_or_rebuild_controls(hwnd: HWND, state: &mut FlyoutState, monitors: &[MonitorInfo]) {
    let new_topology: Vec<String> = monitors.iter().map(|m| m.id.clone()).collect();

    if state.rendered_topology != new_topology || (monitors.is_empty() && state.bindings.is_empty())
    {
        rebuild_monitor_controls(hwnd, state, monitors);
        resize_visible_flyout(hwnd, state);
        return;
    }

    // Update headers and sliders in-place so active mouse drags are never interrupted
    for hdr in &mut state.headers {
        if let Some(mon) = monitors.iter().find(|m| m.id == hdr.monitor_id)
            && hdr.available != mon.available
        {
            hdr.available = mon.available;
            set_window_text(hdr.header_hwnd, &hdr.base_name);
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

            if binding.last_value != target_val || binding.available != mon.available {
                binding.last_value = target_val;
                binding.available = mon.available;
                SendMessageW(
                    binding.slider_hwnd,
                    TBM_SETPOS,
                    Some(WPARAM(1)),
                    Some(LPARAM(target_val as isize)),
                );
                set_window_text(
                    binding.value_label_hwnd,
                    &format_value_text(target_val, mon.available),
                );
            }
        }
    }
}

unsafe fn rebuild_monitor_controls(hwnd: HWND, state: &mut FlyoutState, monitors: &[MonitorInfo]) {
    for child in state.child_hwnds.drain(..) {
        let _ = DestroyWindow(child);
    }
    state.muted_hwnds.clear();
    state.headers.clear();
    state.bindings.clear();
    state.rendered_topology = monitors.iter().map(|m| m.id.clone()).collect();

    let dpi = state.dpi;
    let width = scale_dpi(312, dpi);
    let pad = scale_dpi(16, dpi);
    let mut y = pad;

    if monitors.is_empty() {
        let empty_h = scale_dpi(36, dpi);
        if let Some(lbl) = create_static(
            hwnd,
            state.hinstance,
            "Searching for DDC/CI displays…",
            pad,
            y + scale_dpi(8, dpi),
            width - pad * 2,
            empty_h,
            state.font_regular,
            0,
        ) {
            state.child_hwnds.push(lbl);
            state.muted_hwnds.push(lbl);
        }
        state.flyout_width = width;
        state.flyout_height = y + empty_h + pad;
        return;
    }

    let hdr_h = scale_dpi(20, dpi);
    let hdr_step = scale_dpi(22, dpi);
    let row_step = scale_dpi(30, dpi);
    let section_gap = scale_dpi(10, dpi);
    let val_w = scale_dpi(32, dpi);

    for (idx, mon) in monitors.iter().enumerate() {
        let max_b = mon.max_brightness.max(1);
        let b_val = mon.brightness.min(max_b);

        // When only Brightness is displayed, place the numeric readout right-aligned on the header row
        // (Mies "Consolidate competing labels, values, track" pattern).
        let header_w = if state.show_contrast {
            width - pad * 2
        } else {
            width - pad * 2 - val_w - scale_dpi(8, dpi)
        };

        if let Some(header) = create_static(
            hwnd,
            state.hinstance,
            &mon.display_name,
            pad,
            y,
            header_w,
            hdr_h,
            state.font_bold,
            0,
        ) {
            state.child_hwnds.push(header);
            state.headers.push(MonitorHeaderBinding {
                monitor_id: mon.id.clone(),
                header_hwnd: header,
                base_name: mon.display_name.clone(),
                available: mon.available,
            });
        }

        let header_val_hwnd = if !state.show_contrast {
            let val_text = format_value_text(b_val, mon.available);
            if let Some(vh) = create_static(
                hwnd,
                state.hinstance,
                &val_text,
                width - pad - val_w,
                y,
                val_w,
                hdr_h,
                state.font_regular,
                SS_RIGHT_STYLE,
            ) {
                state.child_hwnds.push(vh);
                state.muted_hwnds.push(vh);
                Some(vh)
            } else {
                None
            }
        } else {
            None
        };

        y += hdr_step;

        if let Some((b_slider, b_val_lbl)) = create_slider_row(
            hwnd,
            state,
            "☀",
            b_val,
            max_b,
            mon.available,
            pad,
            y,
            width,
            header_val_hwnd,
        ) {
            state.bindings.push(SliderBinding {
                monitor_id: mon.id.clone(),
                kind: ControlKind::Brightness,
                slider_hwnd: b_slider,
                value_label_hwnd: b_val_lbl,
                last_value: b_val,
                max_value: max_b,
                available: mon.available,
                is_dragging: false,
            });
        }
        y += row_step;

        if state.show_contrast {
            let max_c = mon.max_contrast.max(1);
            let c_val = mon.contrast.min(max_c);
            if let Some((c_slider, c_val_lbl)) = create_slider_row(
                hwnd,
                state,
                "◐",
                c_val,
                max_c,
                mon.available,
                pad,
                y,
                width,
                None,
            ) {
                state.bindings.push(SliderBinding {
                    monitor_id: mon.id.clone(),
                    kind: ControlKind::Contrast,
                    slider_hwnd: c_slider,
                    value_label_hwnd: c_val_lbl,
                    last_value: c_val,
                    max_value: max_c,
                    available: mon.available,
                    is_dragging: false,
                });
            }
            y += row_step;
        }

        if idx + 1 < monitors.len() {
            y += section_gap / 2;
            if let Some(div) = create_static(
                hwnd,
                state.hinstance,
                "",
                pad,
                y,
                width - pad * 2,
                scale_dpi(2, dpi),
                state.font_regular,
                SS_ETCHEDHORZ_STYLE,
            ) {
                state.child_hwnds.push(div);
            }
            y += section_gap;
        }
    }

    state.flyout_width = width;
    state.flyout_height = y + pad - scale_dpi(4, dpi);
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_slider_row(
    parent: HWND,
    state: &mut FlyoutState,
    glyph: &str,
    initial_val: u32,
    max_val: u32,
    available: bool,
    pad: i32,
    y: i32,
    total_width: i32,
    existing_val_hwnd: Option<HWND>,
) -> Option<(HWND, HWND)> {
    let dpi = state.dpi;
    let show_glyph_and_inline_val = existing_val_hwnd.is_none();

    let glyph_w = if show_glyph_and_inline_val {
        scale_dpi(18, dpi)
    } else {
        0
    };
    let val_w = if show_glyph_and_inline_val {
        scale_dpi(32, dpi)
    } else {
        0
    };
    let gap = scale_dpi(6, dpi);
    let slider_x = if show_glyph_and_inline_val {
        pad + glyph_w + gap
    } else {
        pad
    };
    let slider_w = if show_glyph_and_inline_val {
        total_width - pad * 2 - glyph_w - val_w - gap * 2
    } else {
        total_width - pad * 2
    };
    let text_y_offset = scale_dpi(4, dpi);
    let text_h = scale_dpi(20, dpi);
    let slider_h = scale_dpi(26, dpi);

    if show_glyph_and_inline_val {
        let glyph_hwnd = create_static(
            parent,
            state.hinstance,
            glyph,
            pad,
            y + text_y_offset,
            glyph_w,
            text_h,
            state.font_regular,
            0,
        )?;
        state.child_hwnds.push(glyph_hwnd);
        state.muted_hwnds.push(glyph_hwnd);
    }

    let slider_hwnd = CreateWindowExW(
        Default::default(),
        TRACKBAR_CLASSW,
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(TBS_HORZ | TBS_NOTICKS),
        slider_x,
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
        TBM_SETPOS,
        Some(WPARAM(1)),
        Some(LPARAM(initial_val as isize)),
    );
    state.child_hwnds.push(slider_hwnd);

    let val_hwnd = if let Some(vh) = existing_val_hwnd {
        vh
    } else {
        let val_text = format_value_text(initial_val, available);
        let vh = create_static(
            parent,
            state.hinstance,
            &val_text,
            total_width - pad - val_w,
            y + text_y_offset,
            val_w,
            text_h,
            state.font_regular,
            SS_RIGHT_STYLE,
        )?;
        state.child_hwnds.push(vh);
        state.muted_hwnds.push(vh);
        vh
    };

    Some((slider_hwnd, val_hwnd))
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
    extra_style: u32,
) -> Option<HWND> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = CreateWindowExW(
        Default::default(),
        w!("STATIC"),
        PCWSTR(wide.as_ptr()),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(extra_style),
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

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
        WM_ERASEBKGND => {
            if !state_ptr.is_null() {
                let state = &*state_ptr;
                let hdc = HDC(wparam.0 as *mut std::ffi::c_void);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let _ = FillRect(hdc, &rc, state.bg_brush);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        WM_SETTINGCHANGE => {
            // Live-sync when Windows dark/light theme changes in Settings while ThemeMode::System is active
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                if state.theme_mode == ThemeMode::System {
                    let next_dark = is_windows_dark_theme_enabled();
                    if next_dark != state.palette.is_dark {
                        apply_theme_to_flyout(hwnd, state);
                    }
                }
            }
            LRESULT(0)
        }

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
                    let (new_reg, new_bold) = create_fonts_for_dpi(new_dpi);
                    state.font_regular = new_reg;
                    state.font_bold = new_bold;
                    let monitors = state.ddc.snapshots();
                    rebuild_monitor_controls(hwnd, state, &monitors);
                    resize_visible_flyout(hwnd, state);
                    let _ = DeleteObject(HGDIOBJ(old_reg.0));
                    let _ = DeleteObject(HGDIOBJ(old_bold.0));
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
                        set_window_text(binding.value_label_hwnd, &format_value_text(pos, true));

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
                let ctrl_hwnd = HWND(lparam.0 as *mut std::ffi::c_void);
                SetBkColor(hdc, state.palette.surface_bg);
                if state.muted_hwnds.contains(&ctrl_hwnd) {
                    SetTextColor(hdc, state.palette.text_muted);
                } else {
                    SetTextColor(hdc, state.palette.text_primary);
                }
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
                    let (show_contrast, is_dark) = if !state_ptr.is_null() {
                        let s = &*state_ptr;
                        (s.show_contrast, s.palette.is_dark)
                    } else {
                        (true, is_windows_dark_theme_enabled())
                    };
                    show_tray_context_menu(hwnd, show_contrast, is_dark);
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
                IDM_TOGGLE_CONTRAST => {
                    if !state_ptr.is_null() {
                        let state = &mut *state_ptr;
                        state.show_contrast = !state.show_contrast;
                        save_user_preferences(state.theme_mode, state.show_contrast);
                        let snapshots = state.ddc.snapshots();
                        rebuild_monitor_controls(hwnd, state, &snapshots);
                        if IsWindowVisible(hwnd).as_bool() {
                            resize_visible_flyout(hwnd, state);
                        } else {
                            position_and_show_flyout(hwnd, state);
                        }
                    }
                }
                IDM_TOGGLE_DARK_THEME => {
                    if !state_ptr.is_null() {
                        let state = &mut *state_ptr;
                        state.theme_mode = if state.palette.is_dark {
                            ThemeMode::Light
                        } else {
                            ThemeMode::Dark
                        };
                        save_user_preferences(state.theme_mode, state.show_contrast);
                        apply_theme_to_flyout(hwnd, state);
                    }
                }
                IDM_REFRESH_MONITORS => {
                    if !state_ptr.is_null() {
                        let state = &mut *state_ptr;
                        state.ddc.request_refresh();
                    }
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
                let _ = DeleteObject(HGDIOBJ(state.bg_brush.0));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
