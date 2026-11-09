//! The program's window, with native Windows controls. All radio work runs on a worker
//! thread and reports through `crate::Msg`; a timer in the window collects those messages.

use crate::channels::{self, Channel};
use crate::{list_ports, run_action, set_sink, Action, Language, Msg};
use native_windows_gui as nwg;
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

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
    apply: nwg::Button,
    delete: nwg::Button,
    read: nwg::Button,
    save: nwg::Button,
    import: nwg::Button,
    export: nwg::Button,

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
        if let Some(rx) = self.rx_msgs.borrow().as_ref() {
            for m in rx.try_iter() {
                match m {
                    Msg::Log(line) => self.log.appendln(&line),
                    Msg::Progress(label, value) => {
                        self.progress.set_pos((value.clamp(0.0, 1.0) * 1000.0) as u32);
                        self.progress_label.set_text(&format!("{label}: {:.0}%", value * 100.0));
                    }
                    Msg::ChannelsRead(area) => read = Some(area),
                    Msg::Finished(r) => finished = Some(r),
                }
            }
        }
        if let Some(area) = read {
            *self.channels.borrow_mut() = channels::parse_channels(&area);
            *self.area.borrow_mut() = Some(area);
            self.fill_list();
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

    fn fill_list(&self) {
        self.list.clear();
        for c in self.channels.borrow().iter() {
            let row = [
                c.number.to_string(),
                channels::format_mhz(c.rx),
                channels::format_mhz(c.tx),
                c.name.clone(),
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
        Ok(Channel { number, rx, tx, name })
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
             Los tonos, la potencia y los demás ajustes de cada canal se conservan.\n\
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
        let result = fs::read_to_string(&file)
            .map_err(|e| format!("No se pudo leer el archivo: {e}"))
            .and_then(|text| channels::from_csv(&text));
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

fn build(ui: &mut Ui) -> Result<(), nwg::NwgError> {
    nwg::Window::builder()
        .size((740, 690))
        .center(true)
        .title(TITLE)
        .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::VISIBLE)
        .build(&mut ui.window)?;
    let w = &ui.window;

    nwg::Label::builder().text(MODEL_SHORT).position((16, 8)).size((708, 22)).parent(w).build(&mut ui.model_label)?;
    nwg::Label::builder().text("Puerto COM:").position((16, 40)).size((90, 22)).parent(w).build(&mut ui.port_label)?;
    nwg::ComboBox::builder().position((110, 36)).size((220, 26)).parent(w).build(&mut ui.port)?;
    nwg::Button::builder().text("Actualizar lista").position((340, 35)).size((130, 28)).parent(w).build(&mut ui.refresh)?;

    nwg::TabsContainer::builder().position((12, 72)).size((716, 340)).parent(w).build(&mut ui.tabs)?;
    nwg::Tab::builder().text("Radio").parent(&ui.tabs).build(&mut ui.tab_radio)?;
    nwg::Tab::builder().text("Canales").parent(&ui.tabs).build(&mut ui.tab_channels)?;

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
        .text("Idioma de los textos: en el radio, MENU > Language > Español.")
        .position((12, 258))
        .size((680, 22))
        .background_color(Some([255, 255, 255])).parent(t)
        .build(&mut ui.text_label)?;

    let t = &ui.tab_channels;
    nwg::ListView::builder()
        .position((10, 10))
        .size((440, 250))
        .list_style(nwg::ListViewStyle::Detailed)
        .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT | nwg::ListViewExFlags::GRID)
        .parent(t)
        .build(&mut ui.list)?;
    for (i, (text, width)) in [("Canal", 60), ("RX (MHz)", 110), ("TX (MHz)", 110), ("Nombre", 130)].iter().enumerate() {
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
    nwg::Button::builder().text("Aplicar").position((x, 158)).size((110, 32)).parent(t).build(&mut ui.apply)?;
    nwg::Button::builder().text("Borrar canal").position((x + 114, 158)).size((110, 32)).parent(t).build(&mut ui.delete)?;

    nwg::Button::builder().text("Leer del radio").position((10, 270)).size((150, 32)).parent(t).build(&mut ui.read)?;
    nwg::Button::builder().text("Guardar en radio").position((170, 270)).size((150, 32)).parent(t).build(&mut ui.save)?;
    nwg::Button::builder().text("Importar CSV...").position((400, 270)).size((140, 32)).parent(t).build(&mut ui.import)?;
    nwg::Button::builder().text("Exportar CSV...").position((550, 270)).size((140, 32)).parent(t).build(&mut ui.export)?;

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
            nwg::Event::OnListViewItemChanged if handle == ui.list.handle => ui.on_select(),
            nwg::Event::OnComboxBoxSelection if handle == ui.port.handle => ui.update_enabled(),
            nwg::Event::OnButtonClick => {
                if handle == ui.refresh.handle {
                    ui.refresh_ports();
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
                }
            }
            _ => {}
        }
    });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
}
