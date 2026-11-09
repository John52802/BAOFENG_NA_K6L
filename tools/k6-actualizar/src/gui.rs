//! The program's window, with native Windows controls. All radio work runs on a worker
//! thread and reports through `crate::Msg`; a timer in the window collects those messages.

use crate::channels::{self, Channel};
use crate::{logo, restore, settings, theme, vfo};
use crate::{list_ports, run_action, set_sink, Action, Language, Msg};
use native_windows_gui as nwg;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

// The smallest the window's contents can be, and the height it opens with. The user's own screen
// has about 720 dots of usable height, title bar included, so these must stay well under that.
const BASE_WIDTH: i32 = 1020;
const BASE_HEIGHT: i32 = 640;
const OPEN_HEIGHT: i32 = 660;
// These include the title bar and borders, which the sizes above don't count.
const MIN_OUTER_WIDTH: i32 = 1040;
const MIN_OUTER_HEIGHT: i32 = 685;
// Container height at BASE_HEIGHT: leaves the settings tab the room its rows and buttons need.
const BASE_TABS_HEIGHT: i32 = 364;
// What the tab bar and borders take from the container's height. The library works it out in
// physical dots without telling, so this leaves room for Windows' display scaling too.
const TAB_CHROME_HEIGHT: i32 = 48;

const TITLE: &str = "Baofeng NA-K6 (FD6818): actualizador";
const BUILT_IN: &str = "(el incluido en el programa)";
const MODEL_SHORT: &str =
    "SOLO para Baofeng NA-K6 con chip de radio FD6818. No usar con FD6818B ni con radios Quansheng.";
const MODEL_NOTICE: &str = "Este programa es SOLO para el Baofeng NA-K6 (familia UV-K6, procesador KD32F328) con chip de radio FD6818.\n\n\
NO lo uses con:\n\
  - Radios con chip FD6818B: la parte de radio dejaría de funcionar.\n\
  - Quansheng UV-K5 / UV-K6: tienen otro procesador.\n\
  - Otros modelos de Baofeng.\n\n\
El chip solo se confirma abriendo el radio y leyéndolo.\n\
El firmware de fábrica no se puede recuperar después de instalar este.";

#[derive(Default)]
struct Ui {
    window: nwg::Window,
    model_label: nwg::Label,
    port_label: nwg::Label,
    port: nwg::ComboBox<String>,
    refresh: nwg::Button,
    dark: nwg::CheckBox,
    tabs: nwg::TabsContainer,

    tab_radio: nwg::Tab,
    backup: nwg::Button,
    backup_label: nwg::Label,
    fw_label: nwg::Label,
    fw_path: nwg::TextInput,
    fw_browse: nwg::Button,
    install: nwg::Button,
    voice_label: nwg::Label,
    voice_es: nwg::RadioButton,
    voice_en: nwg::RadioButton,
    language: nwg::Button,
    text_label: nwg::Label,

    tab_channels: nwg::Tab,
    list: nwg::ListView,
    num_label: nwg::Label,
    num: nwg::TextInput,
    rx_label: nwg::Label,
    rx: nwg::TextInput,
    tx_label: nwg::Label,
    tx: nwg::TextInput,
    name_label: nwg::Label,
    name: nwg::TextInput,
    rx_tone_label: nwg::Label,
    rx_tone: nwg::TextInput,
    tx_tone_label: nwg::Label,
    tx_tone: nwg::TextInput,
    tone_hint: nwg::Label,
    option_labels: Vec<nwg::Label>,
    option_boxes: Vec<nwg::ComboBox<String>>,
    apply: nwg::Button,
    delete: nwg::Button,
    read: nwg::Button,
    save: nwg::Button,
    import: nwg::Button,
    export: nwg::Button,

    tab_settings: nwg::Tab,
    setting_labels: Vec<nwg::Label>,
    setting_boxes: Vec<nwg::ComboBox<String>>,
    settings_read: nwg::Button,
    settings_save: nwg::Button,
    settings_import: nwg::Button,
    settings_export: nwg::Button,
    settings_open: nwg::FileDialog,
    settings_saveas: nwg::FileDialog,
    settings_help: nwg::Button,
    message_label: nwg::Label,
    message_text: nwg::TextInput,
    password_on_label: nwg::Label,
    password_on: nwg::ComboBox<String>,
    password_label: nwg::Label,
    password_text: nwg::TextInput,
    scan_label: nwg::Label,
    scan_text: nwg::TextInput,
    restore: nwg::Button,
    profile_export: nwg::Button,
    profile_import: nwg::Button,
    restore_open: nwg::FileDialog,
    profile_open: nwg::FileDialog,
    profile_saveas: nwg::FileDialog,
    tooltip: nwg::Tooltip,

    tab_vfo: nwg::Tab,
    vfo_labels: Vec<nwg::Label>,
    vfo_heads: [nwg::Label; 2],
    vfo_freq: [nwg::TextInput; 2],
    vfo_rx_tone: [nwg::TextInput; 2],
    vfo_tx_tone: [nwg::TextInput; 2],
    vfo_direction: [nwg::ComboBox<String>; 2],
    vfo_offset: [nwg::TextInput; 2],
    vfo_power: [nwg::ComboBox<String>; 2],
    vfo_width: [nwg::ComboBox<String>; 2],
    vfo_step: [nwg::ComboBox<String>; 2],
    vfo_mute: [nwg::ComboBox<String>; 2],
    vfo_read: nwg::Button,
    vfo_save: nwg::Button,
    vfo_channel_label: nwg::Label,
    vfo_channel: nwg::ComboBox<String>,
    vfo_to_a: nwg::Button,
    vfo_to_b: nwg::Button,
    vfo_import: nwg::Button,
    vfo_export: nwg::Button,
    vfo_open: nwg::FileDialog,
    vfo_saveas: nwg::FileDialog,
    vfo_hint: nwg::Label,
    vfo_data: RefCell<Option<Vec<u8>>>,

    tab_logo: nwg::Tab,
    logo_frame: nwg::ImageFrame,
    logo_bitmap: RefCell<nwg::Bitmap>,
    logo_read: nwg::Button,
    logo_choose: nwg::Button,
    logo_export: nwg::Button,
    logo_save: nwg::Button,
    logo_invert: nwg::CheckBox,
    logo_dither: nwg::CheckBox,
    logo_hint: nwg::Label,
    logo_open: nwg::FileDialog,
    logo_saveas: nwg::FileDialog,
    logo_loaded: RefCell<Option<Vec<u8>>>,
    logo_gray: RefCell<Option<Vec<u8>>>,
    logo_screen: RefCell<Option<Vec<u8>>>,
    logo_scale: Cell<usize>,
    settings_data: RefCell<Option<Vec<u8>>>,

    log: nwg::TextBox,
    progress: nwg::ProgressBar,
    progress_label: nwg::Label,
    status: nwg::Label,
    timer: nwg::AnimationTimer,
    open_dialog: nwg::FileDialog,
    csv_open: nwg::FileDialog,
    csv_save: nwg::FileDialog,

    rx_msgs: RefCell<Option<Receiver<Msg>>>,
    firmware: RefCell<Option<PathBuf>>,
    area: RefCell<Option<Vec<u8>>>,
    channels: RefCell<Vec<Channel>>,
}

impl Ui {
    fn busy(&self) -> bool {
        self.rx_msgs.borrow().is_some()
    }

    fn selected_port(&self) -> Option<String> {
        let name = self.port.selection_string()?;
        // Items read "COM3 (cable)"; the port name is the first word.
        name.split_whitespace().next().map(str::to_string)
    }

    fn refresh_ports(&self) {
        let previous = self.selected_port();
        let ports = list_ports();
        let items: Vec<String> = ports
            .iter()
            .map(|(name, cable)| if *cable { format!("{name} (cable)") } else { name.clone() })
            .collect();
        let keep = previous.and_then(|p| ports.iter().position(|(n, _)| *n == p));
        // Prefer what was selected, then the programming cable (CP210x), then the first port.
        let choice = keep
            .or_else(|| ports.iter().position(|(_, cable)| *cable))
            .or(if ports.is_empty() { None } else { Some(0) });
        self.port.set_collection(items);
        self.port.set_selection(choice);
        self.update_enabled();
    }

