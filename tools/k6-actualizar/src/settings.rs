//! Radio settings, edited through the firmware's own programming mode.
//!
//! Settings live in a rotating log with a checksum at 0x9000, so this module never touches
//! that flash directly. In programming mode the addresses 0x9000 and 0x9040 stand for the
//! settings in RAM and the power-on text, and writing 0x9040 makes the radio save both the way
//! its own menu does: see ProgromWriteReadData in src/CPS/ProgromFlash.c. Every other address
//! in that mode goes straight to flash, so nothing else may ever be sent from here.

use crate::{emit, ensure_backup_dir, open_link, pause, read_exact, utc_stamp, Link, Msg, Res, MS_REPLY, MS_WAIT_BOOT};
use std::fs;
use std::path::{Path, PathBuf};

const BLOCK_SIZE: usize = 64;
pub const SETTINGS_SIZE: usize = 2 * BLOCK_SIZE;
const ADDR_SETTINGS: u16 = 0x9000; // STR_RADIOINFORM in src/Common/PublType.h.
const ADDR_POWER_TEXT: u16 = 0x9040; // Writing this one is what saves everything.
const BAND_REPLY_SIZE: usize = 16;
const ACK: u8 = 0x06;
const MS_BETWEEN: u64 = if cfg!(test) { 0 } else { 50 };

pub struct Field {
    pub label: &'static str,
    /// The radio's menu entry and the settings member it edits. A test checks them against the firmware.
    #[allow(dead_code)]
    pub menu: &'static str,
    pub member: &'static str,
    pub offset: usize,
    pub mask: u8,
    pub shift: u8,
    /// One text per value, starting at 0, in the order of the radio's menu.
    pub options: &'static [&'static str],
}

const fn byte(label: &'static str, menu: &'static str, member: &'static str, offset: usize, options: &'static [&'static str]) -> Field {
    Field { label, menu, member, offset, mask: 0xFF, shift: 0, options }
}

const ON_OFF: &[&str] = &["No", "Sí"];
const DISPLAY: &[&str] = &["Nombre", "Frecuencia", "Canal", "Nombre y frec."];
const WORK_MODE: &[&str] = &["Frecuencia", "Canal"];
const SIDE_KEY: &[&str] = &["Nada", "Linterna", "Potencia", "Escaneo", "VOX", "Alarma", "Radio FM"];

// Excluded on purpose:
// - Power-on password: handled apart below, because it must never travel in a settings file.
// - Stored but never read by this firmware: rpste, dualTxMode, scanCtcsMode, menuSet, signalType.
// - Bandwidth, power, shift, step, PTT ID, busy lock: they belong to each channel or VFO.
// - Values the radio's menu doesn't offer, even when the firmware has text for them.
// - TX enables at RF_TXEN_ADDR and factory modes: this program doesn't widen where the radio transmits.
pub const FIELDS: &[Field] = &[
    byte("Silenciador", "S_SQL", "sqlLevel", 0, &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]),
    byte("Ahorro batería", "S_SAVE", "saveLevel", 1, &["No", "1:1", "1:2", "1:4"]),
    byte("Doble escucha", "S_TDR", "dualRxFlag", 4, ON_OFF),
    byte("Modo escaneo", "S_SCREV", "scanMode", 10, &["Tiempo", "Portadora", "Búsqueda"]),
    byte("Límite de TX", "S_TOT", "totLevel", 5, &[
        "No", "15 s", "30 s", "45 s", "60 s", "75 s", "90 s", "105 s", "120 s", "135 s", "150 s", "165 s", "180 s",
    ]),
    byte("Prohibir TX", "S_TXFORBID", "txForbid", 49, ON_OFF),
    byte("VOX", "S_VOX", "voxSwitch", 58, ON_OFF),
    byte("Nivel VOX", "S_VOXLV", "voxLevel", 2, &["1", "2"]),
    byte("Retardo VOX", "S_VOXDLY", "voxDelay", 32, &[
        "0.5 s", "0.6 s", "0.7 s", "0.8 s", "0.9 s", "1.0 s", "1.1 s", "1.2 s", "1.3 s", "1.4 s", "1.5 s", "1.6 s",
        "1.7 s", "1.8 s", "1.9 s", "2.0 s",
    ]),
    byte("Quitar cola TX", "S_TAIL", "tailSwitch", 20, ON_OFF),
    byte("Tono fin de TX", "S_ROGE", "txOffTone", 23, &["No", "Pitido"]),
    byte("Idioma", "S_LAN", "language", 8, &["English", "中文", "Español"]),
    byte("Voz guía", "S_VOIC", "voiceSw", 7, ON_OFF),
    byte("Pitido teclas", "S_BEEP", "beepsSwitch", 6, ON_OFF),
    byte("Pantalla A", "S_MDF1", "channleDisA", 13, DISPLAY),
    byte("Pantalla B", "S_MDF2", "channleDisB", 14, DISPLAY),
    byte("Apagar luz", "S_ABR", "autoBack", 3, &["No", "5 s", "10 s", "15 s", "20 s"]),
    byte("Contraste", "S_BRIGHT", "brightness", 41, &["1", "2", "3", "4", "5"]),
    byte("Invertir LCD", "S_REFLEX", "DisplayStyles", 42, ON_OFF),
    byte("Autobloqueo", "S_AUTOLK", "keyAutoLock", 16, &["No", "5 s", "10 s", "15 s"]),
    byte("Salir menú", "S_MENUEXIT", "menuExitTime", 33, &[
        "5 s", "10 s", "15 s", "20 s", "25 s", "30 s", "35 s", "40 s", "45 s", "50 s", "60 s",
    ]),
    Field { label: "Inicio: pantalla", menu: "S_PONTYPE", member: "OpFlag1.Bit.b0", offset: 28, mask: 0x03, shift: 0, options: &["Logo", "Mensaje"] },
    Field { label: "Inicio: tono", menu: "S_PONTONE", member: "OpFlag1.Bit.b2", offset: 28, mask: 0x03, shift: 2, options: &["No", "Tono"] },
    byte("Señal corta FM", "S_FMINT", "fmInterrupt", 48, ON_OFF),
    byte("Tono DTMF", "S_DTST", "dtmfTone", 9, &["No", "Teclas", "ID", "Teclas e ID"]),
    byte("Retardo PTT ID", "S_PTTLT", "pttIdTime", 12, &["1", "2", "3", "4", "5", "6", "7"]),
    byte("Tono repetidor", "S_RTONE", "rtone", 29, &["1000 Hz", "1450 Hz", "1750 Hz", "2100 Hz"]),
    byte("Cola repetidor", "S_RPTRL", "rptrl", 22, &["No", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]),
    byte("Modo alarma", "S_ALMOD", "alarmMode", 17, &["Local", "Envía sonido"]),
    byte("PF1 corto", "S_SK1", "userSideKey[0]", 50, SIDE_KEY),
    byte("PF1 largo", "S_SKL1", "userSideKey[1]", 51, SIDE_KEY),
    byte("PF2 corto", "S_SK2", "userSideKey[2]", 52, SIDE_KEY),
    // Not in the radio's menu, but the firmware does use them. `menu` stays empty.
    Field { label: "Modo A", menu: "", member: "chOrVfoMode.Bit.chVofA", offset: 26, mask: 0x0F, shift: 0, options: WORK_MODE },
    Field { label: "Modo B", menu: "", member: "chOrVfoMode.Bit.chVofB", offset: 26, mask: 0x0F, shift: 4, options: WORK_MODE },
    byte("Teclado bloq.", "", "keyLock", 27, ON_OFF),
    byte("Desactivar FM", "", "fmEnale", 25, ON_OFF),
    byte("Alarma local", "", "alarmLocal", 18, ON_OFF),
    byte("Aviso fin de TX", "", "toa", 40, &["No", "1 s", "2 s", "3 s", "4 s", "5 s", "6 s", "7 s", "8 s", "9 s", "10 s"]),
];

