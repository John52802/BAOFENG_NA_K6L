//! Dark mode for the window. Windows doesn't offer one for these classic controls, so this uses
//! its internal dark themes where they exist (looked up at run time, and simply skipped on a
//! Windows without them) and paints the rest. Message boxes and file dialogs stay as Windows
//! draws them.

use native_windows_gui as nwg;
use std::ffi::{CString, OsStr};
use std::fs;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use winapi::shared::minwindef::{BOOL, FARPROC, LPARAM, UINT, WPARAM};
use winapi::shared::windef::{COLORREF, HBRUSH, HDC, HWND, POINT, RECT};
use winapi::um::commctrl::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, NMCUSTOMDRAW, NM_CUSTOMDRAW, TCIF_TEXT,
    TCITEMW, TCM_GETCURSEL, TCM_GETITEMCOUNT, TCM_GETITEMRECT, TCM_GETITEMW, TCS_OWNERDRAWFIXED,
};
use winapi::um::libloaderapi::{GetProcAddress, LoadLibraryW};
use winapi::um::wingdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject, ExcludeClipRect, SelectObject,
    SetBkColor, SetBkMode, SetTextColor, SRCCOPY, TRANSPARENT,
};
use winapi::um::winuser::*;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

const BACK: COLORREF = rgb(32, 32, 32);
const PANEL: COLORREF = rgb(43, 43, 43);
const INPUT: COLORREF = rgb(24, 24, 24);
const TEXT: COLORREF = rgb(232, 232, 232);
const TAB_ON: COLORREF = rgb(58, 58, 58);
const LIGHT_BACK: COLORREF = rgb(240, 240, 240);
const LIGHT_ON: COLORREF = rgb(255, 255, 255);
const LIGHT_TEXT: COLORREF = rgb(0, 0, 0);

const SETTING_FILE: &str = "k6_actualizar_tema.txt";
const LIGHT_WORD: &str = "claro";
const HANDLER_ID: usize = 0x4B36_0001;
const TITLE_BAR_DARK: u32 = 20; // DWMWA_USE_IMMERSIVE_DARK_MODE.
const APP_MODE_ALLOW_DARK: i32 = 1;
const ODT_TAB_CONTROL: UINT = 101;
const LVM_GETBKCOLOR: UINT = 0x1000;
const LVM_GETHEADER: UINT = 0x101F;
const LVM_SETEXTENDEDLISTVIEWSTYLE: UINT = 0x1036;
const LVS_EX_GRIDLINES: LPARAM = 0x0001;
const LVM_SETBKCOLOR: UINT = 0x1001;
const LVM_SETTEXTCOLOR: UINT = 0x1024;
const LVM_SETTEXTBKCOLOR: UINT = 0x1026;
const PBM_SETBARCOLOR: UINT = 0x0409;
const PBM_SETBKCOLOR: UINT = 0x2001;
const CLR_DEFAULT: COLORREF = 0xFF00_0000;
const BAR: COLORREF = rgb(0, 150, 80);

static DARK: AtomicBool = AtomicBool::new(false);
static BACK_BRUSH: AtomicIsize = AtomicIsize::new(0);
static PANEL_BRUSH: AtomicIsize = AtomicIsize::new(0);
static INPUT_BRUSH: AtomicIsize = AtomicIsize::new(0);

fn setting_path() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join(SETTING_FILE))
}

fn word_means_dark(text: &str) -> bool {
    text.trim() != LIGHT_WORD
}

/// The saved choice. Dark unless the user chose light.
pub fn saved_dark() -> bool {
    setting_path().and_then(|p| fs::read_to_string(p).ok()).map_or(true, |t| word_means_dark(&t))
}