    fn update_enabled(&self) {
        let idle = !self.busy();
        let has_port = idle && self.port.selection().is_some();
        let loaded = self.area.borrow().is_some();
        self.port.set_enabled(idle);
        self.refresh.set_enabled(idle);
        self.backup.set_enabled(has_port);
        self.fw_browse.set_enabled(idle);
        self.install.set_enabled(has_port);
        self.voice_es.set_enabled(idle);
        self.voice_en.set_enabled(idle);
        self.language.set_enabled(has_port);
        self.read.set_enabled(has_port);
        self.save.set_enabled(has_port && loaded);
        self.apply.set_enabled(idle && loaded);
        self.delete.set_enabled(idle && loaded);
        self.import.set_enabled(idle && loaded);
        self.export.set_enabled(idle && loaded);
        for b in &self.option_boxes {
            b.set_enabled(idle && loaded);
        }
        let has_settings = self.settings_data.borrow().is_some();
        self.settings_read.set_enabled(has_port);
        self.settings_save.set_enabled(has_port && has_settings);
        self.settings_import.set_enabled(idle && has_settings);
        self.settings_export.set_enabled(idle && has_settings);
        self.message_text.set_enabled(idle && has_settings);
        self.password_on.set_enabled(idle && has_settings);
        self.password_text.set_enabled(idle && has_settings);
        self.scan_text.set_enabled(idle && has_settings);
        self.restore.set_enabled(has_port);
        self.profile_export.set_enabled(has_port);
        self.profile_import.set_enabled(has_port);
        let has_vfo = self.vfo_data.borrow().is_some();
        self.vfo_read.set_enabled(has_port);
        self.vfo_save.set_enabled(has_port && has_vfo);
        let has_channels = !self.channels.borrow().is_empty();
        self.vfo_channel.set_enabled(idle && has_vfo && has_channels);
        self.vfo_to_a.set_enabled(idle && has_vfo && has_channels);
        self.vfo_to_b.set_enabled(idle && has_vfo && has_channels);
        self.vfo_import.set_enabled(idle && has_vfo);
        self.vfo_export.set_enabled(idle && has_vfo);
        for i in 0..vfo::VFO_COUNT {
            self.vfo_freq[i].set_enabled(idle && has_vfo);
            self.vfo_rx_tone[i].set_enabled(idle && has_vfo);
            self.vfo_tx_tone[i].set_enabled(idle && has_vfo);
            self.vfo_direction[i].set_enabled(idle && has_vfo);
            self.vfo_offset[i].set_enabled(idle && has_vfo);
            self.vfo_power[i].set_enabled(idle && has_vfo);
            self.vfo_width[i].set_enabled(idle && has_vfo);
            self.vfo_step[i].set_enabled(idle && has_vfo);
            self.vfo_mute[i].set_enabled(idle && has_vfo);
        }
        let has_logo = self.logo_loaded.borrow().is_some();
        self.logo_read.set_enabled(has_port);
        self.logo_choose.set_enabled(idle && has_logo);
        self.logo_export.set_enabled(idle && has_logo);
        self.logo_save.set_enabled(has_port && has_logo);
        // Always enabled, as they do nothing without a picture: disabled check boxes look jagged
        // in dark mode.
        self.logo_invert.set_enabled(idle);
        self.logo_dither.set_enabled(idle);
        for b in &self.setting_boxes {
            b.set_enabled(idle && has_settings);
        }
    }

    fn message(&self, content: &str, icons: nwg::MessageIcons) {
        nwg::modal_message(
            &self.window,
            &nwg::MessageParams { title: TITLE, content, buttons: nwg::MessageButtons::Ok, icons },
        );
    }

    fn confirm(&self, title: &str, content: &str) -> bool {
        let choice = nwg::modal_message(
            &self.window,
            &nwg::MessageParams {
                title,
                content,
                buttons: nwg::MessageButtons::OkCancel,
                icons: nwg::MessageIcons::Warning,
            },
        );
        choice == nwg::MessageChoice::Ok
    }