/// What a setting does on the radio, as read from the firmware that uses it.
pub fn help(f: &Field) -> &'static str {
    match f.member {
        "sqlLevel" => "Qué tan fuerte debe ser una señal para oírla. Más alto corta más ruido, pero también señales débiles. Con 0 el altavoz queda siempre abierto.",
        "saveLevel" => "Apaga el receptor a ratos en espera para gastar menos batería, y puede perder el inicio de una llamada. Este firmware solo distingue No de los demás: 1:1, 1:2 y 1:4 hacen lo mismo.",
        "dualRxFlag" => "Vigila a la vez las frecuencias A y B, alternando entre ellas.",
        "scanMode" => "Qué hace el escaneo al encontrar una señal. Tiempo: se queda un rato y sigue. Portadora: se queda hasta que la señal termina. Búsqueda: se detiene ahí.",
        "totLevel" => "Corta la transmisión si mantienes PTT más de este tiempo. Protege el radio de calentarse y evita bloquear un canal por accidente.",
        "txForbid" => "Con Sí, el radio no transmite aunque pulses PTT. Solo recibe.",
        "voxSwitch" => "Transmite solo al detectar tu voz, sin pulsar PTT. Puede transmitir por ruido del ambiente.",
        "voxLevel" => "Qué tan fuerte debes hablar para activar el VOX. Con 2 hace falta hablar más fuerte que con 1.",
        "voxDelay" => "Cuánto sigue transmitiendo el VOX después de que dejas de hablar, para no cortar tus pausas.",
        "tailSwitch" => "Envía una señal al soltar PTT para que los otros radios cierren sin el ruido final. Alarga un poco el final de cada transmisión.",
        "txOffTone" => "Envía un pitido al soltar PTT para avisar a los demás que terminaste.",
        "language" => "Idioma de los textos en pantalla.",
        "voiceSw" => "Una voz anuncia lo que haces, como menús y números. La voz en español se instala en la pestaña Radio.",
        "beepsSwitch" => "Pitido al pulsar teclas.",
        "channleDisA" => "Qué muestra la línea A en modo canal: nombre, frecuencia, número de canal o nombre con frecuencia.",
        "channleDisB" => "Qué muestra la línea B en modo canal: nombre, frecuencia, número de canal o nombre con frecuencia.",
        "autoBack" => "Tiempo hasta apagar la luz de pantalla después de la última tecla.",
        "brightness" => "Contraste de la pantalla, de 1 a 5. No cambia la intensidad de la luz.",
        "DisplayStyles" => "Invierte la pantalla: fondo oscuro y letras claras.",
        "keyAutoLock" => "Bloquea el teclado solo tras este tiempo sin usarlo, para evitar pulsaciones por accidente.",
        "menuExitTime" => "Tiempo sin tocar nada hasta que el menú se cierra solo.",
        "OpFlag1.Bit.b0" => "Qué se ve al encender: el logo (se cambia en la pestaña Logo) o el mensaje de la casilla Mensaje inicio.",
        "OpFlag1.Bit.b2" => "Sonido al encender el radio.",
        "fmInterrupt" => "Con Sí, una señal recibida en tu canal corta la radio FM para que la oigas. Con No, la radio FM sigue sonando y esa señal no se oye.",
        "dtmfTone" => "Si oyes en tu propio radio los tonos DTMF que envías: los de las teclas, los del identificador, o ambos.",
        "pttIdTime" => "Espera antes de enviar el identificador PTT ID. Más alto espera más. Solo importa si usas PTT ID.",
        "rtone" => "Tono para abrir repetidores que lo piden. Se envía con PF2 mientras transmites.",
        "rptrl" => "Espera extra al terminar de transmitir o recibir, para no cortar la cola de un repetidor. Más alto espera más. Con No no espera.",
        "alarmMode" => "Qué hace la alarma. Local: suena solo en tu radio. Envía sonido: además transmite el sonido.",
        "chOrVfoMode.Bit.chVofA" => "Con qué arranca la línea A: una frecuencia libre (VFO) o los canales guardados.",
        "chOrVfoMode.Bit.chVofB" => "Con qué arranca la línea B: una frecuencia libre (VFO) o los canales guardados.",
        "keyLock" => "Deja el teclado bloqueado, igual que al bloquearlo desde el radio.",
        "fmEnale" => "Con Sí, la radio FM no se puede abrir.",
        "alarmLocal" => "Con Sí, tu radio también suena cuando la alarma transmite. Con No, transmite sin sonar en tu radio.",
        "toa" => "Avisa este tiempo antes de que el Límite de TX corte la transmisión. Con No, corta sin avisar.",
        "userSideKey[0]" => "Función del botón lateral PF1 al pulsarlo corto.",
        "userSideKey[1]" => "Función del botón lateral PF1 al mantenerlo pulsado.",
        "userSideKey[2]" => "Función del botón lateral PF2 al pulsarlo corto.",
        _ => "",
    }
}