fn save_dark(dark: bool) {
    if let Some(path) = setting_path() {
        // Only a convenience: a folder that can't be written just forgets the choice.
        let _ = fs::write(path, if dark { "oscuro" } else { LIGHT_WORD });
    }
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

fn system_function(library: &str, name: &str) -> FARPROC {
    let Ok(name) = CString::new(name) else { return ptr::null_mut() };
    unsafe {
        let module = LoadLibraryW(wide(library).as_ptr());
        if module.is_null() {
            return ptr::null_mut();
        }
        GetProcAddress(module, name.as_ptr())
    }
}

/// Allows the internal dark themes, which Windows only exports by number.
fn allow_dark_themes(hwnd: HWND, dark: bool) {
    unsafe {
        let module = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if module.is_null() {
            return;
        }
        let by_number = |n: usize| GetProcAddress(module, n as *const i8);
        let set_app_mode = by_number(135);
        if !set_app_mode.is_null() {
            let f: extern "system" fn(i32) -> i32 = mem::transmute(set_app_mode);
            f(if dark { APP_MODE_ALLOW_DARK } else { 0 });
        }
        let allow_window = by_number(133);
        if !allow_window.is_null() {
            let f: extern "system" fn(HWND, BOOL) -> BOOL = mem::transmute(allow_window);
            f(hwnd, dark as BOOL);
        }
    }
}

fn set_theme(hwnd: HWND, name: Option<&str>) {
    let f = system_function("uxtheme.dll", "SetWindowTheme");
    if f.is_null() {
        return;
    }
    unsafe {
        let f: extern "system" fn(HWND, *const u16, *const u16) -> i32 = mem::transmute(f);
        match name {
            // Only two empty names really turn the visual style off.
            Some("") => f(hwnd, wide("").as_ptr(), wide("").as_ptr()),
            Some(n) => f(hwnd, wide(n).as_ptr(), ptr::null()),
            None => f(hwnd, ptr::null(), ptr::null()),
        };
    }
}

fn set_title_bar(hwnd: HWND, dark: bool) {
    let f = system_function("dwmapi.dll", "DwmSetWindowAttribute");
    if f.is_null() {
        return;
    }
    let value: BOOL = dark as BOOL;
    unsafe {
        let f: extern "system" fn(HWND, u32, *const BOOL, u32) -> i32 = mem::transmute(f);
        f(hwnd, TITLE_BAR_DARK, &value, mem::size_of::<BOOL>() as u32);
    }
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn is_check_or_radio(hwnd: HWND) -> bool {
    let kind = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32 & BS_TYPEMASK;
    matches!(kind, BS_CHECKBOX | BS_AUTOCHECKBOX | BS_RADIOBUTTON | BS_AUTORADIOBUTTON | BS_3STATE | BS_AUTO3STATE)
}

fn list_colors() -> (COLORREF, COLORREF) {
    if DARK.load(Ordering::Relaxed) { (INPUT, TEXT) } else { (LIGHT_ON, LIGHT_TEXT) }
}

unsafe fn set_list_colors(hwnd: HWND) {
    let (back, text) = list_colors();
    SendMessageW(hwnd, LVM_SETBKCOLOR, 0, back as LPARAM);
    SendMessageW(hwnd, LVM_SETTEXTBKCOLOR, 0, back as LPARAM);
    SendMessageW(hwnd, LVM_SETTEXTCOLOR, 0, text as LPARAM);
}

unsafe extern "system" fn theme_child(hwnd: HWND, _: LPARAM) -> BOOL {
    let dark = DARK.load(Ordering::Relaxed);
    allow_dark_themes(hwnd, dark);
    match class_of(hwnd).as_str() {
        // Themed check boxes ignore the text color, so in the dark they use the plain look.
        "Button" if is_check_or_radio(hwnd) => set_theme(hwnd, if dark { Some("") } else { None }),
        "Button" => set_theme(hwnd, dark.then_some("DarkMode_Explorer")),
        "ComboBox" | "Edit" => set_theme(hwnd, dark.then_some("DarkMode_CFD")),
        "SysHeader32" => set_theme(hwnd, dark.then_some("DarkMode_ItemsView")),
        "SysListView32" => {
            set_theme(hwnd, dark.then_some("DarkMode_Explorer"));
            set_list_colors(hwnd);
            // The grid can't change color, and its light lines stand out in the dark.
            let grid = if dark { 0 } else { LVS_EX_GRIDLINES };
            SendMessageW(hwnd, LVM_SETEXTENDEDLISTVIEWSTYLE, LVS_EX_GRIDLINES as WPARAM, grid);
        }
        "msctls_progress32" => {
            set_theme(hwnd, if dark { Some("") } else { None });
            SendMessageW(hwnd, PBM_SETBKCOLOR, 0, if dark { INPUT } else { CLR_DEFAULT } as LPARAM);
            SendMessageW(hwnd, PBM_SETBARCOLOR, 0, if dark { BAR } else { CLR_DEFAULT } as LPARAM);
        }
        _ => {}
    }
    1
}

fn brush(cell: &AtomicIsize, color: COLORREF) -> isize {
    let current = cell.load(Ordering::Relaxed);
    if current != 0 {
        return current;
    }
    // Kept for the life of the program.
    let new = unsafe { CreateSolidBrush(color) } as isize;
    cell.store(new, Ordering::Relaxed);
    new
}

unsafe fn draw_tab(strip: HWND, dc: HDC, index: usize, mut area: RECT, selected: bool) {
    let dark = DARK.load(Ordering::Relaxed);
    let (back, text) = match (dark, selected) {
        (true, true) => (TAB_ON, TEXT),
        (true, false) => (BACK, TEXT),
        (false, true) => (LIGHT_ON, LIGHT_TEXT),
        (false, false) => (LIGHT_BACK, LIGHT_TEXT),
    };
    let fill = CreateSolidBrush(back);
    FillRect(dc, &area, fill);
    winapi::um::wingdi::DeleteObject(fill as _);

    let mut title = [0u16; 32];
    let mut tab: TCITEMW = mem::zeroed();
    tab.mask = TCIF_TEXT;
    tab.pszText = title.as_mut_ptr();
    tab.cchTextMax = title.len() as i32;
    SendMessageW(strip, TCM_GETITEMW, index as WPARAM, &mut tab as *mut _ as LPARAM);
    let font = SendMessageW(strip, WM_GETFONT, 0, 0);
    if font != 0 {
        SelectObject(dc, font as _);
    }
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, text);
    DrawTextW(dc, title.as_ptr(), -1, &mut area, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}

/// The library paints this list over a fixed white background, so in dark mode it's done here.
unsafe fn paint_list(hwnd: HWND) {
    let mut area: RECT = mem::zeroed();
    GetClientRect(hwnd, &mut area);
    let mut paint: PAINTSTRUCT = mem::zeroed();
    let dc = BeginPaint(hwnd, &mut paint);
    let buffer_dc = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, area.right, area.bottom);
    let old = SelectObject(buffer_dc, bitmap as _);
    FillRect(buffer_dc, &area, brush(&INPUT_BRUSH, INPUT) as HBRUSH);
    SendMessageW(hwnd, WM_PRINTCLIENT, buffer_dc as WPARAM, 0);
    BitBlt(dc, 0, 0, area.right, area.bottom, buffer_dc, 0, 0, SRCCOPY);
    SelectObject(buffer_dc, old);
    DeleteObject(bitmap as _);
    DeleteDC(buffer_dc);
    EndPaint(hwnd, &paint);
    let header = SendMessageW(hwnd, LVM_GETHEADER, 0, 0);
    if header != 0 {
        RedrawWindow(header as HWND, ptr::null(), ptr::null_mut(), RDW_INVALIDATE | RDW_UPDATENOW);
    }
}

/// Paints the whole tab strip in dark mode: its default painting draws light borders.
unsafe fn paint_strip(strip: HWND) {
    let mut paint: PAINTSTRUCT = mem::zeroed();
    let dc = BeginPaint(strip, &mut paint);
    let mut whole: RECT = mem::zeroed();
    GetClientRect(strip, &mut whole);
    FillRect(dc, &whole, brush(&BACK_BRUSH, BACK) as HBRUSH);
    let count = SendMessageW(strip, TCM_GETITEMCOUNT, 0, 0);
    let current = SendMessageW(strip, TCM_GETCURSEL, 0, 0);
    for i in 0..count {
        let mut area: RECT = mem::zeroed();
        SendMessageW(strip, TCM_GETITEMRECT, i as WPARAM, &mut area as *mut _ as LPARAM);
        draw_tab(strip, dc, i as usize, area, i == current);
    }
    EndPaint(strip, &paint);
}

fn on_strip_message(hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> Option<isize> {
    if msg == WM_PAINT && DARK.load(Ordering::Relaxed) {
        unsafe { paint_strip(hwnd) };
        return Some(0);
    }
    on_message(hwnd, msg, w, l, false)
}

/// The list loses its colors whenever its theme is applied, and its header can't be told a text
/// color any other way than while it draws, so both are handled here.
fn on_list_message(hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> Option<isize> {
    unsafe {
        let _ = w;
        if msg == WM_PAINT && DARK.load(Ordering::Relaxed) {
            if SendMessageW(hwnd, LVM_GETBKCOLOR, 0, 0) as COLORREF != list_colors().0 {
                set_list_colors(hwnd);
            }
            paint_list(hwnd);
            return Some(0);
        }
        if msg == WM_PAINT && SendMessageW(hwnd, LVM_GETBKCOLOR, 0, 0) as COLORREF != list_colors().0 {
            set_list_colors(hwnd);
        }
        if msg == WM_NOTIFY && DARK.load(Ordering::Relaxed) {
            let header = &*(l as *const NMHDR);
            if header.code == NM_CUSTOMDRAW as UINT && class_of(header.hwndFrom) == "SysHeader32" {
                let draw = &*(l as *const NMCUSTOMDRAW);
                if draw.dwDrawStage == CDDS_PREPAINT {
                    return Some(CDRF_NOTIFYITEMDRAW as isize);
                }
                if draw.dwDrawStage == CDDS_ITEMPREPAINT {
                    SetTextColor(draw.hdc, TEXT);
                    return Some(CDRF_DODEFAULT as isize);
                }
            }
        }
    }
    None
}

/// Labels center their text by keeping the rest of their area outside the part they draw, and
/// that outer part is painted light. In dark mode it gets the color of what's behind it.
fn on_label_message(hwnd: HWND, msg: UINT, page: bool) -> Option<isize> {
    if msg != WM_NCPAINT || !DARK.load(Ordering::Relaxed) {
        return None;
    }
    unsafe {
        let dc = GetWindowDC(hwnd);
        let (mut outer, mut inner, mut corner): (RECT, RECT, POINT) = (mem::zeroed(), mem::zeroed(), mem::zeroed());
        GetWindowRect(hwnd, &mut outer);
        GetClientRect(hwnd, &mut inner);
        ClientToScreen(hwnd, &mut corner);
        let (dx, dy) = (corner.x - outer.left, corner.y - outer.top);
        ExcludeClipRect(dc, dx, dy, dx + inner.right, dy + inner.bottom);
        let whole = RECT { left: 0, top: 0, right: outer.right - outer.left, bottom: outer.bottom - outer.top };
        let fill = if page { brush(&PANEL_BRUSH, PANEL) } else { brush(&BACK_BRUSH, BACK) };
        FillRect(dc, &whole, fill as HBRUSH);
        ReleaseDC(hwnd, dc);
    }
    Some(0)
}

struct LabelSearch {
    main: HWND,
}

unsafe extern "system" fn bind_child(hwnd: HWND, param: LPARAM) -> BOOL {
    let search = &*(param as *const LabelSearch);
    let handle = nwg::ControlHandle::Hwnd(hwnd);
    match class_of(hwnd).as_str() {
        "Static" => {
            let page = GetParent(hwnd) != search.main;
            let _ = nwg::bind_raw_event_handler(&handle, HANDLER_ID, move |h, m, _, _| on_label_message(h, m, page));
        }
        "SysListView32" => {
            let _ = nwg::bind_raw_event_handler(&handle, HANDLER_ID, |h, m, w, l| on_list_message(h, m, w, l));
        }
        _ => {}
    }
    1
}

fn on_message(hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM, page: bool) -> Option<isize> {
    // Drawn in both modes, because the tab strip is always painted from here.
    if msg == WM_DRAWITEM {
        let item = unsafe { &*(l as *const DRAWITEMSTRUCT) };
        if item.CtlType == ODT_TAB_CONTROL {
            unsafe { draw_tab(item.hwndItem, item.hDC, item.itemID as usize, item.rcItem, item.itemState & ODS_SELECTED != 0) };
            return Some(1);
        }
    }
    if !DARK.load(Ordering::Relaxed) {
        return None;
    }
    let (back, cell) = if page { (PANEL, &PANEL_BRUSH) } else { (BACK, &BACK_BRUSH) };
    match msg {
        WM_ERASEBKGND => {
            let mut area: RECT = unsafe { mem::zeroed() };
            unsafe {
                GetClientRect(hwnd, &mut area);
                FillRect(w as HDC, &area, brush(cell, back) as HBRUSH);
            }
            Some(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORDLG => {
            let dc = w as HDC;
            // Read-only text boxes also ask here, and they read better with the input color.
            let is_edit = class_of(l as HWND) == "Edit";
            unsafe {
                SetTextColor(dc, TEXT);
                SetBkColor(dc, if is_edit { INPUT } else { back });
            }
            Some(if is_edit { brush(&INPUT_BRUSH, INPUT) } else { brush(cell, back) })
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let dc = w as HDC;
            unsafe {
                SetTextColor(dc, TEXT);
                SetBkColor(dc, INPUT);
            }
            Some(brush(&INPUT_BRUSH, INPUT))
        }
        _ => None,
    }
}

/// Binds the painting. `pages` are the tab pages, which sit one shade lighter than the window.
pub fn install(window: &nwg::Window, tabs: &nwg::TabsContainer, pages: &[nwg::ControlHandle]) {
    if let Some(strip) = tabs.handle.hwnd() {
        unsafe {
            let style = GetWindowLongW(strip, GWL_STYLE) as u32;
            SetWindowLongW(strip, GWL_STYLE, (style | TCS_OWNERDRAWFIXED) as i32);
        }
    }
    let _ = nwg::bind_raw_event_handler(&window.handle, HANDLER_ID, |h, m, w, l| on_message(h, m, w, l, false));
    let _ = nwg::bind_raw_event_handler(&tabs.handle, HANDLER_ID, |h, m, w, l| on_strip_message(h, m, w, l));
    for page in pages {
        let _ = nwg::bind_raw_event_handler(page, HANDLER_ID, |h, m, w, l| on_message(h, m, w, l, true));
    }
    if let Some(main) = window.handle.hwnd() {
        let search = LabelSearch { main };
        unsafe { EnumChildWindows(main, Some(bind_child), &search as *const _ as LPARAM) };
    }
}

/// Switches the whole window, and remembers the choice if `remember` is set.
pub fn apply(window: &nwg::Window, dark: bool, remember: bool) {
    DARK.store(dark, Ordering::Relaxed);
    if remember {
        save_dark(dark);
    }
    let Some(hwnd) = window.handle.hwnd() else { return };
    allow_dark_themes(hwnd, dark);
    set_title_bar(hwnd, dark);
    unsafe {
        EnumChildWindows(hwnd, Some(theme_child), 0);
        RedrawWindow(hwnd, ptr::null(), ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_the_default_and_only_the_light_word_turns_it_off() {
        assert!(word_means_dark("oscuro"));
        assert!(word_means_dark(""));
        assert!(word_means_dark("cualquier cosa"));
        assert!(!word_means_dark("claro"));
        assert!(!word_means_dark("claro\r\n"));
    }

    #[test]
    fn colors_are_stored_as_windows_expects() {
        assert_eq!(rgb(0x11, 0x22, 0x33), 0x0033_2211);
    }
}
