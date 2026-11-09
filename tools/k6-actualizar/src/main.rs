//! Installs the firmware and Spanish voices on a Baofeng UV-K6 family radio.
//!
//! Two protocols are involved:
//! - The bootloader (radio showing UPDATE), as implemented by BFK6_Bootloader.exe.
//! - The running firmware's flash mode, in src/CPS/ProgromFlash.c.
//!
//! This program never sends the chip erase command, and refuses any erase or write
//! to the external flash outside the Spanish voice area.

#![cfg_attr(not(test), windows_subsystem = "windows")]


use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Progress reports from the worker thread to the window.
pub enum Msg {
    Log(String),
    Progress(String, f32),
    ChannelsRead(Vec<u8>),
    Finished(Result<String, String>),
}

static SINK: Mutex<Option<Sender<Msg>>> = Mutex::new(None);

pub fn set_sink(tx: Sender<Msg>) {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
}

fn emit(m: Msg) {
    if let Some(tx) = SINK.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(m);
    }
}

macro_rules! say {
    () => { emit(Msg::Log(String::new())) };
    ($($a:tt)*) => { emit(Msg::Log(format!($($a)*))) };
}

mod channels;
mod gui;

// Built-in files, present only when build.rs finds them in out/. Without them the program
// still works: firmware is chosen from a file, and Spanish voices are not offered.
#[cfg(k6_embedded)]
static FIRMWARE: &[u8] = include_bytes!("../../../out/bf_uvk6-rx-spectrum.bin");
#[cfg(k6_embedded)]
static VOICES: &[u8] = include_bytes!("../../../out/voces_es_gonzalo/voces_es.bin");
#[cfg(not(k6_embedded))]
static FIRMWARE: &[u8] = &[];
#[cfg(not(k6_embedded))]
static VOICES: &[u8] = &[];

const FW_MAX: usize = 120 * 1024;
const FW_STACK_TOP: u32 = 0x2000_5560;
const FW_FLASH_START: u32 = 0x0800_2000;

const FLASH_SIZE: usize = 0x20_0000;
const FACTORY_VOICES: usize = 0xD_0000;
const FACTORY_VOICE_COUNT: usize = 122;
const ES_BASE: usize = 0x15_0000;
const ES_HEADER: usize = 0x1000; // Table and magic: written last.
const ES_MAGIC: u32 = 0x1053_4541;
const ES_MAGIC_OFFSET: usize = 0x7F0;
const SECTOR: usize = 0x1000;
const BLOCK: usize = 1024;

// Commands of the firmware's flash mode.
const CMD_HANDSHAKE: u8 = 0x02;
const CMD_ADDR: u8 = 0x03;
const CMD_ERASE: u8 = 0x04;
const CMD_END: u8 = 0x06;
const CMD_READ: u8 = 0x52;
const CMD_WRITE: u8 = 0x57;
const STATUS_OK: u8 = 0x59;
const ERASE_MODE_4K: u16 = 0x0002;

// Commands of the bootloader.
const BOOT_HANDSHAKE: u8 = 0x01;
const BOOT_DATA: u8 = 0x03;
const BOOT_TOTAL: u8 = 0x04;
const BOOT_END: u8 = 0x45;
const BOOT_OK: u8 = 0x06;

// Short waits in tests, where the radio is simulated.
const MS_REPLY: u64 = if cfg!(test) { 5 } else { 5000 };
const MS_LINK_ACK: u64 = if cfg!(test) { 5 } else { 3000 };
const MS_PAUSE: u64 = if cfg!(test) { 0 } else { 300 };
const MS_LINK_RETRY: u64 = if cfg!(test) { 0 } else { 4000 };
const MS_WAIT_UPDATE: u64 = if cfg!(test) { 200 } else { 120_000 };
const MS_WAIT_BOOT: u64 = if cfg!(test) { 200 } else { 90_000 };

type Res<T> = Result<T, String>;

trait Link {
    fn send(&mut self, data: &[u8]) -> Res<()>;
    /// Returns how many bytes arrived. Zero means nothing came during a short wait.
    fn recv(&mut self, buf: &mut [u8]) -> Res<usize>;
    fn clear_input(&mut self);
    fn set_lines(&mut self, on: bool);
}

struct Serial(Box<dyn serialport::SerialPort>);

impl Link for Serial {
    fn send(&mut self, data: &[u8]) -> Res<()> {
        self.0.write_all(data).map_err(|e| format!("Error al enviar: {e}"))?;
        self.0.flush().map_err(|e| format!("Error al enviar: {e}"))
    }
    fn recv(&mut self, buf: &mut [u8]) -> Res<usize> {
        match self.0.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(format!("Error al leer: {e}")),
        }
    }
    fn clear_input(&mut self) {
        let _ = self.0.clear(serialport::ClearBuffer::Input);
    }
    fn set_lines(&mut self, on: bool) {
        let _ = self.0.write_data_terminal_ready(on);
        let _ = self.0.write_request_to_send(on);
    }
}

fn pause(ms: u64) {
    if ms > 0 {
        sleep(Duration::from_millis(ms));
    }
}