pub const MESSAGE_HELP: &str = "Texto del encendido, de hasta 16 letras sin tildes. Se ve si Inicio: pantalla está en Mensaje.";
const MESSAGE_KEY: &str = "powerOnMsg";
const MESSAGE_AT: usize = BLOCK_SIZE; // powerOnMsg: first bytes of the second block.
const MESSAGE_SIZE: usize = 16;

/// The power-on text, cut where the radio cuts it in UI_DisplayPowerOn.
pub fn message(data: &[u8]) -> String {
    data[MESSAGE_AT..MESSAGE_AT + MESSAGE_SIZE]
        .iter()
        .take_while(|&&b| b != 0xFF && b != 0)
        .map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '?' })
        .collect()
}

/// Commas and semicolons are refused because they separate the columns of the settings file.
pub fn check_message(text: &str) -> Res<()> {
    let printable = text.bytes().all(|b| (0x20..=0x7E).contains(&b) && b != b',' && b != b';');
    if text.len() > MESSAGE_SIZE || !printable {
        return Err(format!("Mensaje no válido: \"{text}\". Máximo 16 letras, sin tildes, ñ, comas ni punto y coma."));
    }
    Ok(())
}

pub const PASSWORD_HELP: &str = "Con Sí, el radio pide esta clave de 6 números al encender, y no se conecta al PC hasta recibirla. Anótala.";
const PASSWORD_FLAG_AT: usize = 60; // pwrPwdFlag.
const PASSWORD_AT: usize = 61; // pwrPassword: 6 digits in 3 bytes, see CheckInputPassWord.

/// Whether the radio asks for a password, as App_CheckPowerOnPassword decides it.
pub fn password_on(data: &[u8]) -> bool {
    !matches!(data[PASSWORD_FLAG_AT], 0 | 0xFF)
}