    fn start(&self, action: Action) {
        let Some(port) = self.selected_port() else { return };
        let (tx, rx) = channel();
        set_sink(tx.clone());
        *self.rx_msgs.borrow_mut() = Some(rx);
        self.log.set_text("");
        self.progress.set_pos(0);
        self.progress_label.set_text("");
        self.status.set_text("Trabajando. No desconectes el cable ni cierres el programa.");
        self.update_enabled();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_action(action, &port)))
                .unwrap_or_else(|_| Err("Error interno del programa".to_string()));
            let _ = tx.send(Msg::Finished(result));
        });
    }

    fn poll(&self) {
        let mut finished = None;
        let mut read = None;
        let mut settings_read = None;
        let mut logo_read = None;
        let mut vfo_read = None;
        if let Some(rx) = self.rx_msgs.borrow().as_ref() {
            for m in rx.try_iter() {
                match m {
                    Msg::Log(line) => self.log.appendln(&line),
                    Msg::Progress(label, value) => {
                        self.progress.set_pos((value.clamp(0.0, 1.0) * 1000.0) as u32);
                        self.progress_label.set_text(&format!("{label}: {:.0}%", value * 100.0));
                    }
                    Msg::ChannelsRead(area) => read = Some(area),
                    Msg::SettingsRead(data) => settings_read = Some(data),
                    Msg::LogoRead(data) => logo_read = Some(data),
                    Msg::VfoRead(data) => vfo_read = Some(data),
                    Msg::Finished(r) => finished = Some(r),
                }
            }
        }
        if let Some(area) = read {
            *self.channels.borrow_mut() = channels::parse_channels(&area);
            *self.area.borrow_mut() = Some(area);
            self.fill_list();
        }
        if let Some(data) = settings_read {
            for (f, b) in settings::FIELDS.iter().zip(&self.setting_boxes) {
                b.set_selection(settings::selected(&data, f));
            }
            self.message_text.set_text(&settings::message(&data));
            self.password_on.set_selection(Some(settings::password_on(&data) as usize));
            self.password_text.set_text(&settings::password(&data));
            self.scan_text.set_text(&settings::scan_range(&data));
            *self.settings_data.borrow_mut() = Some(data);
        }
        if let Some(data) = vfo_read {
            self.show_vfos(&vfo::parse(&data));
            *self.vfo_data.borrow_mut() = Some(data);
        }
        if let Some(data) = logo_read {
            let screen = data[..logo::SCREEN_SIZE].to_vec();
            *self.logo_gray.borrow_mut() = None;
            *self.logo_loaded.borrow_mut() = Some(data);
            self.show_logo(screen);
        }
        if let Some(r) = finished {
            *self.rx_msgs.borrow_mut() = None;
            self.progress_label.set_text("");
            let (status, message, icon) = match r {
                Ok(text) => (text.clone(), text, nwg::MessageIcons::Info),
                Err(text) => (format!("Error: {text}"), text, nwg::MessageIcons::Error),
            };
            self.status.set_text(&status);
            self.log.appendln(&status);
            self.update_enabled();
            self.message(&message, icon);
        }
    }

    fn browse_firmware(&self) {
        if self.open_dialog.run(Some(&self.window)) {
            if let Ok(file) = self.open_dialog.get_selected_item() {
                let path = PathBuf::from(file);
                self.fw_path.set_text(&path.display().to_string());
                *self.firmware.borrow_mut() = Some(path);
            }
        }
    }

    fn on_install(&self) {
        let file = self.firmware.borrow().clone();
        let shown = file.as_ref().map_or(BUILT_IN.to_string(), |p| p.display().to_string());
        let ok = self.confirm(
            "Instalar firmware",
            &format!(
                "{MODEL_SHORT}\n\nArchivo: {shown}\n\n\
                 Se sustituirá el firmware del radio; este paso no hace copia.\n\
                 El firmware de fábrica no se puede recuperar.\n\n\
                 Al continuar: apaga el radio y enciéndelo con los 2 botones naranjas pulsados.\n\
                 No desconectes el cable durante el proceso."
            ),
        );
        if ok {
            self.start(Action::Install(file));
        }
    }

    fn on_language(&self) {
        let language = if self.voice_en.check_state() == nwg::RadioButtonState::Checked {
            Language::Factory
        } else {
            Language::Spanish
        };
        let ok = self.confirm(
            "Cambiar idioma de voces",
            "Primero se hará una copia de la flash (unos 4 minutos).\n\
             Solo se modifica la zona de voces en español.\n\
             Los canales, la calibración y las voces de fábrica no se tocan.",
        );
        if ok {
            self.start(Action::SetLanguage(language));
        }
    }

    fn on_save_settings(&self) {
        let Some(loaded) = self.settings_data.borrow().clone() else { return };
        let choices: Vec<Option<usize>> = self.setting_boxes.iter().map(|b| b.selection()).collect();
        let ask = self.password_on.selection() == Some(1);
        let code = self.password_text.text();
        let wanted = settings::apply(&loaded, &choices, &self.message_text.text())
            .and_then(|data| settings::apply_password(&data, ask, &code))
            .and_then(|data| settings::apply_scan_range(&data, &self.scan_text.text()));
        let wanted = match wanted {
            Ok(w) => w,
            Err(e) => {
                self.message(&e, nwg::MessageIcons::Error);
                return;
            }
        };
        if wanted == loaded {
            self.message("No hay cambios que guardar.", nwg::MessageIcons::Info);
            return;
        }
        if ask && (!settings::password_on(&loaded) || code.trim() != settings::password(&loaded)) {
            let sure = self.confirm(
                "Clave de encendido",
                &format!(
                    "El radio pedirá la clave {} cada vez que se encienda, y hasta recibirla\n\
                     no se podrá usar ni conectar al PC.\n\nAnótala antes de seguir. ¿Continuar?",
                    code.trim()
                ),
            );
            if !sure {
                return;
            }
        }
        let ok = self.confirm(
            "Guardar ajustes",
            "Se guardará antes una copia de los ajustes actuales. El radio se reiniciará\n\
             dos veces para comprobar que los conservó.\n\n\
             No desconectes el cable ni apagues el radio durante el proceso.",
        );
        if ok {
            *self.settings_data.borrow_mut() = None; // The check after saving fills it again.
            self.start(Action::WriteSettings { loaded, wanted });
        }
    }

    fn show_vfos(&self, list: &[vfo::Vfo]) {
        let in_list = |v: u8, n: usize| ((v as usize) < n).then_some(v as usize);
        for (i, v) in list.iter().enumerate() {
            self.vfo_freq[i].set_text(&vfo::format_freq(v.freq));
            self.vfo_rx_tone[i].set_text(&channels::format_tone(v.rx_tone));
            self.vfo_tx_tone[i].set_text(&channels::format_tone(v.tx_tone));
            self.vfo_direction[i].set_selection(in_list(v.direction, vfo::DIRECTIONS.len()));
            self.vfo_offset[i].set_text(&vfo::format_offset(v.offset));
            self.vfo_power[i].set_selection(in_list(v.power, vfo::POWERS.len()));
            self.vfo_width[i].set_selection(in_list(v.narrow, vfo::WIDTHS.len()));
            self.vfo_step[i].set_selection(in_list(v.step, vfo::STEPS.len()));
            self.vfo_mute[i].set_selection(in_list(v.mute, vfo::MUTES.len()));
        }
    }

    fn edited_vfos(&self, loaded: &[u8]) -> Result<Vec<vfo::Vfo>, String> {
        let mut list = vfo::parse(loaded);
        for (i, v) in list.iter_mut().enumerate() {
            let line = ["A", "B"][i];
            let with_line = |e: String| format!("VFO {line}: {e}");
            let freq_text = self.vfo_freq[i].text();
            // An unchanged frequency stays as read, even if the radio holds an unusual one.
            if freq_text.trim() != vfo::format_freq(v.freq) {
                v.freq = channels::parse_mhz(&freq_text).map_err(with_line)?;
            }
            v.rx_tone = channels::parse_tone(&self.vfo_rx_tone[i].text()).map_err(with_line)?;
            v.tx_tone = channels::parse_tone(&self.vfo_tx_tone[i].text()).map_err(with_line)?;
            let offset_text = self.vfo_offset[i].text();
            if offset_text.trim() != vfo::format_offset(v.offset) {
                v.offset = vfo::parse_offset(&offset_text).map_err(with_line)?;
            }
            // A list left unselected keeps what the record has.
            let pick = |b: &nwg::ComboBox<String>, old: u8| b.selection().map_or(old, |x| x as u8);
            v.direction = pick(&self.vfo_direction[i], v.direction);
            v.power = pick(&self.vfo_power[i], v.power);
            v.narrow = pick(&self.vfo_width[i], v.narrow);
            v.step = pick(&self.vfo_step[i], v.step);
            v.mute = pick(&self.vfo_mute[i], v.mute);
        }
        Ok(list)
    }

    fn on_channel_to_vfo(&self, line: usize) {
        let Some(loaded) = self.vfo_data.borrow().clone() else { return };
        let Some(at) = self.vfo_channel.selection() else {
            self.message("Elige un canal de la lista.", nwg::MessageIcons::Warning);
            return;
        };
        let list = self.channels.borrow();
        let Some(channel) = list.get(at) else { return };
        // The other line keeps what's on screen, if that's valid so far.
        let mut vfos = self.edited_vfos(&loaded).unwrap_or_else(|_| vfo::parse(&loaded));
        match vfo::from_channel(channel, &vfos[line]) {
            Ok(v) => {
                vfos[line] = v;
                self.show_vfos(&vfos);
                self.status.set_text(&format!(
                    "Canal {} pasado a la línea {}. Pendiente: \"Guardar en radio\".",
                    channel.number,
                    ["A", "B"][line]
                ));
            }
            Err(e) => self.message(&e, nwg::MessageIcons::Warning),
        }
    }

    fn on_import_vfos(&self) {
        let Some(loaded) = self.vfo_data.borrow().clone() else { return };
        if !self.vfo_open.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.vfo_open.get_selected_item() else { return };
        // A line not in the file keeps what's on screen, if that's valid so far.
        let current = self.edited_vfos(&loaded).unwrap_or_else(|_| vfo::parse(&loaded));
        let result = fs::read(&file)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| format!("No se pudo leer el archivo: {e}"))
            .and_then(|text| vfo::from_csv(&text, &current));
        match result {
            Ok(list) => {
                self.show_vfos(&list);
                self.status.set_text("VFO importados. Pendiente: \"Guardar en radio\".");
            }
            Err(e) => self.message(&e, nwg::MessageIcons::Error),
        }
    }

    fn on_export_vfos(&self) {
        let Some(loaded) = self.vfo_data.borrow().clone() else { return };
        let list = match self.edited_vfos(&loaded) {
            Ok(list) => list,
            Err(e) => {
                self.message(&e, nwg::MessageIcons::Warning);
                return;
            }
        };
        if !self.vfo_saveas.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.vfo_saveas.get_selected_item() else { return };
        let mut path = PathBuf::from(file);
        if path.extension().is_none() {
            path.set_extension("csv");
        }
        match fs::write(&path, vfo::to_csv(&list)) {
            Ok(()) => self.status.set_text(&format!("Exportado: {}", path.display())),
            Err(e) => self.message(&format!("No se pudo guardar: {e}"), nwg::MessageIcons::Error),
        }
    }

    fn on_save_vfos(&self) {
        let Some(loaded) = self.vfo_data.borrow().clone() else { return };
        let wanted = self.edited_vfos(&loaded).and_then(|list| vfo::apply(&loaded, &list));
        let wanted = match wanted {
            Ok(w) => w,
            Err(e) => {
                self.message(&e, nwg::MessageIcons::Warning);
                return;
            }
        };
        if wanted == loaded {
            self.message("No hay cambios que guardar.", nwg::MessageIcons::Info);
            return;
        }
        let ok = self.confirm(
            "Guardar VFO",
            "Se guardará antes una copia de los VFO actuales. El radio se reiniciará\n\
             dos veces para comprobar que los conservó.\n\n\
             No desconectes el cable ni apagues el radio durante el proceso.",
        );
        if ok {
            *self.vfo_data.borrow_mut() = None; // The check after saving fills it again.
            self.start(Action::WriteVfo { loaded, wanted });
        }
    }

    fn on_restore(&self) {
        if !self.restore_open.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.restore_open.get_selected_item() else { return };
        let path = PathBuf::from(file);
        let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let len = fs::metadata(&path).map_or(0, |m| m.len() as usize);
        let kind = match restore::classify(&name, len) {
            Ok(kind) => kind,
            Err(e) => {
                self.message(&e, nwg::MessageIcons::Warning);
                return;
            }
        };
        let what = match kind {
            restore::Kind::Channels => "Reemplazará todos los canales del radio por los de la copia.",
            restore::Kind::Settings => "Reemplazará los ajustes del radio por los de la copia, incluidos la clave de encendido y el mensaje.",
            restore::Kind::Vfo => "Reemplazará los VFO A y B por los de la copia.",
            restore::Kind::Logo => "Reemplazará el logo por el de la copia.",
            restore::Kind::FullFlash => "De una copia completa solo se usan los canales y el logo: reemplazará los del radio. La calibración y los ajustes no se tocan.",
        };
        let ok = self.confirm(
            "Restaurar copia",
            &format!("{name}\n\n{what}\n\nAntes se guardará una copia de lo actual. No desconectes el cable ni apagues el radio."),
        );
        if ok {
            self.forget_loaded();
            self.start(Action::Restore(path));
        }
    }

    /// After a change made from outside the tabs, what they show may no longer be true.
    fn forget_loaded(&self) {
        *self.area.borrow_mut() = None;
        *self.settings_data.borrow_mut() = None;
        *self.vfo_data.borrow_mut() = None;
        *self.logo_loaded.borrow_mut() = None;
        self.channels.borrow_mut().clear();
        self.fill_list();
    }

    fn on_export_profile(&self) {
        if !self.profile_saveas.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.profile_saveas.get_selected_item() else { return };
        let mut path = PathBuf::from(file);
        if path.extension().is_none() {
            path.set_extension("txt");
        }
        self.start(Action::ExportProfile(path));
    }

    fn on_import_profile(&self) {
        if !self.profile_open.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.profile_open.get_selected_item() else { return };
        let ok = self.confirm(
            "Aplicar perfil",
            "Reemplazará los canales, los VFO y los ajustes del radio por los del perfil. La clave de encendido no cambia.\n\n\
             Se comprueba todo el archivo antes de escribir, y se guarda antes una copia de lo actual.\n\
             No desconectes el cable ni apagues el radio: puede reiniciarse varias veces.",
        );
        if ok {
            self.forget_loaded();
            self.start(Action::ImportProfile(PathBuf::from(file)));
        }
    }

    fn on_settings_guide(&self) {
        self.log.set_text("");
        self.log.appendln("Qué hace cada ajuste:");
        for f in settings::FIELDS {
            self.log.appendln(&format!("{}: {}", f.label, settings::help(f)));
        }
        self.log.appendln(&format!("Mensaje inicio: {}", settings::MESSAGE_HELP));
        self.log.appendln(&format!("Pedir clave: {}", settings::PASSWORD_HELP));
        self.log.appendln(&format!("Escaneo VFO: {}", settings::SCAN_HELP));
        self.status.set_text("La explicación de cada ajuste está en el cuadro de abajo.");
    }

    fn show_setting_help(&self, handle: nwg::ControlHandle) {
        let option = channels::CHANNEL_OPTIONS.iter().zip(&self.option_boxes).find(|(_, b)| b.handle == handle);
        if let Some((o, _)) = option {
            self.status.set_text(&format!("{}: {}", o.label, o.help));
        }
        let field = settings::FIELDS.iter().zip(&self.setting_boxes).find(|(_, b)| b.handle == handle);
        if let Some((f, _)) = field {
            self.status.set_text(&format!("{}: {}", f.label, settings::help(f)));
        }
    }

    fn show_logo(&self, screen: Vec<u8>) {
        let bmp = logo::to_bmp(&screen, self.logo_scale.get().max(2), [32, 40, 32], [200, 216, 160]);
        if let Ok(bitmap) = nwg::Bitmap::from_bin(&bmp) {
            self.logo_frame.set_bitmap(Some(&bitmap));
            *self.logo_bitmap.borrow_mut() = bitmap;
        }
        *self.logo_screen.borrow_mut() = Some(screen);
    }

    fn render_logo(&self) {
        let Some(screen) = self.logo_gray.borrow().as_ref().map(|gray| {
            logo::to_screen(
                gray,
                self.logo_invert.check_state() == nwg::CheckBoxState::Checked,
                self.logo_dither.check_state() == nwg::CheckBoxState::Checked,
            )
        }) else {
            return;
        };
        self.show_logo(screen);
    }

    fn on_choose_logo(&self) {
        if !self.logo_open.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.logo_open.get_selected_item() else { return };
        match load_picture(&file.to_string_lossy()) {
            Ok(gray) => {
                *self.logo_gray.borrow_mut() = Some(gray);
                self.render_logo();
                self.update_enabled();
                self.status.set_text("Así se verá en el radio. Pendiente: \"Guardar en radio\".");
            }
            Err(e) => self.message(&e, nwg::MessageIcons::Error),
        }
    }

    fn on_export_logo(&self) {
        if !self.logo_saveas.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.logo_saveas.get_selected_item() else { return };
        let mut path = PathBuf::from(file);
        if path.extension().is_none() {
            path.set_extension("bmp");
        }
        let Some(screen) = self.logo_screen.borrow().clone() else { return };
        match fs::write(&path, logo::to_bmp(&screen, 1, [0, 0, 0], [255, 255, 255])) {
            Ok(()) => self.status.set_text(&format!("Exportado: {}", path.display())),
            Err(e) => self.message(&format!("No se pudo guardar: {e}"), nwg::MessageIcons::Error),
        }
    }

    fn on_save_logo(&self) {
        let Some(loaded) = self.logo_loaded.borrow().clone() else { return };
        let Some(screen) = self.logo_screen.borrow().clone() else { return };
        if screen == loaded[..logo::SCREEN_SIZE] {
            self.message("No hay cambios que guardar.", nwg::MessageIcons::Info);
            return;
        }
        let ok = self.confirm(
            "Guardar logo",
            "Se guardará antes una copia del logo actual en la carpeta backups.\n\n\
             No desconectes el cable ni apagues el radio durante el proceso.",
        );
        if ok {
            *self.logo_loaded.borrow_mut() = None; // Must be read again afterwards.
            *self.logo_gray.borrow_mut() = None;
            self.start(Action::WriteLogo { loaded, screen });
        }
    }

    fn on_import_settings(&self) {
        if !self.settings_open.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.settings_open.get_selected_item() else { return };
        // Only the key and number columns matter, so a spreadsheet's text encoding can't hurt.
        let result = fs::read(&file)
            .map_err(|e| format!("No se pudo leer el archivo: {e}"))
            .and_then(|bytes| {
                let text = String::from_utf8_lossy(&bytes);
                settings::from_csv(&text).map(|parsed| (parsed, settings::scan_from_csv(&text)))
            });
        match result {
            Ok(((choices, text), imported_scan)) => {
                let mut n = 0;
                if let Some(text) = text {
                    self.message_text.set_text(&text);
                    n += 1;
                }
                if let Some(scan) = imported_scan {
                    self.scan_text.set_text(&scan);
                    n += 1;
                }
                for (b, choice) in self.setting_boxes.iter().zip(choices) {
                    if choice.is_some() {
                        b.set_selection(choice);
                        n += 1;
                    }
                }
                self.status.set_text(&format!("{n} ajustes importados. Pendiente: \"Guardar en radio\"."));
            }
            Err(e) => self.message(&e, nwg::MessageIcons::Error),
        }
    }

    fn on_export_settings(&self) {
        if !self.settings_saveas.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.settings_saveas.get_selected_item() else { return };
        let mut path = PathBuf::from(file);
        if path.extension().is_none() {
            path.set_extension("csv");
        }
        let choices: Vec<Option<usize>> = self.setting_boxes.iter().map(|b| b.selection()).collect();
        let file = settings::to_full_csv(&choices, &self.message_text.text(), self.scan_text.text().trim());
        match fs::write(&path, file) {
            Ok(()) => self.status.set_text(&format!("Exportado: {}", path.display())),
            Err(e) => self.message(&format!("No se pudo guardar: {e}"), nwg::MessageIcons::Error),
        }
    }

    /// Fits everything to the window.
    fn layout(&self) {
        let (cw, ch) = self.window.size();
        if cw == 0 || ch == 0 {
            return; // Minimized.
        }
        let w = (cw as i32).max(BASE_WIDTH);
        let h = (ch as i32).max(BASE_HEIGHT);
        let full = (w - 24) as u32;

        // Extra height goes mostly to the tabs, the rest to the message box.
        let extra = h - BASE_HEIGHT;
        let tabs_h = BASE_TABS_HEIGHT + extra * 3 / 5;
        let log_y = 72 + tabs_h + 8;
        let log_h = h - 110 - log_y;
        self.model_label.set_size((w - 32) as u32, 22);
        self.tabs.set_size(full, tabs_h as u32);
        self.log.set_position(12, log_y);
        self.log.set_size(full, log_h as u32);
        self.progress_label.set_position(12, h - 104);
        self.progress_label.set_size(full, 20);
        self.progress.set_position(12, h - 82);
        self.progress.set_size(full, 18);
        self.status.set_position(12, h - 56);
        self.status.set_size(full, 40);

        // Inside a tab.
        let tw = w - 24;
        let th = tabs_h - TAB_CHROME_HEIGHT;

        // Radio.
        self.backup_label.set_size((tw - 226) as u32, 22);
        let path_w = tw - 316;
        self.fw_path.set_size(path_w as u32, 26);
        self.fw_browse.set_position(92 + path_w + 8, 71);
        self.text_label.set_size((tw - 36) as u32, 22);

        // Channels: the list and its name column take the extra room.
        let list_w = tw - 496;
        let list_h = th - 90;
        self.list.set_size(list_w as u32, list_h as u32);
        self.list.update_column(3, nwg::InsertListViewColumn {
            index: Some(3),
            fmt: None,
            width: Some(98 + list_w - 500),
            text: Some("Nombre".to_string()),
        });
        let x = 10 + list_w + 14;
        for (y, label, input) in [
            (10, &self.num_label, &self.num),
            (46, &self.rx_label, &self.rx),
            (82, &self.tx_label, &self.tx),
            (118, &self.name_label, &self.name),
            (154, &self.rx_tone_label, &self.rx_tone),
            (190, &self.tx_tone_label, &self.tx_tone),
        ] {
            label.set_position(x, y + 2);
            input.set_position(x + 114, y);
        }
        for (i, (label, input)) in self.option_labels.iter().zip(&self.option_boxes).enumerate() {
            let y = 10 + i as i32 * 36;
            label.set_position(x + 232, y + 2);
            input.set_position(x + 330, y);
        }
        self.apply.set_position(x, 226);
        self.delete.set_position(x + 114, 226);
        self.tone_hint.set_position(10, th - 74);
        self.tone_hint.set_size(list_w as u32, 22);
        self.read.set_position(10, th - 42);
        self.save.set_position(170, th - 42);
        self.import.set_position(tw - 316, th - 42);
        self.export.set_position(tw - 166, th - 42);

        // Settings: wider columns, and rows spread out up to a comfortable distance.
        const ROWS: i32 = 11;
        let buttons_y = th - 30;
        let row_h = ((buttons_y - 6) / ROWS).clamp(25, 34);
        let column = (tw - 8) / 4;
        let place = |i: i32| (8 + (i / ROWS) * column, 6 + (i % ROWS) * row_h);
        let box_w = (column - 118) as u32;
        for (i, (label, input)) in self.setting_labels.iter().zip(&self.setting_boxes).enumerate() {
            let (x, y) = place(i as i32);
            label.set_position(x, y + 3);
            input.set_position(x + 106, y);
            input.set_size(box_w, 24);
        }
        let (x, y) = place(settings::FIELDS.len() as i32);
        self.message_label.set_position(x, y + 3);
        self.message_text.set_position(x + 106, y);
        self.message_text.set_size(box_w, 24);
        let (x, y) = place(settings::FIELDS.len() as i32 + 1);
        self.password_on_label.set_position(x, y + 3);
        self.password_on.set_position(x + 106, y);
        self.password_on.set_size(box_w, 24);
        let (x, y) = place(settings::FIELDS.len() as i32 + 2);
        self.password_label.set_position(x, y + 3);
        self.password_text.set_position(x + 106, y);
        self.password_text.set_size(box_w, 24);
        let (x, y) = place(settings::FIELDS.len() as i32 + 3);
        self.scan_label.set_position(x, y + 3);
        self.scan_text.set_position(x + 106, y);
        self.scan_text.set_size(box_w, 24);
        self.settings_read.set_position(8, buttons_y);
        self.settings_save.set_position(130, buttons_y);
        self.settings_import.set_position(262, buttons_y);
        self.settings_export.set_position(380, buttons_y);
        self.settings_help.set_position(498, buttons_y);

        // Logo: the preview grows in whole steps, so the dots stay even.
        let scale = (((tw - 412) / logo::WIDTH as i32).min((th - 100) / logo::HEIGHT as i32)).max(2) as usize;
        let (pw, ph) = ((logo::WIDTH * scale) as i32, (logo::HEIGHT * scale) as i32);
        self.logo_frame.set_size(pw as u32, ph as u32);
        let x = 12 + pw + 20;
        self.logo_read.set_position(x, 12);
        self.logo_choose.set_position(x, 48);
        self.logo_export.set_position(x, 84);
        self.logo_save.set_position(x + 190, 12);
        self.logo_invert.set_position(x + 190, 50);
        self.logo_dither.set_position(x + 190, 86);
        self.logo_hint.set_position(12, 12 + ph + 16);
        self.logo_hint.set_size((tw - 36) as u32, 66);
        if scale != self.logo_scale.replace(scale) {
            if let Some(screen) = self.logo_screen.borrow().clone() {
                self.show_logo(screen);
            }
        }
    }

    fn fill_list(&self) {
        let names: Vec<String> = self
            .channels
            .borrow()
            .iter()
            .map(|c| format!("{:03}  {}  {}", c.number, channels::format_mhz(c.rx), c.name))
            .collect();
        self.vfo_channel.set_collection(names);
        self.vfo_channel.set_selection(None);
        self.update_enabled();
        self.list.clear();
        for c in self.channels.borrow().iter() {
            let row = [
                c.number.to_string(),
                channels::format_mhz(c.rx),
                channels::format_mhz(c.tx),
                c.name.clone(),
                channels::format_tone(c.rx_tone),
                channels::format_tone(c.tx_tone),
                option_text(c, 0),
                option_text(c, 1),
            ];
            self.list.insert_items_row(None, &row);
        }
    }

    fn on_select(&self) {
        let Some(i) = self.list.selected_item() else { return };
        let list = self.channels.borrow();
        let Some(c) = list.get(i) else { return };
        self.num.set_text(&c.number.to_string());
        self.rx.set_text(&channels::format_mhz(c.rx));
        self.tx.set_text(&channels::format_mhz(c.tx));
        self.name.set_text(&c.name);
        self.rx_tone.set_text(&channels::format_tone(c.rx_tone));
        self.tx_tone.set_text(&channels::format_tone(c.tx_tone));
        for ((b, o), v) in self.option_boxes.iter().zip(&channels::CHANNEL_OPTIONS).zip(c.options) {
            b.set_selection(((v as usize) < o.options.len()).then_some(v as usize));
        }
    }

    fn edited_channel(&self) -> Result<Channel, String> {
        let number: usize = self
            .num
            .text()
            .trim()
            .parse()
            .map_err(|_| format!("El canal debe ser un número de 1 a {}", channels::CHANNEL_COUNT))?;
        if number == 0 || number > channels::CHANNEL_COUNT {
            return Err(format!("El canal debe ser un número de 1 a {}", channels::CHANNEL_COUNT));
        }
        let rx = channels::parse_mhz(&self.rx.text())?;
        let tx_text = self.tx.text();
        let tx = if tx_text.trim().is_empty() { rx } else { channels::parse_mhz(&tx_text)? };
        let name = self.name.text().trim().to_string();
        channels::check_name(&name)?;
        let rx_tone = channels::parse_tone(&self.rx_tone.text())?;
        let tx_tone = channels::parse_tone(&self.tx_tone.text())?;
        // An option left unselected keeps what the channel has.
        let known = self.channels.borrow().iter().find(|c| c.number == number).map(|c| c.options);
        let mut options = known.unwrap_or_else(channels::default_options);
        for (v, b) in options.iter_mut().zip(&self.option_boxes) {
            if let Some(i) = b.selection() {
                *v = i as u8;
            }
        }
        Ok(Channel { number, rx, tx, name, rx_tone, tx_tone, options })
    }

    fn on_apply(&self) {
        match self.edited_channel() {
            Ok(c) => {
                {
                    let mut list = self.channels.borrow_mut();
                    list.retain(|o| o.number != c.number);
                    list.push(c);
                    list.sort_by_key(|o| o.number);
                }
                self.fill_list();
                self.status.set_text("Cambio pendiente: pulsa \"Guardar en radio\" para grabarlo.");
            }
            Err(e) => self.message(&e, nwg::MessageIcons::Warning),
        }
    }

    fn on_delete(&self) {
        let Ok(number) = self.num.text().trim().parse::<usize>() else {
            self.message("Elige un canal de la lista.", nwg::MessageIcons::Warning);
            return;
        };
        self.channels.borrow_mut().retain(|c| c.number != number);
        self.fill_list();
        self.status.set_text("Cambio pendiente: pulsa \"Guardar en radio\" para grabarlo.");
    }

    fn on_save(&self) {
        let Some(area) = self.area.borrow().clone() else { return };
        let wanted = match channels::apply_channels(&area, &self.channels.borrow()) {
            Ok(w) => w,
            Err(e) => {
                self.message(&e, nwg::MessageIcons::Error);
                return;
            }
        };
        if wanted == area {
            self.message("No hay cambios que guardar.", nwg::MessageIcons::Info);
            return;
        }
        let ok = self.confirm(
            "Guardar canales",
            "Se guardará antes una copia de los canales actuales y luego se\n\
             reescribirán solo las zonas de memoria que cambiaron.\n\n\
             No desconectes el cable ni apagues el radio durante el proceso.",
        );
        if ok {
            *self.area.borrow_mut() = None; // Must be read again afterwards.
            self.channels.borrow_mut().clear();
            self.list.clear();
            self.start(Action::WriteChannels { loaded: area, wanted });
        }
    }

    fn on_import(&self) {
        if !self.csv_open.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.csv_open.get_selected_item() else { return };
        // Spreadsheets may save in another text encoding: names are checked afterwards anyway.
        let result = fs::read(&file)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| format!("No se pudo leer el archivo: {e}"))
            .and_then(|text| channels::from_csv(&text, &self.channels.borrow()));
        match result {
            Ok(list) => {
                let n = list.len();
                *self.channels.borrow_mut() = list;
                self.fill_list();
                self.status.set_text(&format!("{n} canales importados. Pendiente: \"Guardar en radio\"."));
            }
            Err(e) => self.message(&e, nwg::MessageIcons::Error),
        }
    }

    fn on_export(&self) {
        if !self.csv_save.run(Some(&self.window)) {
            return;
        }
        let Ok(file) = self.csv_save.get_selected_item() else { return };
        let mut path = PathBuf::from(file);
        if path.extension().is_none() {
            path.set_extension("csv");
        }
        match fs::write(&path, channels::to_csv(&self.channels.borrow())) {
            Ok(()) => self.status.set_text(&format!("Exportado: {}", path.display())),
            Err(e) => self.message(&format!("No se pudo guardar: {e}"), nwg::MessageIcons::Error),
        }
    }
}