/// CRC-16/XMODEM, as in src/Driver/crc.c and BFK6's CrcValidation.
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn read_exact(link: &mut dyn Link, count: usize, ms: u64) -> Res<Vec<u8>> {
    let mut buf = vec![0u8; count];
    let mut got = 0;
    let deadline = Instant::now() + Duration::from_millis(ms);
    while got < count {
        got += link.recv(&mut buf[got..])?;
        if got < count && Instant::now() > deadline {
            return Err(format!("El radio no respondió ({got} de {count} bytes)"));
        }
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Bootloader
// ---------------------------------------------------------------------------

fn boot_packet(cmd: u8, arg: u8, data: &[u8]) -> Vec<u8> {
    let mut p = vec![0xAA, cmd, arg, (data.len() >> 8) as u8, data.len() as u8];
    p.extend_from_slice(data);
    let crc = crc16(&p[1..]);
    p.extend_from_slice(&[(crc >> 8) as u8, crc as u8, 0xEF]);
    p
}

/// Returns (command, argument) of the next valid reply.
fn boot_reply(link: &mut dyn Link, ms: u64) -> Res<(u8, u8)> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        let mut one = [0u8; 1];
        if link.recv(&mut one)? == 1 && one[0] == 0xAA {
            break;
        }
        if Instant::now() > deadline {
            return Err("sin respuesta".into());
        }
    }
    let head = read_exact(link, 4, ms)?;
    let len = ((head[2] as usize) << 8) | head[3] as usize;
    let rest = read_exact(link, len + 3, ms)?;
    let mut body = head.clone();
    body.extend_from_slice(&rest[..len]);
    let crc = ((rest[len] as u16) << 8) | rest[len + 1] as u16;
    if crc16(&body) != crc {
        return Err("respuesta con CRC incorrecta".into());
    }
    Ok((head[0], head[1]))
}

fn boot_exchange(link: &mut dyn Link, packet: &[u8], cmd: u8) -> Res<()> {
    let mut last = String::new();
    // BFK6 sends each packet up to six times: the first one and five retries.
    for _ in 0..6 {
        link.clear_input();
        link.send(packet)?;
        match boot_reply(link, MS_REPLY) {
            Ok((c, BOOT_OK)) if c == cmd => return Ok(()),
            Ok((0xE1, _)) => return Err("El radio rechazó el saludo".into()),
            Ok((0xE2, _)) => last = "error de datos".into(),
            Ok((0xE3, _)) => return Err("El radio informó un error de dirección".into()),
            Ok((0xE4, _)) => return Err("El radio informó un error al escribir su memoria".into()),
            Ok((0xE5, _)) => return Err("El radio informó un comando erróneo".into()),
            Ok((c, a)) => last = format!("respuesta inesperada {c:02X}/{a:02X}"),
            Err(e) => last = e,
        }
    }
    Err(format!("Sin confirmación del radio: {last}"))
}

fn check_firmware(fw: &[u8]) -> Res<()> {
    if fw.len() < 8 || fw.len() > FW_MAX {
        return Err(format!("Tamaño de firmware no válido: {}", fw.len()));
    }
    let sp = u32::from_le_bytes([fw[0], fw[1], fw[2], fw[3]]);
    let reset = u32::from_le_bytes([fw[4], fw[5], fw[6], fw[7]]);
    if sp != FW_STACK_TOP || reset < FW_FLASH_START || reset >= FW_FLASH_START + FW_MAX as u32 {
        return Err(format!("El firmware no parece de este radio (SP {sp:08X}, inicio {reset:08X})"));
    }
    Ok(())
}

fn install_firmware(link: &mut dyn Link, fw: &[u8]) -> Res<()> {
    check_firmware(fw)?;
    link.set_lines(true); // BFK6 opens the port with DTR and RTS on.

    say!("Esperando el modo UPDATE...");
    say!("  1. Apaga el radio, con el cable conectado.");
    say!("  2. Mantén los 2 botones naranjas y enciéndelo.");
    say!("  3. Suelta cuando la pantalla diga UPDATE.");

    let hello = boot_packet(BOOT_HANDSHAKE, 0, b"BOOTLOADER");
    let deadline = Instant::now() + Duration::from_millis(MS_WAIT_UPDATE);
    loop {
        link.clear_input();
        link.send(&hello)?;
        match boot_reply(link, if cfg!(test) { 5 } else { 1000 }) {
            Ok((BOOT_HANDSHAKE, BOOT_OK)) => break,
            Ok((0xE1, _)) => return Err("El radio rechazó el saludo".into()),
            _ => {}
        }
        if Instant::now() > deadline {
            return Err("El radio no entró en modo UPDATE a tiempo".into());
        }
    }
    say!("Modo UPDATE detectado. No toques el radio ni el cable.");

    let total = fw.len().div_ceil(BLOCK);
    if total > 255 {
        return Err("Firmware demasiado grande para el protocolo".into());
    }
    boot_exchange(link, &boot_packet(BOOT_TOTAL, 0, &[total as u8]), BOOT_TOTAL)?;

    for (i, chunk) in fw.chunks(BLOCK).enumerate() {
        let mut block = chunk.to_vec();
        block.resize(BLOCK, 0); // BFK6 pads the last block with zeros.
        boot_exchange(link, &boot_packet(BOOT_DATA, i as u8, &block), BOOT_DATA)
            .map_err(|e| format!("Bloque {}: {e}", i + 1))?;
        emit(Msg::Progress("Firmware".into(), (i + 1) as f32 / total as f32));
    }
    boot_exchange(link, &boot_packet(BOOT_END, 0, &[]), BOOT_END)?;
    say!("Firmware cargado.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Firmware flash mode
// ---------------------------------------------------------------------------

fn fw_packet(cmd: u8, number: u16, data: &[u8]) -> Vec<u8> {
    let mut p = vec![
        0xA5,
        cmd,
        (number >> 8) as u8,
        number as u8,
        (data.len() >> 8) as u8,
        data.len() as u8,
    ];
    p.extend_from_slice(data);
    let crc = crc16(&p[1..]);
    p.extend_from_slice(&[(crc >> 8) as u8, crc as u8]);
    p
}

fn assert_es_range(addr: usize, len: usize) -> Res<()> {
    if addr < ES_BASE || addr.checked_add(len).map_or(true, |end| end > FLASH_SIZE) {
        return Err(format!("Rechazado: 0x{addr:06X}+{len} está fuera de la zona de voces"));
    }
    Ok(())
}

fn assert_es_sector(addr: usize) -> Res<()> {
    if addr % SECTOR != 0 {
        return Err(format!("Rechazado: 0x{addr:06X} no es inicio de sector"));
    }
    assert_es_range(addr, SECTOR)
}

fn fw_command(link: &mut dyn Link, packet: &[u8], cmd: u8) -> Res<()> {
    link.clear_input();
    link.send(packet)?;
    let r = read_exact(link, 9, MS_REPLY)?;
    let crc = ((r[7] as u16) << 8) | r[8] as u16;
    if r[0] != 0xA5 || r[1] != cmd || r[6] != STATUS_OK || crc16(&r[1..7]) != crc {
        return Err(format!("El radio rechazó el comando {cmd:02X}: {r:02X?}"));
    }
    Ok(())
}

fn read_flash(link: &mut dyn Link, addr: usize, len: usize) -> Res<Vec<u8>> {
    let mut d = (addr as u32).to_le_bytes().to_vec();
    d.extend_from_slice(&[(len >> 8) as u8, len as u8]);
    let req = fw_packet(CMD_READ, 0, &d);
    let mut last = String::new();
    for _ in 0..3 {
        link.clear_input();
        if let Err(e) = link.send(&req) {
            last = e;
            continue;
        }
        let attempt = (|| -> Res<Vec<u8>> {
            let head = read_exact(link, 6, MS_REPLY)?;
            if head[0] != 0xA5 || head[1] != CMD_READ {
                return Err(format!(
                    "respuesta inesperada {:02X}/{:02X}; ¿tiene el firmware nuevo?",
                    head[0], head[1]
                ));
            }
            if ((head[4] as usize) << 8 | head[5] as usize) != len {
                return Err("longitud incorrecta; ¿tiene el firmware nuevo?".into());
            }
            let rest = read_exact(link, len + 2, MS_REPLY)?;
            let mut all = head[1..].to_vec();
            all.extend_from_slice(&rest[..len]);
            if crc16(&all) != ((rest[len] as u16) << 8 | rest[len + 1] as u16) {
                return Err("CRC incorrecta".into());
            }
            Ok(rest[..len].to_vec())
        })();
        match attempt {
            Ok(v) => return Ok(v),
            Err(e) => last = e,
        }
        pause(MS_PAUSE);
    }
    Err(format!("Lectura de 0x{addr:06X}: {last}"))
}

fn read_range(link: &mut dyn Link, addr: usize, len: usize, label: &str) -> Res<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let n = BLOCK.min(len - out.len());
        out.extend(read_flash(link, addr + out.len(), n)?);
        emit(Msg::Progress(label.to_string(), out.len() as f32 / len as f32));
    }
    Ok(out)
}