/// The password as digits, or empty if the stored bytes aren't digits.
pub fn password(data: &[u8]) -> String {
    let bytes = &data[PASSWORD_AT..PASSWORD_AT + 3];
    if bytes.iter().any(|b| b >> 4 > 9 || b & 15 > 9) {
        return String::new();
    }
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Applies the password controls over `data`. Nothing changes unless they differ from the radio.
pub fn apply_password(data: &[u8], on: bool, text: &str) -> Res<Vec<u8>> {
    let mut out = data.to_vec();
    let text = text.trim();
    let valid = text.len() == 6 && text.bytes().all(|b| b.is_ascii_digit());
    if on && !valid {
        return Err("Para pedir clave al encender, escribe una clave de exactamente 6 números.".into());
    }
    if valid && text != password(data) {
        let d = text.as_bytes();
        for i in 0..3 {
            out[PASSWORD_AT + i] = (d[i * 2] - b'0') << 4 | (d[i * 2 + 1] - b'0');
        }
    } else if !valid && !text.is_empty() && text != password(data) {
        return Err("La clave debe tener exactamente 6 números.".into());
    }
    if on != password_on(data) {
        out[PASSWORD_FLAG_AT] = on as u8;
    }
    Ok(out)
}

/// The option shown for a field, or None if the radio holds a value its menu doesn't offer.
pub fn selected(data: &[u8], f: &Field) -> Option<usize> {
    let value = ((data[f.offset] >> f.shift) & f.mask) as usize;
    (value < f.options.len()).then_some(value)
}

/// Applies the choices over what was read. `None` leaves that value as it was, and every byte
/// not in `FIELDS` stays untouched, which includes the password. The power-on text only changes
/// if `text` differs from what the radio holds.
pub fn apply(loaded: &[u8], choices: &[Option<usize>], text: &str) -> Res<Vec<u8>> {
    if loaded.len() != SETTINGS_SIZE || choices.len() != FIELDS.len() {
        return Err("Datos de ajustes de tamaño inesperado".into());
    }
    let mut out = loaded.to_vec();
    for (f, choice) in FIELDS.iter().zip(choices) {
        let Some(value) = *choice else { continue };
        if value >= f.options.len() {
            return Err(format!("{}: valor no válido", f.label));
        }
        out[f.offset] = (out[f.offset] & !(f.mask << f.shift)) | ((value as u8) << f.shift);
    }
    if text != message(loaded) {
        check_message(text)?;
        // Filled with zeros, as the radio's own menu does.
        out[MESSAGE_AT..MESSAGE_AT + MESSAGE_SIZE].fill(0);
        out[MESSAGE_AT..MESSAGE_AT + text.len()].copy_from_slice(text.as_bytes());
    }
    Ok(out)
}

/// Writes the choices to a file. The member name identifies each setting and the number is the
/// value; the other two columns are only there to be read. An empty value means "leave as is".
pub fn to_csv(choices: &[Option<usize>], text: &str) -> String {
    let mut out = String::from("\u{feff}clave,nombre,valor,texto\r\n");
    for (f, choice) in FIELDS.iter().zip(choices) {
        let value = choice.map_or(String::new(), |v| v.to_string());
        let shown = choice.and_then(|v| f.options.get(v)).copied().unwrap_or("");
        out.push_str(&format!("{},{},{},{}\r\n", f.member, f.label, value, shown));
    }
    // A text that could break the columns is left out, so importing keeps what the radio has.
    let safe = if check_message(text).is_ok() { text } else { "" };
    out.push_str(&format!("{MESSAGE_KEY},Mensaje inicio,{safe},\r\n"));
    out
}

/// Reads a file from `to_csv`, all or nothing. Settings missing from it stay as `None`, which
/// `apply` leaves alone, and so does an empty or missing message. Spreadsheets may use ';' as the
/// separator.
pub fn from_csv(text: &str) -> Res<(Vec<Option<usize>>, Option<String>)> {
    let mut out = vec![None; FIELDS.len()];
    let mut text_found: Option<String> = None;
    let mut message_seen = false;
    let mut seen = vec![false; FIELDS.len()];
    let mut found = 0;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || (i == 0 && line.to_lowercase().starts_with("clave")) {
            continue;
        }
        let row = i + 1;
        let sep = if line.contains(';') { ';' } else { ',' };
        let cols: Vec<&str> = line.split(sep).map(str::trim).collect();
        if cols.len() < 3 {
            return Err(format!("Fila {row}: se esperaban clave, nombre y valor"));
        }
        if cols[0] == MESSAGE_KEY {
            if message_seen {
                return Err(format!("Fila {row}: \"{MESSAGE_KEY}\" está repetido"));
            }
            message_seen = true;
            found += 1;
            if !cols[2].is_empty() {
                check_message(cols[2]).map_err(|e| format!("Fila {row}: {e}"))?;
                text_found = Some(cols[2].to_string());
            }
            continue;
        }
        let at = FIELDS
            .iter()
            .position(|f| f.member == cols[0])
            .ok_or_else(|| format!("Fila {row}: ajuste desconocido \"{}\"", cols[0]))?;
        if seen[at] {
            return Err(format!("Fila {row}: \"{}\" está repetido", cols[0]));
        }
        seen[at] = true;
        found += 1;
        let f = &FIELDS[at];
        if cols[2].is_empty() {
            continue;
        }
        let value: usize = cols[2]
            .parse()
            .ok()
            .filter(|&v| v < f.options.len())
            .ok_or_else(|| format!("Fila {row}: valor no válido para {}: \"{}\"", f.label, cols[2]))?;
        out[at] = Some(value);
    }
    if found == 0 {
        return Err("El archivo no contiene ajustes.".into());
    }
    Ok((out, text_found))
}

fn enter(link: &mut dyn Link) -> Res<()> {
    open_link(link, MS_WAIT_BOOT)?;
    pause(MS_BETWEEN);
    link.clear_input();
    link.send(b"F")?; // Moves the radio on to the read and write commands.
    read_exact(link, BAND_REPLY_SIZE, MS_REPLY)?;
    pause(MS_BETWEEN);
    Ok(())
}

/// Ends the session, which restarts the radio.
fn leave(link: &mut dyn Link) {
    let _ = link.send(b"E");
    let _ = read_exact(link, 1, MS_REPLY);
}

fn read_block(link: &mut dyn Link, addr: u16) -> Res<Vec<u8>> {
    let [hi, lo] = addr.to_be_bytes();
    link.send(&[b'R', hi, lo, BLOCK_SIZE as u8])?;
    let reply = read_exact(link, 4 + BLOCK_SIZE, MS_REPLY)?;
    if reply[..4] != [b'R', hi, lo, BLOCK_SIZE as u8] {
        return Err("Respuesta inesperada del radio al leer los ajustes".into());
    }
    pause(MS_BETWEEN);
    Ok(reply[4..].to_vec())
}