fn option_text(c: &Channel, i: usize) -> String {
    let at = c.options[i] as usize;
    channels::CHANNEL_OPTIONS[i].options.get(at).map_or_else(|| format!("?{at}"), |t| t.to_string())
}

/// Loads any picture Windows can read as brightness values for the whole screen.
fn load_picture(path: &str) -> Result<Vec<u8>, String> {
    let fail = |e: nwg::NwgError| format!("No se pudo abrir la imagen: {e}");
    let decoder = nwg::ImageDecoder::new().map_err(fail)?;
    let frame = decoder.from_filename(path).and_then(|s| s.frame(0)).map_err(fail)?;
    let (w, h) = frame.size();
    let (nw, nh) = logo::fit_size(w, h);
    let scaled = decoder.resize_image(&frame, [nw, nh]).map_err(fail)?;
    let (nw, nh) = scaled.size();
    // These formats share one identifier that differs in its last byte: see Windows' wincodec.h.
    // Byte order per format: blue-green-red, red-green-blue, or gray.
    let (size, red, green, blue, alpha) = match scaled.pixel_format().Data4[7] {
        0x0C => (3, 2, 1, 0, None),
        0x0D => (3, 0, 1, 2, None),
        0x0E => (4, 2, 1, 0, None),
        0x0F | 0x10 => (4, 2, 1, 0, Some(3)),
        0x08 => (1, 0, 0, 0, None),
        _ => return Err("Formato de imagen no compatible. Guárdala como PNG o BMP de 24 bits.".to_string()),
    };
    let pixels = scaled.pixels(size as u32).map_err(fail)?;
    let gray: Vec<u8> = pixels
        .chunks_exact(size)
        .map(|p| {
            let luma = (p[red] as u32 * 299 + p[green] as u32 * 587 + p[blue] as u32 * 114) / 1000;
            // Transparent parts show as white.
            let a = alpha.map_or(255, |i| p[i] as u32);
            ((luma * a + 255 * (255 - a)) / 255) as u8
        })
        .collect();
    Ok(logo::center_on_white(&gray, nw as usize, nh as usize))
}