/// Unguarded: only call through the functions below, which check the address first.
fn send_erase(link: &mut dyn Link, addr: usize) -> Res<()> {
    let mut d = (addr as u32).to_le_bytes().to_vec();
    d.extend_from_slice(&[0, 1]); // One sector per command.
    fw_command(link, &fw_packet(CMD_ERASE, ERASE_MODE_4K, &d), CMD_ERASE)
}

/// Unguarded: only call through the functions below, which check the address first.
fn send_write(link: &mut dyn Link, addr: usize, data: &[u8]) -> Res<()> {
    let start = (addr as u32).to_le_bytes();
    fw_command(link, &fw_packet(CMD_ADDR, 0, &start), CMD_ADDR)?;
    for (i, chunk) in data.chunks(BLOCK).enumerate() {
        fw_command(link, &fw_packet(CMD_WRITE, i as u16, chunk), CMD_WRITE)?;
    }
    Ok(())
}

fn erase_sector(link: &mut dyn Link, addr: usize) -> Res<()> {
    assert_es_sector(addr)?;
    send_erase(link, addr)
}

fn write_range(link: &mut dyn Link, addr: usize, data: &[u8]) -> Res<()> {
    assert_es_range(addr, data.len())?;
    send_write(link, addr, data)
}

fn assert_channel_range(addr: usize, len: usize) -> Res<()> {
    if addr.checked_add(len).map_or(true, |end| end > channels::CHANNEL_AREA) {
        return Err(format!("Rechazado: 0x{addr:06X}+{len} está fuera de la zona de canales"));
    }
    Ok(())
}

fn erase_channel_sector(link: &mut dyn Link, addr: usize) -> Res<()> {
    if addr % SECTOR != 0 {
        return Err(format!("Rechazado: 0x{addr:06X} no es inicio de sector"));
    }
    assert_channel_range(addr, SECTOR)?;
    send_erase(link, addr)
}

fn write_channel_range(link: &mut dyn Link, addr: usize, data: &[u8]) -> Res<()> {
    assert_channel_range(addr, data.len())?;
    send_write(link, addr, data)
}

/// Puts the running firmware in flash mode, trying both DTR/RTS states.
fn enter_flash_mode(link: &mut dyn Link, wait_ms: u64) -> Res<()> {
    let header = b"PROGRAMBF-K6PROU";
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    let mut lines = false;
    loop {
        link.set_lines(lines);
        link.clear_input();
        link.send(header)?;
        if let Ok(ack) = read_exact(link, 1, MS_LINK_ACK) {
            if ack[0] == 0x06 {
                say!("  Enlace establecido con DTR/RTS {}.", if lines { "activados" } else { "desactivados" });
                break;
            }
        }
        if Instant::now() > deadline {
            return Err(
                "El radio no responde. Comprueba que esté en la pantalla principal y que el cable \
                 entre hasta el fondo. Si sigue igual, vuelve a cargar el firmware."
                    .into(),
            );
        }
        lines = !lines;
        pause(MS_LINK_RETRY);
    }
    pause(MS_PAUSE);
    link.clear_input();
    link.send(b"D")?;
    pause(MS_PAUSE);
    fw_command(link, &fw_packet(CMD_HANDSHAKE, 0, &[]), CMD_HANDSHAKE)
}

fn leave_flash_mode(link: &mut dyn Link) {
    let _ = link.send(&fw_packet(CMD_END, 0, &[]));
    pause(100);
}