fn write_block(link: &mut dyn Link, addr: u16, data: &[u8]) -> Res<()> {
    // Other addresses are raw flash in this mode, and a sector start would even be erased.
    if (addr != ADDR_SETTINGS && addr != ADDR_POWER_TEXT) || data.len() != BLOCK_SIZE {
        return Err(format!("Escritura rechazada en 0x{addr:04X}: no es una zona de ajustes"));
    }
    let [hi, lo] = addr.to_be_bytes();
    let mut packet = vec![b'W', hi, lo, BLOCK_SIZE as u8];
    packet.extend_from_slice(data);
    link.send(&packet)?;
    if read_exact(link, 1, MS_REPLY)?[0] != ACK {
        return Err("El radio no confirmó la escritura de los ajustes".into());
    }
    pause(MS_BETWEEN);
    Ok(())
}

fn read_both(link: &mut dyn Link) -> Res<Vec<u8>> {
    let mut data = read_block(link, ADDR_SETTINGS)?;
    data.extend(read_block(link, ADDR_POWER_TEXT)?);
    Ok(data)
}

pub fn read_from_radio(link: &mut dyn Link) -> Res<Vec<u8>> {
    say!("Leyendo ajustes...");
    enter(link)?;
    let result = read_both(link);
    leave(link);
    result
}