fn build(ui: &mut Ui) -> Result<(), nwg::NwgError> {
    nwg::Window::builder()
        .size((BASE_WIDTH, OPEN_HEIGHT))
        .center(true)
        .title(TITLE)
        .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::VISIBLE | nwg::WindowFlags::RESIZABLE)
        .build(&mut ui.window)?;
    let w = &ui.window;

    nwg::Label::builder().text(MODEL_SHORT).position((16, 8)).size((708, 22)).parent(w).build(&mut ui.model_label)?;
    nwg::Label::builder().text("Puerto COM:").position((16, 40)).size((90, 22)).parent(w).build(&mut ui.port_label)?;
    nwg::ComboBox::builder().position((110, 36)).size((220, 26)).parent(w).build(&mut ui.port)?;
    nwg::Button::builder().text("Actualizar lista").position((340, 35)).size((130, 28)).parent(w).build(&mut ui.refresh)?;
    nwg::CheckBox::builder()
        .text("Modo oscuro")
        .position((490, 37))
        .size((130, 24))
        .check_state(if theme::saved_dark() { nwg::CheckBoxState::Checked } else { nwg::CheckBoxState::Unchecked })
        .parent(w)
        .build(&mut ui.dark)?;

    nwg::TabsContainer::builder().position((12, 72)).size((716, 340)).parent(w).build(&mut ui.tabs)?;
    nwg::Tab::builder().text("Radio").parent(&ui.tabs).build(&mut ui.tab_radio)?;
    nwg::Tab::builder().text("Canales").parent(&ui.tabs).build(&mut ui.tab_channels)?;
    nwg::Tab::builder().text("Ajustes").parent(&ui.tabs).build(&mut ui.tab_settings)?;
    nwg::Tab::builder().text("VFO").parent(&ui.tabs).build(&mut ui.tab_vfo)?;
    nwg::Tab::builder().text("Logo").parent(&ui.tabs).build(&mut ui.tab_logo)?;

    let t = &ui.tab_radio;
    nwg::Button::builder().text("Sacar copia").position((12, 14)).size((170, 34)).parent(t).build(&mut ui.backup)?;
    nwg::Label::builder()
        .text("Guarda la flash en la carpeta backups. No cambia el radio.")
        .position((196, 22))
        .size((490, 22))
        .background_color(Some([255, 255, 255])).parent(t)
        .build(&mut ui.backup_label)?;

    nwg::Label::builder().text("Firmware:").position((12, 76)).size((80, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.fw_label)?;
    nwg::TextInput::builder().text(BUILT_IN).readonly(true).position((92, 73)).size((400, 26)).parent(t).build(&mut ui.fw_path)?;
    nwg::Button::builder().text("Elegir...").position((500, 71)).size((90, 30)).parent(t).build(&mut ui.fw_browse)?;
    nwg::Button::builder().text("Instalar firmware").position((92, 110)).size((170, 34)).parent(t).build(&mut ui.install)?;

    nwg::Label::builder().text("Voces:").position((12, 172)).size((80, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.voice_label)?;
    nwg::RadioButton::builder()
        .text("Español (Gonzalo)")
        .flags(nwg::RadioButtonFlags::VISIBLE | nwg::RadioButtonFlags::GROUP)
        .check_state(nwg::RadioButtonState::Checked)
        .position((92, 170))
        .size((160, 24))
        .background_color(Some([255, 255, 255])).parent(t)
        .build(&mut ui.voice_es)?;
    nwg::RadioButton::builder().text("Inglés de fábrica").position((262, 170)).size((150, 24)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.voice_en)?;
    nwg::Button::builder().text("Cambiar idioma de voces").position((92, 202)).size((200, 34)).parent(t).build(&mut ui.language)?;
    nwg::Label::builder()
        .text("Idioma de los textos: pestaña Ajustes, o en el radio MENU > Language > Español.")
        .position((12, 258))
        .size((680, 22))
        .background_color(Some([255, 255, 255])).parent(t)
        .build(&mut ui.text_label)?;
    nwg::Button::builder().text("Restaurar copia...").position((12, 284)).size((170, 28)).parent(t).build(&mut ui.restore)?;
    nwg::Button::builder().text("Guardar perfil...").position((192, 284)).size((170, 28)).parent(t).build(&mut ui.profile_export)?;
    nwg::Button::builder().text("Aplicar perfil...").position((372, 284)).size((170, 28)).parent(t).build(&mut ui.profile_import)?;

    let t = &ui.tab_channels;
    nwg::ListView::builder()
        .position((10, 10))
        .size((440, 222))
        .list_style(nwg::ListViewStyle::Detailed)
        .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT | nwg::ListViewExFlags::GRID)
        .parent(t)
        .build(&mut ui.list)?;
    for (i, (text, width)) in [("Canal", 46), ("RX (MHz)", 70), ("TX (MHz)", 70), ("Nombre", 98), ("Tono RX", 58), ("Tono TX", 58), ("Pot.", 44), ("Ancho", 56)].iter().enumerate() {
        ui.list.insert_column(nwg::InsertListViewColumn {
            index: Some(i as i32),
            fmt: None,
            width: Some(*width),
            text: Some(text.to_string()),
        });
    }
    ui.list.set_headers_enabled(true);

    let x = 464;
    nwg::Label::builder().text("Canal (1-999):").position((x, 12)).size((110, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.num_label)?;
    nwg::TextInput::builder().position((x + 114, 10)).size((110, 26)).parent(t).build(&mut ui.num)?;
    nwg::Label::builder().text("RX (MHz):").position((x, 48)).size((110, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.rx_label)?;
    nwg::TextInput::builder().position((x + 114, 46)).size((110, 26)).parent(t).build(&mut ui.rx)?;
    nwg::Label::builder().text("TX (MHz):").position((x, 84)).size((110, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.tx_label)?;
    nwg::TextInput::builder().position((x + 114, 82)).size((110, 26)).parent(t).build(&mut ui.tx)?;
    nwg::Label::builder().text("Nombre:").position((x, 120)).size((110, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.name_label)?;
    nwg::TextInput::builder().limit(12).position((x + 114, 118)).size((110, 26)).parent(t).build(&mut ui.name)?;
    nwg::Label::builder().text("Tono RX:").position((x, 156)).size((110, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.rx_tone_label)?;
    nwg::TextInput::builder().limit(6).position((x + 114, 154)).size((110, 26)).parent(t).build(&mut ui.rx_tone)?;
    nwg::Label::builder().text("Tono TX:").position((x, 192)).size((110, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.tx_tone_label)?;
    nwg::TextInput::builder().limit(6).position((x + 114, 190)).size((110, 26)).parent(t).build(&mut ui.tx_tone)?;
    ui.option_labels = channels::CHANNEL_OPTIONS.iter().map(|_| Default::default()).collect();
    ui.option_boxes = channels::CHANNEL_OPTIONS.iter().map(|_| Default::default()).collect();
    for (i, o) in channels::CHANNEL_OPTIONS.iter().enumerate() {
        let y = 10 + i as i32 * 36;
        nwg::Label::builder().text(o.label).position((x + 232, y + 2)).size((96, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.option_labels[i])?;
        nwg::ComboBox::builder()
            .collection(o.options.iter().map(|x| x.to_string()).collect())
            .position((x + 330, y))
            .size((118, 26))
            .parent(t)
            .build(&mut ui.option_boxes[i])?;
    }
    nwg::Button::builder().text("Aplicar").position((x, 226)).size((110, 32)).parent(t).build(&mut ui.apply)?;
    nwg::Button::builder().text("Borrar canal").position((x + 114, 226)).size((110, 32)).parent(t).build(&mut ui.delete)?;
    nwg::Label::builder().text("Tonos: 88.5 (CTCSS), D023N o D023I (DCS). Vacío: sin tono.").position((10, 238)).size((440, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.tone_hint)?;

    nwg::Button::builder().text("Leer del radio").position((10, 270)).size((150, 32)).parent(t).build(&mut ui.read)?;
    nwg::Button::builder().text("Guardar en radio").position((170, 270)).size((150, 32)).parent(t).build(&mut ui.save)?;
    nwg::Button::builder().text("Importar CSV...").position((400, 270)).size((140, 32)).parent(t).build(&mut ui.import)?;
    nwg::Button::builder().text("Exportar CSV...").position((550, 270)).size((140, 32)).parent(t).build(&mut ui.export)?;

    let t = &ui.tab_settings;
    const ROWS: usize = 11;
    ui.setting_labels = settings::FIELDS.iter().map(|_| Default::default()).collect();
    ui.setting_boxes = settings::FIELDS.iter().map(|_| Default::default()).collect();
    for (i, f) in settings::FIELDS.iter().enumerate() {
        let x = 8 + (i / ROWS) as i32 * 236;
        let y = 6 + (i % ROWS) as i32 * 25;
        nwg::Label::builder()
            .text(f.label)
            .position((x, y + 3))
            .size((104, 20))
            .background_color(Some([255, 255, 255]))
            .parent(t)
            .build(&mut ui.setting_labels[i])?;
        nwg::ComboBox::builder()
            .collection(f.options.iter().map(|o| o.to_string()).collect())
            .position((x + 106, y))
            .size((118, 24))
            .parent(t)
            .build(&mut ui.setting_boxes[i])?;
    }
    // The free place at the end of the last column.
    let (x, y) = (8 + 2 * 236, 6 + 10 * 25);
    nwg::Label::builder().text("Mensaje inicio").position((x, y + 3)).size((104, 20)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.message_label)?;
    nwg::TextInput::builder().limit(16).position((x + 106, y)).size((118, 24)).parent(t).build(&mut ui.message_text)?;
    nwg::Label::builder().text("Escaneo VFO").position((x, y + 3)).size((104, 20)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.scan_label)?;
    nwg::TextInput::builder().limit(7).position((x + 106, y)).size((118, 24)).parent(t).build(&mut ui.scan_text)?;
    nwg::Tooltip::builder().build(&mut ui.tooltip)?;
    ui.tooltip.set_delay_time(Some(200));
    for (f, b) in settings::FIELDS.iter().zip(&ui.setting_boxes) {
        ui.tooltip.register(b, settings::help(f));
    }
    ui.tooltip.register(&ui.message_text, settings::MESSAGE_HELP);
    for (o, b) in channels::CHANNEL_OPTIONS.iter().zip(&ui.option_boxes) {
        ui.tooltip.register(b, o.help);
    }
    nwg::Label::builder().text("Pedir clave").position((x, y + 3)).size((104, 20)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.password_on_label)?;
    nwg::ComboBox::builder().collection(vec!["No".to_string(), "Sí".to_string()]).position((x + 106, y)).size((118, 24)).parent(t).build(&mut ui.password_on)?;
    nwg::Label::builder().text("Clave (6 núm.)").position((x, y + 3)).size((104, 20)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.password_label)?;
    nwg::TextInput::builder().limit(6).position((x + 106, y)).size((118, 24)).parent(t).build(&mut ui.password_text)?;
    ui.tooltip.register(&ui.password_on, settings::PASSWORD_HELP);
    ui.tooltip.register(&ui.password_text, settings::PASSWORD_HELP);
    ui.tooltip.register(&ui.scan_text, settings::SCAN_HELP);
    nwg::Button::builder().text("Leer del radio").position((8, 282)).size((116, 28)).parent(t).build(&mut ui.settings_read)?;
    nwg::Button::builder().text("Guardar en radio").position((130, 282)).size((126, 28)).parent(t).build(&mut ui.settings_save)?;
    nwg::Button::builder().text("Importar CSV...").position((262, 282)).size((112, 28)).parent(t).build(&mut ui.settings_import)?;
    nwg::Button::builder().text("Exportar CSV...").position((380, 282)).size((112, 28)).parent(t).build(&mut ui.settings_export)?;
    nwg::Button::builder().text("¿Qué hace cada ajuste?").position((498, 282)).size((196, 28)).parent(t).build(&mut ui.settings_help)?;

    let t = &ui.tab_vfo;
    const ROW: i32 = 29;
    let names = ["Frecuencia (MHz)", "Tono RX", "Tono TX", "Dirección", "Desplaz. (MHz)", "Potencia", "Ancho", "Paso", "Silencio"];
    ui.vfo_labels = names.iter().map(|_| Default::default()).collect();
    for (i, name) in names.iter().enumerate() {
        nwg::Label::builder().text(name).position((12, 32 + i as i32 * ROW)).size((130, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.vfo_labels[i])?;
    }
    let lists: [Vec<String>; 5] = [vfo::DIRECTIONS, vfo::POWERS, vfo::WIDTHS, vfo::STEPS, vfo::MUTES].map(|l| l.iter().map(|x| x.to_string()).collect());
    for i in 0..vfo::VFO_COUNT {
        let x = 150 + i as i32 * 170;
        let at = |row: i32| (x, 30 + row * ROW);
        nwg::Label::builder().text(["Línea A", "Línea B"][i]).position((x + 4, 6)).size((150, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.vfo_heads[i])?;
        nwg::TextInput::builder().limit(9).position(at(0)).size((160, 26)).parent(t).build(&mut ui.vfo_freq[i])?;
        nwg::TextInput::builder().limit(6).position(at(1)).size((160, 26)).parent(t).build(&mut ui.vfo_rx_tone[i])?;
        nwg::TextInput::builder().limit(6).position(at(2)).size((160, 26)).parent(t).build(&mut ui.vfo_tx_tone[i])?;
        nwg::ComboBox::builder().collection(lists[0].clone()).position(at(3)).size((160, 26)).parent(t).build(&mut ui.vfo_direction[i])?;
        nwg::TextInput::builder().limit(9).position(at(4)).size((160, 26)).parent(t).build(&mut ui.vfo_offset[i])?;
        nwg::ComboBox::builder().collection(lists[1].clone()).position(at(5)).size((160, 26)).parent(t).build(&mut ui.vfo_power[i])?;
        nwg::ComboBox::builder().collection(lists[2].clone()).position(at(6)).size((160, 26)).parent(t).build(&mut ui.vfo_width[i])?;
        nwg::ComboBox::builder().collection(lists[3].clone()).position(at(7)).size((160, 26)).parent(t).build(&mut ui.vfo_step[i])?;
        nwg::ComboBox::builder().collection(lists[4].clone()).position(at(8)).size((160, 26)).parent(t).build(&mut ui.vfo_mute[i])?;
    }
    nwg::Button::builder().text("Leer del radio").position((510, 30)).size((180, 32)).parent(t).build(&mut ui.vfo_read)?;
    nwg::Button::builder().text("Guardar en radio").position((510, 70)).size((180, 32)).parent(t).build(&mut ui.vfo_save)?;
    nwg::Button::builder().text("Importar CSV...").position((700, 30)).size((180, 32)).parent(t).build(&mut ui.vfo_import)?;
    nwg::Button::builder().text("Exportar CSV...").position((700, 70)).size((180, 32)).parent(t).build(&mut ui.vfo_export)?;
    nwg::Label::builder().text("Canal:").position((510, 113)).size((50, 22)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.vfo_channel_label)?;
    nwg::ComboBox::builder().position((562, 110)).size((318, 26)).parent(t).build(&mut ui.vfo_channel)?;
    nwg::Button::builder().text("Pasar a línea A").position((510, 142)).size((180, 30)).parent(t).build(&mut ui.vfo_to_a)?;
    nwg::Button::builder().text("Pasar a línea B").position((700, 142)).size((180, 30)).parent(t).build(&mut ui.vfo_to_b)?;
    nwg::Label::builder()
        .text("Solo hay dos líneas. Para pasar un canal, léelos antes en la pestaña Canales.\r\n\r\nTonos: 88.5 (CTCSS), D023N o D023I (DCS). Vacío: sin tono.\r\n\r\nDirección y desplazamiento: para transmitir en otra frecuencia, como con un repetidor. Por ejemplo, - y 0.6.")
        .position((510, 178))
        .size((470, 136))
        .background_color(Some([255, 255, 255]))
        .parent(t)
        .build(&mut ui.vfo_hint)?;

    let t = &ui.tab_logo;
    nwg::ImageFrame::builder().position((12, 12)).size((256, 128)).parent(t).build(&mut ui.logo_frame)?;
    let x = 288;
    nwg::Button::builder().text("Leer del radio").position((x, 12)).size((180, 30)).parent(t).build(&mut ui.logo_read)?;
    nwg::Button::builder().text("Elegir imagen...").position((x, 48)).size((180, 30)).parent(t).build(&mut ui.logo_choose)?;
    nwg::Button::builder().text("Exportar imagen...").position((x, 84)).size((180, 30)).parent(t).build(&mut ui.logo_export)?;
    nwg::Button::builder().text("Guardar en radio").position((x + 190, 12)).size((180, 30)).parent(t).build(&mut ui.logo_save)?;
    nwg::CheckBox::builder().text("Invertir colores").position((x + 190, 50)).size((180, 24)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.logo_invert)?;
    nwg::CheckBox::builder().text("Tramado (para fotos)").position((x + 190, 86)).size((180, 24)).background_color(Some([255, 255, 255])).parent(t).build(&mut ui.logo_dither)?;
    nwg::Label::builder()
        .text("Se ve al encender si en Ajustes pones Inicio: pantalla en Logo.\r\n\
               La pantalla es de 128 x 64 puntos en blanco y negro: la imagen se adapta sin deformarse.\r\n\
               Admite PNG, JPG y BMP. Lo mejor es un dibujo simple de alto contraste.")
        .position((12, 156))
        .size((680, 66))
        .background_color(Some([255, 255, 255]))
        .parent(t)
        .build(&mut ui.logo_hint)?;

    nwg::TextBox::builder()
        .position((12, 420))
        .size((716, 160))
        .readonly(true)
        .flags(nwg::TextBoxFlags::VISIBLE | nwg::TextBoxFlags::VSCROLL | nwg::TextBoxFlags::AUTOVSCROLL | nwg::TextBoxFlags::TAB_STOP)
        .parent(w)
        .build(&mut ui.log)?;
    nwg::Label::builder().text("").position((12, 586)).size((716, 20)).parent(w).build(&mut ui.progress_label)?;
    nwg::ProgressBar::builder().position((12, 608)).size((716, 18)).range(0..1000).parent(w).build(&mut ui.progress)?;
    nwg::Label::builder().text("Listo para empezar.").position((12, 634)).size((716, 40)).parent(w).build(&mut ui.status)?;

    nwg::AnimationTimer::builder()
        .parent(w)
        .interval(Duration::from_millis(100))
        .active(true)
        .build(&mut ui.timer)?;

    nwg::FileDialog::builder()
        .title("Elegir firmware")
        .action(nwg::FileDialogAction::Open)
        .filters("Firmware (*.bin)|Todos (*.*)")
        .build(&mut ui.open_dialog)?;
    nwg::FileDialog::builder()
        .title("Importar canales")
        .action(nwg::FileDialogAction::Open)
        .filters("CSV (*.csv)|Todos (*.*)")
        .build(&mut ui.csv_open)?;
    nwg::FileDialog::builder()
        .title("Exportar canales")
        .action(nwg::FileDialogAction::Save)
        .filters("CSV (*.csv)")
        .build(&mut ui.csv_save)?;
    nwg::FileDialog::builder()
        .title("Importar ajustes")
        .action(nwg::FileDialogAction::Open)
        .filters("CSV (*.csv)|Todos (*.*)")
        .build(&mut ui.settings_open)?;
    nwg::FileDialog::builder()
        .title("Exportar ajustes")
        .action(nwg::FileDialogAction::Save)
        .filters("CSV (*.csv)")
        .build(&mut ui.settings_saveas)?;
    nwg::FileDialog::builder()
        .title("Elegir imagen para el logo")
        .action(nwg::FileDialogAction::Open)
        .filters("Imágenes (*.png;*.jpg;*.jpeg;*.bmp;*.gif)|Todos (*.*)")
        .build(&mut ui.logo_open)?;
    nwg::FileDialog::builder()
        .title("Importar VFO")
        .action(nwg::FileDialogAction::Open)
        .filters("CSV (*.csv)|Todos (*.*)")
        .build(&mut ui.vfo_open)?;
    nwg::FileDialog::builder()
        .title("Elegir copia para restaurar")
        .action(nwg::FileDialogAction::Open)
        .filters("Copias (*.bin)|Todos (*.*)")
        .build(&mut ui.restore_open)?;
    nwg::FileDialog::builder()
        .title("Aplicar perfil")
        .action(nwg::FileDialogAction::Open)
        .filters("Perfiles (*.txt)|Todos (*.*)")
        .build(&mut ui.profile_open)?;
    nwg::FileDialog::builder()
        .title("Guardar perfil")
        .action(nwg::FileDialogAction::Save)
        .filters("Perfiles (*.txt)")
        .build(&mut ui.profile_saveas)?;
    nwg::FileDialog::builder()
        .title("Exportar VFO")
        .action(nwg::FileDialogAction::Save)
        .filters("CSV (*.csv)")
        .build(&mut ui.vfo_saveas)?;
    nwg::FileDialog::builder()
        .title("Exportar logo")
        .action(nwg::FileDialogAction::Save)
        .filters("BMP (*.bmp)")
        .build(&mut ui.logo_saveas)?;
    Ok(())
}

pub fn run() {
    if nwg::init().is_err() {
        return;
    }
    let _ = nwg::Font::set_global_family("Segoe UI");

    let mut ui = Ui::default();
    if let Err(e) = build(&mut ui) {
        nwg::simple_message(TITLE, &format!("No se pudo crear la ventana: {e}"));
        return;
    }
    let ui = Rc::new(ui);
    let pages = [ui.tab_radio.handle, ui.tab_channels.handle, ui.tab_settings.handle, ui.tab_vfo.handle, ui.tab_logo.handle];
    theme::install(&ui.window, &ui.tabs, &pages);
    theme::apply(&ui.window, theme::saved_dark(), false);
    ui.layout();
    ui.refresh_ports();
    ui.message(MODEL_NOTICE, nwg::MessageIcons::Warning);

    let handler_ui = Rc::clone(&ui);
    let window = ui.window.handle;
    let handler = nwg::full_bind_event_handler(&window, move |event, data, handle| {
        let ui = &handler_ui;
        match event {
            nwg::Event::OnWindowClose if handle == ui.window.handle => {
                if ui.busy() {
                    if let nwg::EventData::OnWindowClose(close) = data {
                        close.close(false);
                    }
                    ui.status.set_text("No cierres el programa: está trabajando con el radio.");
                } else {
                    nwg::stop_thread_dispatch();
                }
            }
            nwg::Event::OnTimerTick if handle == ui.timer.handle => ui.poll(),
            // Maximizing arrives as its own event, not as a resize.
            nwg::Event::OnResize | nwg::Event::OnWindowMaximize if handle == ui.window.handle => ui.layout(),
            nwg::Event::OnMinMaxInfo if handle == ui.window.handle => {
                if let nwg::EventData::OnMinMaxInfo(info) = data {
                    info.set_min_size(MIN_OUTER_WIDTH, MIN_OUTER_HEIGHT);
                }
            }
            nwg::Event::OnListViewItemChanged if handle == ui.list.handle => ui.on_select(),
            nwg::Event::OnComboxBoxSelection if handle == ui.port.handle => ui.update_enabled(),
            nwg::Event::OnComboBoxDropdown => ui.show_setting_help(handle),
            nwg::Event::OnButtonClick => {
                if handle == ui.refresh.handle {
                    ui.refresh_ports();
                } else if handle == ui.dark.handle {
                    theme::apply(&ui.window, ui.dark.check_state() == nwg::CheckBoxState::Checked, true);
                } else if handle == ui.backup.handle {
                    ui.start(Action::Backup);
                } else if handle == ui.fw_browse.handle {
                    ui.browse_firmware();
                } else if handle == ui.install.handle {
                    ui.on_install();
                } else if handle == ui.language.handle {
                    ui.on_language();
                } else if handle == ui.read.handle {
                    ui.start(Action::ReadChannels);
                } else if handle == ui.save.handle {
                    ui.on_save();
                } else if handle == ui.apply.handle {
                    ui.on_apply();
                } else if handle == ui.delete.handle {
                    ui.on_delete();
                } else if handle == ui.import.handle {
                    ui.on_import();
                } else if handle == ui.export.handle {
                    ui.on_export();
                } else if handle == ui.settings_read.handle {
                    ui.start(Action::ReadSettings);
                } else if handle == ui.settings_save.handle {
                    ui.on_save_settings();
                } else if handle == ui.settings_import.handle {
                    ui.on_import_settings();
                } else if handle == ui.settings_export.handle {
                    ui.on_export_settings();
                } else if handle == ui.settings_help.handle {
                    ui.on_settings_guide();
                } else if handle == ui.restore.handle {
                    ui.on_restore();
                } else if handle == ui.profile_export.handle {
                    ui.on_export_profile();
                } else if handle == ui.profile_import.handle {
                    ui.on_import_profile();
                } else if handle == ui.vfo_read.handle {
                    ui.start(Action::ReadVfo);
                } else if handle == ui.vfo_save.handle {
                    ui.on_save_vfos();
                } else if handle == ui.vfo_to_a.handle {
                    ui.on_channel_to_vfo(0);
                } else if handle == ui.vfo_to_b.handle {
                    ui.on_channel_to_vfo(1);
                } else if handle == ui.vfo_import.handle {
                    ui.on_import_vfos();
                } else if handle == ui.vfo_export.handle {
                    ui.on_export_vfos();
                } else if handle == ui.logo_read.handle {
                    ui.start(Action::ReadLogo);
                } else if handle == ui.logo_choose.handle {
                    ui.on_choose_logo();
                } else if handle == ui.logo_export.handle {
                    ui.on_export_logo();
                } else if handle == ui.logo_save.handle {
                    ui.on_save_logo();
                } else if handle == ui.logo_invert.handle || handle == ui.logo_dither.handle {
                    ui.render_logo();
                }
            }
            _ => {}
        }
    });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
}