fn check_voice_image(img: &[u8]) -> Res<()> {
    if img.len() <= ES_HEADER || img.len() > FLASH_SIZE - ES_BASE {
        return Err(format!("Tamaño de voces no válido: {}", img.len()));
    }
    if le32(img, ES_MAGIC_OFFSET) != ES_MAGIC {
        return Err("La imagen de voces no tiene la marca esperada".into());
    }
    Ok(())
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn utc_stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}{m:02}{d:02}_{:02}{:02}{:02}_UTC", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The "backups" folder next to the executable.
fn backup_dir() -> Res<PathBuf> {
    let exe = std::env::current_exe().map_err(|e| format!("No se encuentra la carpeta del programa: {e}"))?;
    let parent = exe.parent().ok_or("No se encuentra la carpeta del programa")?;
    Ok(parent.join("backups"))
}

/// Creates the folder if needed, and says so when it does.
pub(crate) fn ensure_backup_dir(dir: &std::path::Path) -> Res<()> {
    if dir.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(dir).map_err(|e| {
        format!("No se pudo crear la carpeta de copias {}: {e}. Mueve el programa a una carpeta donde puedas escribir.", dir.display())
    })?;
    say!("Carpeta de copias creada: {}", dir.display());
    Ok(())
}

fn save_backup(dir: &PathBuf, data: &[u8]) -> Res<PathBuf> {
    ensure_backup_dir(dir)?;
    let path = dir.join(format!("Baofeng_K6_flash_{}.bin", utc_stamp()));
    let tmp = path.with_extension("part");
    fs::write(&tmp, data).map_err(|e| format!("No se pudo guardar la copia: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("No se pudo guardar la copia: {e}"))?;
    Ok(path)
}

/// Erases the Spanish voices found in this backup. Returns false if there were none.
/// Only touches the sectors their own table says they occupy.
fn erase_spanish(link: &mut dyn Link, backup: &[u8]) -> Res<bool> {
    if le32(backup, ES_BASE + ES_MAGIC_OFFSET) != ES_MAGIC {
        return Ok(false);
    }
    let mut old_end = 0usize;
    let mut at = ES_BASE;
    while le32(backup, at) != u32::MAX && at < ES_BASE + ES_MAGIC_OFFSET {
        let end = le32(backup, at) as usize + le32(backup, at + 4) as usize;
        if end > FLASH_SIZE - ES_BASE {
            return Err("La tabla de voces anterior no es válida. No se escribió nada.".into());
        }
        old_end = old_end.max(end);
        at += 8;
    }
    let end = ES_BASE + old_end.div_ceil(SECTOR) * SECTOR;
    assert_es_range(ES_BASE, end - ES_BASE)?;
    say!("  Borrando las voces en español (0x{ES_BASE:06X}..0x{end:06X})...");
    // The sector with the magic goes first: from then on the radio uses factory voices.
    for addr in (ES_BASE..end).step_by(SECTOR) {
        erase_sector(link, addr)?;
    }
    let erased = read_range(link, ES_BASE, end - ES_BASE, "Comprobando borrado")?;
    if erased.iter().any(|&b| b != 0xFF) {
        return Err("El borrado no se completó".into());
    }
    Ok(true)
}

/// Writes the voices given a full flash backup of this radio.
fn write_voices(link: &mut dyn Link, backup: &[u8], img: &[u8]) -> Res<&'static str> {
    check_voice_image(img)?;
    if backup.len() != FLASH_SIZE {
        return Err(format!("Copia de tamaño inesperado: {}", backup.len()));
    }
    let factory = (0..FACTORY_VOICE_COUNT)
        .filter(|i| {
            let at = FACTORY_VOICES + i * 8;
            let (len, addr) = (le32(backup, at), le32(backup, at + 4));
            len > 0 && len <= 40960 && addr < 0x8_0000
        })
        .count();
    if factory != FACTORY_VOICE_COUNT {
        return Err(format!(
            "Solo {factory} de {FACTORY_VOICE_COUNT} voces de fábrica están donde deben. No se escribió nada."
        ));
    }
    if &backup[ES_BASE..ES_BASE + img.len()] == img {
        return Ok("Estas voces ya estaban instaladas; no se escribió nada.");
    }

    if !erase_spanish(link, backup)? && backup[ES_BASE..ES_BASE + img.len()].iter().any(|&b| b != 0xFF) {
        return Err("La zona de voces tiene datos desconocidos. No se escribió nada.".into());
    }

    say!("  Grabando audios...");
    write_range(link, ES_BASE + ES_HEADER, &img[ES_HEADER..])?;
    let check = read_range(link, ES_BASE + ES_HEADER, img.len() - ES_HEADER, "Verificando audios")?;
    if check != img[ES_HEADER..] {
        return Err("Los audios no coinciden al leerlos de vuelta".into());
    }
    say!("  Grabando tabla...");
    write_range(link, ES_BASE, &img[..ES_HEADER])?;
    let check = read_range(link, ES_BASE, ES_HEADER, "Verificando tabla")?;
    if check != img[..ES_HEADER] {
        return Err("La tabla no coincide al leerla de vuelta".into());
    }
    Ok("Voces grabadas y verificadas.")
}

fn take_backup(link: &mut dyn Link, dir: &PathBuf) -> Res<(Vec<u8>, PathBuf)> {
    say!("Copia de la memoria flash (unos 4 minutos):");
    let backup = read_range(link, 0, FLASH_SIZE, "Copia")?;
    let path = save_backup(dir, &backup)?;
    say!("  Copia guardada: {}", path.display());
    Ok((backup, path))
}

fn say_waiting() {
    say!("Esperando a que el radio esté en la pantalla principal...");
    say!("  Si no arranca solo, apágalo y enciéndelo, sin los botones naranjas.");
}

fn backup_only(link: &mut dyn Link, dir: &PathBuf, wait_ms: u64) -> Res<PathBuf> {
    enter_flash_mode(link, wait_ms)?;
    let result = take_backup(link, dir).map(|(_, path)| path);
    leave_flash_mode(link); // Also after an error: restarts the radio.
    result
}

fn install_voices(link: &mut dyn Link, img: &[u8], dir: &PathBuf) -> Res<PathBuf> {
    say_waiting();
    enter_flash_mode(link, MS_WAIT_BOOT)?;
    let result = take_backup(link, dir).and_then(|(backup, path)| {
        say!("{}", write_voices(link, &backup, img)?);
        Ok(path)
    });
    leave_flash_mode(link);
    result
}

/// Goes back to the factory voices. They were never modified, so erasing is enough.
fn remove_voices(link: &mut dyn Link, dir: &PathBuf) -> Res<PathBuf> {
    say_waiting();
    enter_flash_mode(link, MS_WAIT_BOOT)?;
    let result = take_backup(link, dir).and_then(|(backup, path)| {
        if erase_spanish(link, &backup)? {
            say!("Voces en español borradas: vuelve a sonar el inglés de fábrica.");
        } else {
            say!("Este radio ya usa las voces de fábrica; no se borró nada.");
        }
        Ok(path)
    });
    leave_flash_mode(link);
    result
}

/// Available ports, each with whether it is the programming cable (CP210x).
pub fn list_ports() -> Vec<(String, bool)> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let cable = matches!(&p.port_type, serialport::SerialPortType::UsbPort(u) if u.vid == 0x10C4);
            (p.port_name, cable)
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Language {
    Spanish,
    Factory,
}

pub enum Action {
    Backup,
    /// The firmware file to load, or the one built into this program.
    Install(Option<PathBuf>),
    SetLanguage(Language),
    ReadChannels,
    WriteChannels { loaded: Vec<u8>, wanted: Vec<u8> },
}

fn apply_language(link: &mut dyn Link, language: Language, dir: &PathBuf) -> Res<PathBuf> {
    match language {
        Language::Spanish => install_voices(link, VOICES, dir),
        Language::Factory => remove_voices(link, dir),
    }
}

/// Runs one action from start to finish. Returns the closing message.
pub fn run_action(action: Action, port_name: &str) -> Res<String> {
    // Before touching the port: a bad file must not cost the user a trip to UPDATE mode.
    let firmware = match &action {
        Action::Install(Some(path)) => {
            let data = fs::read(path).map_err(|e| format!("No se pudo leer {}: {e}", path.display()))?;
            check_firmware(&data)?;
            say!("Firmware: {} ({} bytes)", path.display(), data.len());
            Some(data)
        }
        Action::Install(None) if FIRMWARE.is_empty() => {
            return Err("Esta versión no trae firmware incluido: elige un archivo .bin.".into());
        }
        Action::Install(None) => {
            check_firmware(FIRMWARE)?;
            say!("Firmware: el incluido en el programa ({} bytes)", FIRMWARE.len());
            Some(FIRMWARE.to_vec())
        }
        Action::SetLanguage(Language::Spanish) if VOICES.is_empty() => {
            return Err("Esta versión no trae las voces en español.".into());
        }
        _ => None,
    };

    say!("Puerto: {port_name}. Cierra BFK6_Bootloader si está abierto.");
    let port = serialport::new(port_name, 115_200)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| format!("No se pudo abrir {port_name}: {e}"))?;
    let mut link = Serial(port);
    let dir = backup_dir()?;

    const VOICE_END: &str = "Listo. Desconecta el cable y reinicia el radio. En su menú: VOICE activado.";
    match action {
        Action::Backup => {
            say_waiting();
            let path = backup_only(&mut link, &dir, MS_WAIT_BOOT)?;
            Ok(format!("Copia terminada. Guardada en:
{}", path.display()))
        }
        Action::Install(_) => {
            install_firmware(&mut link, firmware.as_deref().unwrap_or(FIRMWARE))?;
            Ok("Firmware cargado. Si el radio no arranca solo, apágalo y enciéndelo sin los botones naranjas.".into())
        }
        Action::SetLanguage(language) => {
            let path = apply_language(&mut link, language, &dir)?;
            Ok(format!("{VOICE_END}

Copia de la flash guardada en:
{}", path.display()))
        }
        Action::ReadChannels => {
            say_waiting();
            let area = channels::read_from_radio(&mut link)?;
            let found = channels::parse_channels(&area).len();
            emit(Msg::ChannelsRead(area));
            Ok(format!("{found} canales leídos."))
        }
        Action::WriteChannels { loaded, wanted } => {
            say_waiting();
            let (changed, path) = channels::write_to_radio(&mut link, &loaded, &wanted, &dir)?;
            Ok(if changed == 0 {
                "No había cambios que guardar.".to_string()
            } else {
                format!(
                    "Canales guardados y verificados ({changed} zonas de memoria reescritas).

Copia de los canales anteriores en:
{}",
                    path.display()
                )
            })
        }
    }
}

fn main() {
    gui::run();
}

#[cfg(all(test, k6_embedded))]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    // Packets captured from k6_voices_write.ps1 -SelfTest, which works on the real radio.
    #[test]
    fn packets_match_the_tested_script() {
        assert_eq!(crc16(b"123456789"), 0x31C3);
        assert_eq!(fw_packet(CMD_HANDSHAKE, 0, &[]), [0xA5, 0x02, 0, 0, 0, 0, 0x44, 0x83]);
        assert_eq!(
            fw_packet(CMD_WRITE, 0x0102, &[1, 2, 3]),
            [0xA5, 0x57, 1, 2, 0, 3, 1, 2, 3, 0xC5, 0xB0]
        );
        assert_eq!(
            fw_packet(CMD_ERASE, ERASE_MODE_4K, &[0, 0, 0x15, 0, 0, 1]),
            [0xA5, 0x04, 0, 2, 0, 6, 0, 0, 0x15, 0, 0, 1, 0x81, 0xC6]
        );
        let read = fw_packet(CMD_READ, 0, &[0, 0, 0x0D, 0, 4, 0]);
        assert_eq!(&read[read.len() - 2..], [0xE5, 0x3C]);
    }

    #[test]
    fn range_guards_refuse_everything_outside_the_voice_area() {
        for (a, l) in [(0, 16), (0xD0000, 16), (0x14FFFF, 2), (0x1FFFFF, 2), (usize::MAX, 2)] {
            assert!(assert_es_range(a, l).is_err(), "{a:X}");
        }
        for a in [0, 0x8000, 0xD0000, 0x14F000, 0x150800, 0x200000] {
            assert!(assert_es_sector(a).is_err(), "{a:X}");
        }
        assert!(assert_es_range(ES_BASE, 16).is_ok());
        assert!(assert_es_sector(0x1FF000).is_ok());
    }

    #[test]
    fn embedded_files_are_valid() {
        check_firmware(FIRMWARE).unwrap();
        check_voice_image(VOICES).unwrap();
        assert!(check_firmware(&[0u8; 64]).is_err());
    }

    /// Simulates the bootloader, following BootHelper in BFK6_Bootloader.exe.
    #[derive(Default)]
    struct FakeBootloader {
        rx: VecDeque<u8>,
        ignore_hello: usize,
        corrupt_block: Option<u8>,
        total: u8,
        blocks: Vec<Vec<u8>>,
        finished: bool,
        lines: bool,
    }

    impl FakeBootloader {
        fn reply(&mut self, cmd: u8, arg: u8) {
            self.rx.extend(boot_packet(cmd, arg, &[]));
        }
    }

    impl Link for FakeBootloader {
        fn send(&mut self, p: &[u8]) -> Res<()> {
            assert_eq!(p[0], 0xAA);
            assert_eq!(*p.last().unwrap(), 0xEF);
            let len = (p[3] as usize) << 8 | p[4] as usize;
            assert_eq!(p.len(), len + 8);
            let crc = (p[5 + len] as u16) << 8 | p[6 + len] as u16;
            assert_eq!(crc16(&p[1..5 + len]), crc);
            let data = &p[5..5 + len];
            match p[1] {
                BOOT_HANDSHAKE => {
                    assert_eq!(data, b"BOOTLOADER");
                    assert!(self.lines, "BFK6 uses DTR and RTS on");
                    if self.ignore_hello > 0 {
                        self.ignore_hello -= 1;
                    } else {
                        self.reply(BOOT_HANDSHAKE, BOOT_OK);
                    }
                }
                BOOT_TOTAL => {
                    self.total = data[0];
                    self.reply(BOOT_TOTAL, BOOT_OK);
                }
                BOOT_DATA => {
                    assert_eq!(len, BLOCK);
                    if self.corrupt_block == Some(p[2]) {
                        self.corrupt_block = None;
                        self.reply(0xE2, 0);
                        return Ok(());
                    }
                    assert_eq!(p[2] as usize, self.blocks.len());
                    self.blocks.push(data.to_vec());
                    self.reply(BOOT_DATA, BOOT_OK);
                }
                BOOT_END => {
                    self.finished = true;
                    self.reply(BOOT_END, BOOT_OK);
                }
                c => panic!("Unknown bootloader command {c:02X}"),
            }
            Ok(())
        }
        fn recv(&mut self, buf: &mut [u8]) -> Res<usize> {
            let n = buf.len().min(self.rx.len());
            for b in buf.iter_mut().take(n) {
                *b = self.rx.pop_front().unwrap();
            }
            Ok(n)
        }
        fn clear_input(&mut self) {
            self.rx.clear();
        }
        fn set_lines(&mut self, on: bool) {
            self.lines = on;
        }
    }

    #[test]
    fn firmware_upload_matches_the_bootloader_protocol() {
        let mut radio = FakeBootloader { ignore_hello: 3, corrupt_block: Some(5), ..Default::default() };
        install_firmware(&mut radio, FIRMWARE).unwrap();
        assert!(radio.finished);
        assert_eq!(radio.total as usize, FIRMWARE.len().div_ceil(BLOCK));
        assert_eq!(radio.blocks.len(), radio.total as usize);
        let sent: Vec<u8> = radio.blocks.concat();
        assert_eq!(&sent[..FIRMWARE.len()], FIRMWARE);
        assert!(sent[FIRMWARE.len()..].iter().all(|&b| b == 0));
    }

    #[test]
    fn firmware_upload_stops_on_a_write_error() {
        struct WriteError(FakeBootloader);
        impl Link for WriteError {
            fn send(&mut self, p: &[u8]) -> Res<()> {
                if p[1] == BOOT_DATA {
                    self.0.reply(0xE4, 0);
                    return Ok(());
                }
                self.0.send(p)
            }
            fn recv(&mut self, b: &mut [u8]) -> Res<usize> {
                self.0.recv(b)
            }
            fn clear_input(&mut self) {
                self.0.clear_input()
            }
            fn set_lines(&mut self, on: bool) {
                self.0.set_lines(on)
            }
        }
        let e = install_firmware(&mut WriteError(FakeBootloader::default()), FIRMWARE).unwrap_err();
        assert!(e.contains("escribir"), "{e}");
    }

    /// Simulates src/CPS/ProgromFlash.c on top of a NOR flash, which only clears bits.
    struct FakeRadio {
        rx: VecDeque<u8>,
        flash: Vec<u8>,
        start: usize,
        next: u16,
        linked: bool,
        flash_mode: bool,
        ended: bool,
        need_lines: Option<bool>,
        lines: bool,
        allow_channels: bool,
    }

    impl FakeRadio {
        fn new() -> Self {
            let mut flash = vec![0xFFu8; FLASH_SIZE];
            flash[..0x1000].fill(0x11); // Channels: must never change.
            flash[0xC000..0xD000].fill(0x22); // Calibration and settings: must never change.
            for i in 0..FACTORY_VOICE_COUNT {
                let at = FACTORY_VOICES + i * 8;
                flash[at..at + 4].copy_from_slice(&1000u32.to_le_bytes());
                flash[at + 4..at + 8].copy_from_slice(&(0x800u32 + i as u32 * 1000).to_le_bytes());
            }
            FakeRadio {
                rx: VecDeque::new(),
                flash,
                start: 0,
                next: 0,
                linked: false,
                flash_mode: false,
                ended: false,
                need_lines: None,
                lines: false,
                allow_channels: false,
            }
        }
        fn status(&mut self, p: &[u8]) {
            let mut r = vec![0xA5, p[1], p[2], p[3], 0, 1, STATUS_OK];
            let crc = crc16(&r[1..]);
            r.extend_from_slice(&[(crc >> 8) as u8, crc as u8]);
            self.rx.extend(r);
        }
        fn protected_regions_intact(&self) -> bool {
            let voices = self.flash[FACTORY_VOICES..ES_BASE].iter().any(|&b| b != 0xFF);
            self.flash[..0x1000].iter().all(|&b| b == 0x11)
                && self.flash[0xC000..0xD000].iter().all(|&b| b == 0x22)
                && voices
        }
    }

    impl Link for FakeRadio {
        fn send(&mut self, p: &[u8]) -> Res<()> {
            if !self.flash_mode {
                if p == b"D" {
                    assert!(self.linked);
                    self.flash_mode = true;
                    return Ok(());
                }
                assert_eq!(p, b"PROGRAMBF-K6PROU");
                if self.need_lines.map_or(true, |l| l == self.lines) {
                    self.linked = true;
                    self.rx.push_back(0x06);
                }
                return Ok(());
            }
            assert_eq!(p[0], 0xA5);
            let len = (p[4] as usize) << 8 | p[5] as usize;
            assert_eq!(p.len(), len + 8);
            assert_eq!(crc16(&p[1..6 + len]), (p[6 + len] as u16) << 8 | p[7 + len] as u16);
            let d = &p[6..6 + len];
            match p[1] {
                CMD_HANDSHAKE => self.status(p),
                CMD_ADDR => {
                    self.start = u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize;
                    self.next = 0;
                    self.status(p);
                }
                CMD_WRITE => {
                    let in_channels = self.allow_channels && self.start + len <= channels::CHANNEL_AREA;
                    assert!(self.start >= ES_BASE || in_channels, "Write at {:X}", self.start);
                    assert_eq!((p[2] as u16) << 8 | p[3] as u16, self.next);
                    for (i, b) in d.iter().enumerate() {
                        self.flash[self.start + i] &= b; // NOR flash only clears bits.
                    }
                    self.start += len;
                    self.next += 1;
                    self.status(p);
                }
                CMD_ERASE => {
                    assert_eq!(p[3], 0x02, "Only 4 KB erase is allowed; 0x01 would erase the chip");
                    let addr = u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize;
                    assert_eq!((d[4], d[5]), (0, 1));
                    let in_channels = self.allow_channels && addr + SECTOR <= channels::CHANNEL_AREA;
                    assert!((addr >= ES_BASE || in_channels) && addr % SECTOR == 0, "Erase at {addr:X}");
                    self.flash[addr..addr + SECTOR].fill(0xFF);
                    self.status(p);
                }
                CMD_READ => {
                    let addr = u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize;
                    let n = (d[4] as usize) << 8 | d[5] as usize;
                    assert!(n <= 1024 && addr + n <= FLASH_SIZE);
                    let mut r = vec![0xA5, p[1], p[2], p[3], (n >> 8) as u8, n as u8];
                    r.extend_from_slice(&self.flash[addr..addr + n]);
                    let crc = crc16(&r[1..]);
                    r.extend_from_slice(&[(crc >> 8) as u8, crc as u8]);
                    self.rx.extend(r);
                }
                CMD_END => {
                    // The real radio restarts here and leaves flash mode.
                    self.ended = true;
                    self.flash_mode = false;
                    self.linked = false;
                }
                c => panic!("Command {c:02X} must never be sent"),
            }
            Ok(())
        }
        fn recv(&mut self, buf: &mut [u8]) -> Res<usize> {
            let n = buf.len().min(self.rx.len());
            for b in buf.iter_mut().take(n) {
                *b = self.rx.pop_front().unwrap();
            }
            Ok(n)
        }
        fn clear_input(&mut self) {
            self.rx.clear();
        }
        fn set_lines(&mut self, on: bool) {
            self.lines = on;
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("k6_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn voices_install_on_an_empty_radio_and_keep_everything_else() {
        let mut radio = FakeRadio::new();
        let before = radio.flash.clone();
        let dir = temp_dir("empty");
        let path = install_voices(&mut radio, VOICES, &dir).unwrap();
        assert!(radio.ended);
        assert_eq!(&radio.flash[ES_BASE..ES_BASE + VOICES.len()], VOICES);
        assert_eq!(radio.flash[..ES_BASE], before[..ES_BASE]);
        assert!(radio.protected_regions_intact());
        assert_eq!(fs::read(path).unwrap(), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn voices_replace_previous_ones_including_a_longer_image() {
        let mut radio = FakeRadio::new();
        let mut old = vec![0xFFu8; 0x30000];
        old[..4].copy_from_slice(&0x2F000u32.to_le_bytes());
        old[4..8].copy_from_slice(&0x1000u32.to_le_bytes());
        old[ES_MAGIC_OFFSET..ES_MAGIC_OFFSET + 4].copy_from_slice(&ES_MAGIC.to_le_bytes());
        old[0x1000..].fill(0x33);
        radio.flash[ES_BASE..ES_BASE + old.len()].copy_from_slice(&old);
        let before = radio.flash.clone();
        let dir = temp_dir("replace");
        install_voices(&mut radio, VOICES, &dir).unwrap();
        assert_eq!(&radio.flash[ES_BASE..ES_BASE + VOICES.len()], VOICES);
        assert!(radio.flash[ES_BASE + VOICES.len()..ES_BASE + old.len()].iter().all(|&b| b == 0xFF));
        assert_eq!(radio.flash[..ES_BASE], before[..ES_BASE]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nothing_is_written_when_the_voices_are_already_there() {
        let mut radio = FakeRadio::new();
        radio.flash[ES_BASE..ES_BASE + VOICES.len()].copy_from_slice(VOICES);
        let backup = radio.flash.clone();
        let msg = write_voices(&mut radio, &backup, VOICES).unwrap();
        assert!(msg.contains("ya estaban"));
        assert_eq!(radio.flash, backup);
    }

    #[test]
    fn nothing_is_written_over_unknown_data_or_an_unexpected_layout() {
        let mut radio = FakeRadio::new();
        radio.flash[ES_BASE + 0x2000] = 0x55;
        let backup = radio.flash.clone();
        assert!(write_voices(&mut radio, &backup, VOICES).is_err());
        assert_eq!(radio.flash, backup);

        let mut radio = FakeRadio::new();
        radio.flash[FACTORY_VOICES..FACTORY_VOICES + 8].fill(0);
        let backup = radio.flash.clone();
        assert!(write_voices(&mut radio, &backup, VOICES).is_err());
        assert_eq!(radio.flash, backup);
    }

    #[test]
    fn the_link_tries_both_line_states() {
        for needed in [false, true] {
            let mut radio = FakeRadio::new();
            radio.need_lines = Some(needed);
            enter_flash_mode(&mut radio, MS_WAIT_BOOT).unwrap();
            assert!(radio.flash_mode);
            assert_eq!(radio.lines, needed);
        }
    }

    #[test]
    fn a_backup_changes_nothing_and_restarts_the_radio() {
        let mut radio = FakeRadio::new();
        let before = radio.flash.clone();
        let dir = temp_dir("backup");
        let path = backup_only(&mut radio, &dir, MS_WAIT_BOOT).unwrap();
        assert_eq!(radio.flash, before);
        assert_eq!(fs::read(path).unwrap(), before);
        assert!(radio.ended);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn factory_language_only_erases_spanish_voices() {
        let mut radio = FakeRadio::new();
        radio.flash[ES_BASE..ES_BASE + VOICES.len()].copy_from_slice(VOICES);
        let before = radio.flash.clone();
        let dir = temp_dir("factory");
        remove_voices(&mut radio, &dir).unwrap();
        assert!(radio.flash[ES_BASE..].iter().all(|&b| b == 0xFF));
        assert_eq!(radio.flash[..ES_BASE], before[..ES_BASE]);
        assert!(radio.protected_regions_intact());

        // A second time there is nothing to erase, and installing again restores them.
        let empty = radio.flash.clone();
        remove_voices(&mut radio, &dir).unwrap();
        assert_eq!(radio.flash, empty);
        install_voices(&mut radio, VOICES, &dir).unwrap();
        assert_eq!(&radio.flash[ES_BASE..ES_BASE + VOICES.len()], VOICES);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn channel_save_rewrites_only_changed_sectors_and_keeps_the_rest() {
        let mut radio = FakeRadio::new();
        radio.allow_channels = true;
        radio.flash[..channels::CHANNEL_AREA].fill(0xFF);
        radio.flash[0x8000..0x8100].fill(0x77); // VFO data: right after the channel area.
        radio.flash[..32].copy_from_slice(&[
            0x00, 0x25, 0x49, 0x14, 0x00, 0x25, 0x49, 0x14, 0x12, 0x34, 0x56, 0x78, 0, 0, 3, 4, 0xFF, 0xFF, 0xFF, 0xFF,
            b'A', b'W', b'J', 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        radio.flash[0x3000] = 0x11; // A sector that must not be erased.
        let before = radio.flash.clone();
        let dir = temp_dir("channels");

        let loaded = channels::read_from_radio(&mut radio).unwrap();
        let mut list = channels::parse_channels(&loaded);
        list[0].rx = 14_652_000;
        list.push(channels::Channel { number: 200, rx: 14_500_000, tx: 14_500_000, name: "NUEVO".into() });
        let wanted = channels::apply_channels(&loaded, &list).unwrap();
        let (changed, _) = channels::write_to_radio(&mut radio, &loaded, &wanted, &dir).unwrap();
        assert_eq!(changed, 2); // Channel 1 and channel 200 are in different sectors.
        assert_eq!(radio.flash[..channels::CHANNEL_AREA], wanted[..]);
        assert_eq!(radio.flash[channels::CHANNEL_AREA..], before[channels::CHANNEL_AREA..]);
        assert_eq!(radio.flash[8..20], before[8..20], "Tones and flags are kept");
        assert!(radio.ended);

        // The radio changed after reading: nothing is written over it.
        let snapshot = radio.flash.clone();
        let stale = channels::write_to_radio(&mut radio, &loaded, &wanted, &dir).unwrap_err();
        assert!(stale.contains("cambiaron"), "{stale}");
        assert_eq!(radio.flash, snapshot);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn channel_guards_refuse_everything_outside_the_channel_area() {
        let mut radio = FakeRadio::new();
        radio.allow_channels = true;
        assert!(erase_channel_sector(&mut radio, 0x8000).is_err());
        assert!(erase_channel_sector(&mut radio, 0x100).is_err());
        assert!(write_channel_range(&mut radio, 0x7FF0, &[0u8; 32]).is_err());
        assert!(write_channel_range(&mut radio, ES_BASE, &[0u8; 4]).is_err());
    }

    #[test]
    fn a_failed_install_still_restarts_the_radio() {
        let mut radio = FakeRadio::new();
        radio.flash[ES_BASE + 0x2000] = 0x55;
        let dir = temp_dir("restart");
        assert!(install_voices(&mut radio, VOICES, &dir).is_err());
        assert!(radio.ended);
        fs::remove_dir_all(dir).unwrap();
    }
}