/// Keeps a copy of the current settings, writes the new ones, then restarts the radio and reads
/// them again: the radio loads them from flash at startup, so this checks what was really saved.
pub fn write_to_radio(link: &mut dyn Link, loaded: &[u8], wanted: &[u8], dir: &Path) -> Res<(Vec<u8>, PathBuf)> {
    if loaded.len() != SETTINGS_SIZE || wanted.len() != SETTINGS_SIZE {
        return Err("Datos de ajustes de tamaño inesperado".into());
    }
    say!("Comprobando el radio...");
    enter(link)?;
    let written = (|| {
        let current = read_both(link)?;
        if current != loaded {
            return Err("Los ajustes del radio cambiaron desde que los leíste. Léelos de nuevo.".to_string());
        }
        ensure_backup_dir(dir)?;
        let path = dir.join(format!("Baofeng_K6_ajustes_{}.bin", utc_stamp()));
        fs::write(&path, &current).map_err(|e| format!("No se pudo guardar la copia: {e}"))?;
        say!("Copia de los ajustes: {}", path.display());
        say!("Guardando ajustes...");
        write_block(link, ADDR_SETTINGS, &wanted[..BLOCK_SIZE])?;
        write_block(link, ADDR_POWER_TEXT, &wanted[BLOCK_SIZE..])?;
        Ok(path)
    })();
    leave(link);
    let path = written?;

    say!("Reiniciando el radio para comprobar lo guardado...");
    enter(link)?;
    let check = read_both(link);
    leave(link);
    let check = check?;
    if check != wanted {
        return Err(format!(
            "El radio no conservó los ajustes al reiniciar. La copia anterior está en {}",
            path.display()
        ));
    }
    Ok((check, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// The settings record of a real radio, plus its power-on text block.
    fn real() -> Vec<u8> {
        let mut d = vec![
            0x03, 0x01, 0x00, 0x03, 0x01, 0x08, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x00, 0x01, 0x05, 0x05, 0x01, 0x01, 0x00, 0x11, 0x00, 0x00, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x90, 0x01, 0xD6, 0x01, 0x00, 0x00, 0x01, 0x05, 0x06, 0x00, 0x00, 0x01, 0xFF,
            0x00, 0x00, 0xFF, 0x00, 0x52, 0x80, 0x27,
        ];
        d.extend(vec![0xFF; BLOCK_SIZE]);
        d
    }

    fn lossy(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// Offset of each member of STR_RADIOINFORM, worked out from the firmware's header.
    fn struct_offsets() -> Vec<(String, usize, usize)> {
        let src = lossy(include_bytes!("../../../src/Common/PublType.h"));
        let end = src.find("}STR_RADIOINFORM").unwrap();
        let start = src[..end].rfind("typedef struct").unwrap();
        let mut at = 0;
        let mut out = Vec::new();
        for line in src[start..end].lines().skip(1) {
            let code = line.split("//").next().unwrap().trim();
            let Some((kind, rest)) = code.split_once(char::is_whitespace) else { continue };
            let width = match kind {
                "U8" | "UNION_VM" | "UNION_FLAG2" => 1,
                "U16" => 2,
                _ => continue,
            };
            let rest = rest.trim().trim_end_matches(';').trim();
            let (name, count) = match rest.split_once('[') {
                Some((n, c)) => (n.trim(), c.trim_end_matches(']').parse::<usize>().unwrap()),
                None => (rest, 1),
            };
            out.push((name.to_string(), at, width * count));
            at += width * count;
        }
        assert_eq!(at, BLOCK_SIZE, "The radio's settings no longer take one block");
        out
    }

    #[test]
    fn every_option_matches_the_firmware_menu() {
        let menu = lossy(include_bytes!("../../../src/App/AppMenu.c"));
        let compact_menu: String = menu.split_whitespace().collect();
        let offsets = struct_offsets();
        for f in FIELDS {
            // That the member really is at this offset, for every setting.
            let base = f.member.split(['.', '[']).next().unwrap();
            let (_, at, size) = offsets.iter().find(|(n, _, _)| n == base).unwrap_or_else(|| panic!("{base} not in struct"));
            let index = f.member.split_once('[').map_or(0, |(_, i)| i.trim_end_matches(']').parse().unwrap());
            assert!(index < *size, "{}", f.label);
            assert_eq!(f.offset, at + index, "{}: offset", f.label);

            // Settings outside the menu have no menu to match, but the firmware must read them.
            if f.menu.is_empty() {
                let reads = [
                    lossy(include_bytes!("../../../src/App/AppFm.c")),
                    lossy(include_bytes!("../../../src/App/AppAlarm.c")),
                    lossy(include_bytes!("../../../src/Core/Radio.c")),
                    lossy(include_bytes!("../../../src/Core/RadioTask.c")),
                    lossy(include_bytes!("../../../src/Core/Functions.c")),
                ];
                let wanted = format!("g_radioInform.{}", f.member);
                assert!(reads.iter().any(|r| r.contains(&wanted)), "{}: not read by the firmware", f.label);
                continue;
            }

            // How many values the radio's menu offers.
            let case = menu.find(&format!("case {}:", f.menu)).unwrap_or_else(|| panic!("{} not in menu", f.menu));
            let body = &menu[case..case + menu[case..].find("break;").unwrap()];
            let max = body.split("subMaxItem = ").nth(1).unwrap_or_else(|| panic!("{}: no subMaxItem", f.menu));
            let max: usize = max.split(';').next().unwrap().trim().parse().unwrap();
            assert_eq!(f.options.len(), max, "{}: option count", f.label);

            // That the menu saves this choice into this member.
            let save = format!("g_radioInform.{}=g_menuInfo.selectedItem", f.member);
            assert!(compact_menu.contains(&save), "{}: the menu doesn't save {}", f.label, f.member);
        }
    }

    #[test]
    fn bit_fields_match_the_firmware() {
        let src = lossy(include_bytes!("../../../src/Common/PublType.h"));
        let end = src.find("}UNION_FLAG2").unwrap();
        let body = &src[src[..end].rfind("typedef union").unwrap()..end];
        let compact: String = body.split_whitespace().collect();
        assert!(compact.contains("b0:2;") && compact.contains("b2:2;"), "{compact}");
    }

    #[test]
    fn values_a_real_radio_holds_are_read_correctly() {
        let d = real();
        let shown = |label: &str| {
            let f = FIELDS.iter().find(|f| f.label == label).unwrap();
            selected(&d, f).map(|i| f.options[i])
        };
        assert_eq!(shown("Silenciador"), Some("3"));
        assert_eq!(shown("Ahorro batería"), Some("1:1"));
        assert_eq!(shown("Límite de TX"), Some("120 s"));
        assert_eq!(shown("Apagar luz"), Some("15 s"));
        assert_eq!(shown("Contraste"), Some("4"));
        assert_eq!(shown("Pantalla B"), Some("Frecuencia"));
        assert_eq!(shown("PF1 corto"), Some("Linterna"));
        assert_eq!(shown("Inicio: pantalla"), Some("Logo"));
    }

    #[test]
    fn only_the_chosen_values_change() {
        let d = real();
        let mut choices = vec![None; FIELDS.len()];
        assert_eq!(apply(&d, &choices, &message(&d)).unwrap(), d);

        choices[0] = Some(7);
        let out = apply(&d, &choices, &message(&d)).unwrap();
        assert_eq!(out[0], 7);
        assert_eq!(out[1..], d[1..]);

        assert!(apply(&d, &vec![Some(99); FIELDS.len()], &message(&d)).is_err());
    }

    #[test]
    fn shared_byte_fields_keep_their_neighbours() {
        let mut d = real();
        d[28] = 0b1100_0001;
        let at = |label: &str| FIELDS.iter().position(|f| f.label == label).unwrap();
        let mut choices = vec![None; FIELDS.len()];
        choices[at("Inicio: tono")] = Some(1);
        assert_eq!(apply(&d, &choices, "").unwrap()[28], 0b1100_0101);
        choices[at("Inicio: tono")] = None;
        choices[at("Inicio: pantalla")] = Some(0);
        assert_eq!(apply(&d, &choices, "").unwrap()[28], 0b1100_0000);
    }

    #[test]
    fn the_two_work_modes_share_a_byte_without_touching_each_other() {
        let mut d = real();
        d[26] = 0x11; // Both in channel mode, as a real radio holds them.
        let at = |label: &str| FIELDS.iter().position(|f| f.label == label).unwrap();
        assert_eq!(selected(&d, &FIELDS[at("Modo A")]), Some(1));
        assert_eq!(selected(&d, &FIELDS[at("Modo B")]), Some(1));
        let mut choices = vec![None; FIELDS.len()];
        choices[at("Modo A")] = Some(0);
        assert_eq!(apply(&d, &choices, "").unwrap()[26], 0x10);
        choices[at("Modo A")] = None;
        choices[at("Modo B")] = Some(0);
        assert_eq!(apply(&d, &choices, "").unwrap()[26], 0x01);
    }

    #[test]
    fn nothing_that_widens_transmit_or_locks_the_radio_is_offered() {
        for f in FIELDS {
            for banned in ["pwrPwdFlag", "pwrPassword", "txEnable"] {
                assert!(!f.member.contains(banned), "{}", f.member);
            }
        }
    }

    #[test]
    fn a_value_the_menu_lacks_stays_untouched() {
        let mut d = real();
        d[3] = 7; // Luz automática: the firmware can hold this, but its menu stops at 4.
        let f = FIELDS.iter().find(|f| f.label == "Apagar luz").unwrap();
        assert_eq!(selected(&d, f), None);
        let choices: Vec<Option<usize>> = FIELDS.iter().map(|f| selected(&d, f)).collect();
        assert_eq!(apply(&d, &choices, &message(&d)).unwrap(), d);
    }

    #[test]
    fn the_password_is_stored_as_the_radio_checks_it() {
        let d = real();
        assert!(!password_on(&d));
        assert_eq!(apply_password(&d, false, &password(&d)).unwrap(), d, "No change, no bytes changed");

        let out = apply_password(&d, true, "123456").unwrap();
        assert_eq!(out[60], 1);
        assert_eq!(out[61..64], [0x12, 0x34, 0x56]);
        assert!(password_on(&out));
        assert_eq!(password(&out), "123456");

        // Switching it off keeps the digits, and 0xFF counts as off, as in the firmware.
        let off = apply_password(&out, false, "123456").unwrap();
        assert_eq!(off[60], 0);
        assert_eq!(off[61..64], [0x12, 0x34, 0x56]);
        let mut ff = real();
        ff[60] = 0xFF;
        assert!(!password_on(&ff));
        assert_eq!(apply_password(&ff, false, &password(&ff)).unwrap()[60], 0xFF);

        for bad in ["", "12345", "1234567", "12A456", " "] {
            assert!(apply_password(&d, true, bad).is_err(), "{bad:?}");
        }
        assert!(apply_password(&d, false, "12").is_err());
        assert_eq!(password(&[0xFF; 64]), "");
        assert!(apply_password(&[0xFF; 64], true, "").is_err());
    }

    #[test]
    fn the_password_never_travels_in_a_settings_file() {
        let mut d = real();
        d[60] = 1;
        d[61..64].copy_from_slice(&[0x12, 0x34, 0x56]);
        let choices: Vec<Option<usize>> = FIELDS.iter().map(|f| selected(&d, f)).collect();
        let file = to_csv(&choices, "");
        assert!(!file.contains("123456") && !file.contains("pwr"), "{file}");
        assert!(from_csv("pwrPwdFlag,Clave,1,").is_err());
    }

    #[test]
    fn every_setting_has_help() {
        for f in FIELDS {
            assert!(!help(f).is_empty(), "{}", f.label);
        }
    }

    #[test]
    fn the_message_is_read_and_written_like_the_radio_does() {
        let mut d = real();
        assert_eq!(message(&d), "", "0xFF means no text");
        let none = vec![None; FIELDS.len()];
        let out = apply(&d, &none, "HOLA").unwrap();
        assert_eq!(&out[MESSAGE_AT..MESSAGE_AT + 4], b"HOLA");
        assert_eq!(out[MESSAGE_AT + 4..MESSAGE_AT + 16], [0; 12]);
        assert_eq!(out[MESSAGE_AT + 16..], d[MESSAGE_AT + 16..], "The rest of the block stays");
        assert_eq!(message(&out), "HOLA");

        // A shorter text leaves no old letters behind, and clearing it works.
        let again = apply(&out, &none, "HI").unwrap();
        assert_eq!(message(&again), "HI");
        assert_eq!(message(&apply(&again, &none, "").unwrap()), "");

        for bad in ["12345678901234567", "SEÑAL", "A,B", "A;B"] {
            assert!(apply(&d, &none, bad).is_err(), "{bad}");
        }

        // An odd text already in the radio stays byte for byte unless it's edited.
        d[MESSAGE_AT..MESSAGE_AT + 3].copy_from_slice(&[b'A', 0x85, b'B']);
        assert_eq!(apply(&d, &none, &message(&d)).unwrap(), d);
    }

    #[test]
    fn file_keeps_every_setting_and_rewrites_nothing_else() {
        let d = real();
        let choices: Vec<Option<usize>> = FIELDS.iter().map(|f| selected(&d, f)).collect();
        let (back, text) = from_csv(&to_csv(&choices, "RADIO 1")).unwrap();
        assert_eq!(back, choices);
        assert_eq!(text.as_deref(), Some("RADIO 1"));
        let (_, empty) = from_csv(&to_csv(&choices, "")).unwrap();
        assert_eq!(empty, None, "An empty text keeps the radio's");
        let (_, unsafe_text) = from_csv(&to_csv(&choices, "A,B")).unwrap();
        assert_eq!(unsafe_text, None);

        // Every member name must be unique, or a file couldn't tell them apart.
        for f in FIELDS {
            assert_eq!(FIELDS.iter().filter(|o| o.member == f.member).count(), 1, "{}", f.member);
        }
    }

    #[test]
    fn a_file_carries_settings_to_another_radio_without_its_private_bytes() {
        let source = real();
        let choices: Vec<Option<usize>> = FIELDS.iter().map(|f| selected(&source, f)).collect();
        let mut other = real();
        other[0] = 9; // A setting in the file: must follow the file.
        other[60..64].copy_from_slice(&[1, 2, 3, 4]); // Power-on password: must stay.
        other[BLOCK_SIZE] = b'X'; // Power-on text: must stay.
        let (file, text) = from_csv(&to_csv(&choices, "")).unwrap();
        assert_eq!(text, None);
        let out = apply(&other, &file, &message(&other)).unwrap();
        assert_eq!(out[0], source[0]);
        assert_eq!(out[60..64], [1, 2, 3, 4]);
        assert_eq!(out[BLOCK_SIZE], b'X');
    }

    #[test]
    fn partial_and_spreadsheet_files_are_read() {
        let (got, _) = from_csv("\u{feff}clave;nombre;valor;texto\r\nsqlLevel;Silenciador;5;5\r\nvoiceSw;Voz;;\r\n").unwrap();
        assert_eq!(got[0], Some(5));
        assert_eq!(got.iter().flatten().count(), 1);
    }

    #[test]
    fn a_bad_file_changes_nothing() {
        for bad in [
            "",
            "clave,nombre,valor,texto\r\n",
            "desconocido,X,1,1",
            "sqlLevel,Silenciador",
            "sqlLevel,Silenciador,10,10",
            "sqlLevel,Silenciador,abc,",
            "sqlLevel,Silenciador,-1,",
            "sqlLevel,Silenciador,1,1\nsqlLevel,Silenciador,2,2",
            "pwrPassword,Clave,1,1",
            "sqlLevel,Silenciador,1,1\nvoiceSw,Voz,7,",
            "powerOnMsg,Mensaje,SEÑAL,",
            "powerOnMsg,Mensaje,A,\npowerOnMsg,Mensaje,B,",
        ] {
            assert!(from_csv(bad).is_err(), "{bad}");
        }
    }

    /// Programming mode as in ProgromFlash.c. RAM is lost on restart unless 0x9040 saved it.
    struct FakeRadio {
        ram: Vec<u8>,
        saved: Vec<u8>,
        out: VecDeque<u8>,
        in_session: bool,
        writes: Vec<u16>,
        restarts: usize,
        keeps_nothing: bool,
    }

    impl FakeRadio {
        fn new() -> Self {
            Self { ram: real(), saved: real(), out: VecDeque::new(), in_session: false, writes: vec![], restarts: 0, keeps_nothing: false }
        }
    }

    impl Link for FakeRadio {
        fn send(&mut self, data: &[u8]) -> Res<()> {
            if data.starts_with(b"PROGRAM") {
                self.in_session = true;
                self.out.push_back(ACK);
                return Ok(());
            }
            assert!(self.in_session, "Command outside a session");
            match data[0] {
                b'F' => self.out.extend([0u8; BAND_REPLY_SIZE]),
                b'R' => {
                    let at = (u16::from_be_bytes([data[1], data[2]]) - ADDR_SETTINGS) as usize;
                    self.out.extend(&data[..4]);
                    self.out.extend(&self.ram[at..at + BLOCK_SIZE]);
                }
                b'W' => {
                    let addr = u16::from_be_bytes([data[1], data[2]]);
                    self.writes.push(addr);
                    assert_eq!(data.len(), 4 + BLOCK_SIZE);
                    let at = (addr - ADDR_SETTINGS) as usize;
                    self.ram[at..at + BLOCK_SIZE].copy_from_slice(&data[4..]);
                    if addr == ADDR_POWER_TEXT && !self.keeps_nothing {
                        self.saved = self.ram.clone();
                    }
                    self.out.push_back(ACK);
                }
                b'E' => {
                    self.out.push_back(ACK);
                    self.in_session = false;
                    self.restarts += 1;
                    self.ram = self.saved.clone();
                }
                other => panic!("Unknown command {other}"),
            }
            Ok(())
        }
        fn recv(&mut self, buf: &mut [u8]) -> Res<usize> {
            let n = buf.len().min(self.out.len());
            for b in buf.iter_mut().take(n) {
                *b = self.out.pop_front().unwrap();
            }
            Ok(n)
        }
        fn clear_input(&mut self) {
            self.out.clear();
        }
        fn set_lines(&mut self, _on: bool) {}
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("k6_settings_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn saving_writes_only_the_settings_and_checks_after_a_restart() {
        let mut radio = FakeRadio::new();
        let loaded = read_from_radio(&mut radio).unwrap();
        assert_eq!(loaded, real());
        assert_eq!(radio.restarts, 1);

        let mut choices = vec![None; FIELDS.len()];
        choices[0] = Some(5);
        let wanted = apply(&loaded, &choices, &message(&loaded)).unwrap();
        let dir = temp_dir("save");
        let (check, copy) = write_to_radio(&mut radio, &loaded, &wanted, &dir).unwrap();

        assert_eq!(check, wanted);
        assert_eq!(radio.saved, wanted);
        assert_eq!(radio.writes, [ADDR_SETTINGS, ADDR_POWER_TEXT]);
        assert_eq!(radio.restarts, 3);
        assert_eq!(fs::read(copy).unwrap(), loaded);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_setting_changed_on_the_radio_since_reading_stops_the_save() {
        let mut radio = FakeRadio::new();
        let loaded = read_from_radio(&mut radio).unwrap();
        radio.ram[0] = 9;
        radio.saved[0] = 9;
        let wanted = apply(&loaded, &vec![Some(0); FIELDS.len()], "").unwrap();
        let dir = temp_dir("changed");
        assert!(write_to_radio(&mut radio, &loaded, &wanted, &dir).is_err());
        assert!(radio.writes.is_empty());
        assert_eq!(radio.restarts, 2, "The radio must still be released");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_lost_on_restart_are_reported() {
        let mut radio = FakeRadio::new();
        radio.keeps_nothing = true;
        let loaded = read_from_radio(&mut radio).unwrap();
        let mut choices = vec![None; FIELDS.len()];
        choices[0] = Some(5);
        let wanted = apply(&loaded, &choices, &message(&loaded)).unwrap();
        let dir = temp_dir("lost");
        let e = write_to_radio(&mut radio, &loaded, &wanted, &dir).unwrap_err();
        assert!(e.contains("no conservó"), "{e}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_the_two_settings_addresses_can_be_written() {
        let mut radio = FakeRadio::new();
        radio.in_session = true;
        for addr in [0x0000, 0x8000, 0x8FFF, 0x9001, 0x9080, 0xA000, 0xF000] {
            assert!(write_block(&mut radio, addr, &[0; BLOCK_SIZE]).is_err(), "{addr:04X}");
        }
        assert!(radio.writes.is_empty());
    }
}
