//! Radio channels: the format in flash, validation, CSV and writing back to the radio.
//!
//! Each channel takes 32 bytes from flash address 0: RX and TX frequency as little endian
//! BCD in 10 Hz units, tones and flags, then a 12-byte name at offset 20. Fields this
//! program does not edit (tones, power, bandwidth...) are kept exactly as they were.

use crate::{
    emit, enter_flash_mode, erase_channel_sector, leave_flash_mode, read_range, write_channel_range, Link, Msg, Res,
    MS_WAIT_BOOT, SECTOR,
};
use std::fs;
use std::path::{Path, PathBuf};

pub const CHANNEL_COUNT: usize = 999;
pub const CHANNEL_SIZE: usize = 32;
const NAME_OFFSET: usize = 20;
const NAME_SIZE: usize = 12;
/// All channel data lives below this address. Nothing else is ever written here.
pub const CHANNEL_AREA: usize = 0x8000;
const FLAGS_OFFSET: usize = 15;
const DEFAULT_FLAGS: u8 = 0x06; // Same as the channels the radio saves itself.

// Receive range of the rx-spectrum firmware, in 10 Hz units.
const FREQ_MIN: u32 = 10_000_000;
const FREQ_MAX: u32 = 100_000_000;

#[derive(Clone, PartialEq, Debug)]
pub struct Channel {
    pub number: usize, // Starting at 1, as the radio shows it.
    pub rx: u32,
    pub tx: u32,
    pub name: String,
}

fn from_bcd(b: &[u8]) -> Option<u32> {
    let mut v: u32 = 0;
    for &x in b.iter().rev() {
        if x >> 4 > 9 || x & 15 > 9 {
            return None;
        }
        v = v.checked_mul(100)?.checked_add((x >> 4) as u32 * 10 + (x & 15) as u32)?;
    }
    Some(v)
}

fn to_bcd(mut v: u32) -> [u8; 4] {
    let mut o = [0u8; 4];
    for b in &mut o {
        let pair = (v % 100) as u8;
        *b = (pair / 10) << 4 | pair % 10;
        v /= 100;
    }
    o
}

/// Formats 10 Hz units as MHz with no needless zeros: 14550000 becomes 145.5.
pub fn format_mhz(v: u32) -> String {
    let s = format!("{}.{:05}", v / 100_000, v % 100_000);
    let t = s.trim_end_matches('0');
    if t.ends_with('.') { format!("{t}0") } else { t.to_string() }
}

/// Parses MHz without going through floating point, which would lose digits.
pub fn parse_mhz(text: &str) -> Res<u32> {
    let t = text.trim().replace(',', ".");
    let bad = || format!("Frecuencia no válida: \"{text}\"");
    let (whole, frac) = t.split_once('.').unwrap_or((&t, ""));
    if whole.is_empty() || frac.len() > 5 || !whole.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let mut digits = frac.to_string();
    digits.extend(std::iter::repeat('0').take(5 - frac.len()));
    let w: u32 = whole.parse().map_err(|_| bad())?;
    let f: u32 = digits.parse().map_err(|_| bad())?;
    let v = w.checked_mul(100_000).and_then(|x| x.checked_add(f)).ok_or_else(bad)?;
    if !(FREQ_MIN..FREQ_MAX).contains(&v) {
        return Err(format!("\"{text}\" está fuera de 100 a 999,99999 MHz"));
    }
    Ok(v)
}

fn parse_name(raw: &[u8]) -> String {
    raw.iter().take_while(|&&b| b != 0xFF && b != 0).map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '?' }).collect()
}

pub fn check_name(name: &str) -> Res<()> {
    if name.len() > NAME_SIZE || !name.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Err(format!("Nombre no válido: \"{name}\". Máximo 12 letras, sin tildes ni ñ."));
    }
    Ok(())
}

/// Lists the channels the radio treats as active.
pub fn parse_channels(area: &[u8]) -> Vec<Channel> {
    let mut out = Vec::new();
    for i in 0..CHANNEL_COUNT {
        let c = &area[i * CHANNEL_SIZE..(i + 1) * CHANNEL_SIZE];
        // Same test as the radio: see ChannelCheckActiveAll in src/Core/Radio.c.
        if c[0] == 0xFF || c[3] == 0 {
            continue;
        }
        if let (Some(rx), Some(tx)) = (from_bcd(&c[0..4]), from_bcd(&c[4..8])) {
            out.push(Channel { number: i + 1, rx, tx, name: parse_name(&c[NAME_OFFSET..NAME_OFFSET + NAME_SIZE]) });
        }
    }
    out
}

/// Applies an edited list over the channel area read from the radio.
pub fn apply_channels(area: &[u8], channels: &[Channel]) -> Res<Vec<u8>> {
    let old = parse_channels(area);
    let mut out = area.to_vec();
    for c in channels {
        if c.number == 0 || c.number > CHANNEL_COUNT {
            return Err(format!("Número de canal no válido: {}", c.number));
        }
        if !(FREQ_MIN..FREQ_MAX).contains(&c.rx) || !(FREQ_MIN..FREQ_MAX).contains(&c.tx) {
            return Err(format!("Canal {}: frecuencia fuera de rango", c.number));
        }
        check_name(&c.name)?;
    }
    for o in &old {
        if !channels.iter().any(|c| c.number == o.number) {
            let at = (o.number - 1) * CHANNEL_SIZE;
            out[at..at + CHANNEL_SIZE].fill(0xFF); // Deleted.
        }
    }
    for c in channels {
        let at = (c.number - 1) * CHANNEL_SIZE;
        match old.iter().find(|o| o.number == c.number) {
            Some(o) if o == c => continue,
            Some(_) => {}
            None => {
                // A new channel starts from what the radio itself would store.
                out[at..at + CHANNEL_SIZE].fill(0);
                out[at + FLAGS_OFFSET] = DEFAULT_FLAGS;
            }
        }
        out[at..at + 4].copy_from_slice(&to_bcd(c.rx));
        out[at + 4..at + 8].copy_from_slice(&to_bcd(c.tx));
        let name = &mut out[at + NAME_OFFSET..at + NAME_OFFSET + NAME_SIZE];
        name.fill(0xFF);
        name[..c.name.len()].copy_from_slice(c.name.as_bytes());
    }
    Ok(out)
}

pub fn to_csv(channels: &[Channel]) -> String {
    let mut s = String::from("canal,rx_mhz,tx_mhz,nombre\r\n");
    for c in channels {
        s.push_str(&format!("{},{},{},{}\r\n", c.number, format_mhz(c.rx), format_mhz(c.tx), c.name));
    }
    s
}

/// Reads the format written by `to_csv`. Spreadsheets may use ';' as the separator.
pub fn from_csv(text: &str) -> Res<Vec<Channel>> {
    let mut out: Vec<Channel> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || (i == 0 && line.to_lowercase().starts_with("canal")) {
            continue;
        }
        let sep = if line.contains(';') { ';' } else { ',' };
        let f: Vec<&str> = line.split(sep).map(str::trim).collect();
        let row = i + 1;
        if f.len() < 3 || f.len() > 4 {
            return Err(format!("Fila {row}: se esperaban canal, RX, TX y nombre"));
        }
        let number: usize = f[0].parse().map_err(|_| format!("Fila {row}: canal no válido \"{}\"", f[0]))?;
        if number == 0 || number > CHANNEL_COUNT {
            return Err(format!("Fila {row}: el canal debe ser de 1 a {CHANNEL_COUNT}"));
        }
        if out.iter().any(|c| c.number == number) {
            return Err(format!("Fila {row}: el canal {number} está repetido"));
        }
        let rx = parse_mhz(f[1]).map_err(|e| format!("Fila {row}: {e}"))?;
        let tx = if f[2].is_empty() { rx } else { parse_mhz(f[2]).map_err(|e| format!("Fila {row}: {e}"))? };
        let name = f.get(3).copied().unwrap_or("").to_string();
        check_name(&name).map_err(|e| format!("Fila {row}: {e}"))?;
        out.push(Channel { number, rx, tx, name });
    }
    out.sort_by_key(|c| c.number);
    Ok(out)
}

pub fn read_from_radio(link: &mut dyn Link) -> Res<Vec<u8>> {
    say!("Leyendo canales...");
    enter_flash_mode(link, MS_WAIT_BOOT)?;
    let result = read_range(link, 0, CHANNEL_AREA, "Lectura de canales");
    leave_flash_mode(link);
    result
}

/// Saves a copy of the current channels, then rewrites only the sectors that changed.
/// Reads the radio again first, so changes made on the radio since are never lost silently.
pub fn write_to_radio(link: &mut dyn Link, loaded: &[u8], wanted: &[u8], dir: &Path) -> Res<(usize, PathBuf)> {
    if wanted.len() != CHANNEL_AREA || loaded.len() != CHANNEL_AREA {
        return Err("Datos de canales de tamaño inesperado".into());
    }
    say!("Comprobando el radio...");
    enter_flash_mode(link, MS_WAIT_BOOT)?;
    let result = (|| {
        let current = read_range(link, 0, CHANNEL_AREA, "Lectura de canales")?;
        if current != loaded {
            return Err("Los canales del radio cambiaron desde que los leíste. Léelos de nuevo.".to_string());
        }
        crate::ensure_backup_dir(dir)?;
        let path = dir.join(format!("Baofeng_K6_canales_{}.bin", crate::utc_stamp()));
        fs::write(&path, &current).map_err(|e| format!("No se pudo guardar la copia: {e}"))?;
        say!("Copia de los canales: {}", path.display());

        let changed: Vec<usize> =
            (0..CHANNEL_AREA).step_by(SECTOR).filter(|&a| current[a..a + SECTOR] != wanted[a..a + SECTOR]).collect();
        for (n, &addr) in changed.iter().enumerate() {
            emit(Msg::Progress("Guardando canales".into(), n as f32 / changed.len() as f32));
            let new = &wanted[addr..addr + SECTOR];
            erase_channel_sector(link, addr)?;
            write_channel_range(link, addr, new)?;
            let check = read_range(link, addr, SECTOR, "Verificando")?;
            if check != new {
                return Err(format!(
                    "Zona 0x{addr:04X} no coincide al leerla. Vuelve a guardar; la copia está en {}",
                    path.display()
                ));
            }
        }
        Ok((changed.len(), path))
    })();
    leave_flash_mode(link);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Vec<u8> {
        let mut a = vec![0xFFu8; CHANNEL_AREA];
        // Channel 1, exactly as found in a real backup: 144.925 MHz, name "AWJverse".
        a[..32].copy_from_slice(&[
            0x00, 0x25, 0x49, 0x14, 0x00, 0x25, 0x49, 0x14, 0x12, 0x34, 0x56, 0x78, 0x00, 0x00, 0x03, 0x04, 0xFF, 0xFF,
            0xFF, 0xFF, b'A', b'W', b'J', b'v', b'e', b'r', b's', b'e', 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        a
    }

    #[test]
    fn reads_a_real_channel() {
        let c = parse_channels(&area());
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].number, c[0].rx, c[0].tx), (1, 14_492_500, 14_492_500));
        assert_eq!(c[0].name, "AWJverse");
        assert_eq!(format_mhz(c[0].rx), "144.925");
    }

    #[test]
    fn frequencies_are_parsed_exactly() {
        assert_eq!(parse_mhz("145.5").unwrap(), 14_550_000);
        assert_eq!(parse_mhz("118,3").unwrap(), 11_830_000);
        assert_eq!(parse_mhz("446.00625").unwrap(), 44_600_625);
        assert_eq!(parse_mhz("100").unwrap(), 10_000_000);
        for bad in ["", "abc", "99.9", "1000", "145.123456", "1.2.3", "-145", "145.5x"] {
            assert!(parse_mhz(bad).is_err(), "{bad}");
        }
        for v in [14_492_500, 44_600_625, 10_000_000, 99_999_999] {
            assert_eq!(parse_mhz(&format_mhz(v)).unwrap(), v);
            assert_eq!(from_bcd(&to_bcd(v)).unwrap(), v);
        }
    }

    #[test]
    fn untouched_channels_stay_byte_identical() {
        let a = area();
        assert_eq!(apply_channels(&a, &parse_channels(&a)).unwrap(), a);
    }

    #[test]
    fn editing_keeps_the_other_fields() {
        let a = area();
        let mut list = parse_channels(&a);
        list[0].rx = parse_mhz("146.52").unwrap();
        list[0].name = "PRUEBA".into();
        let out = apply_channels(&a, &list).unwrap();
        assert_eq!(out[8..20], a[8..20], "Tones, power and flags must not change");
        assert_eq!(out[4..8], a[4..8], "TX was not edited");
        let again = parse_channels(&out);
        assert_eq!(again[0].rx, 14_652_000);
        assert_eq!(again[0].name, "PRUEBA");
        assert_eq!(&out[28..32], &[0xFF; 4], "A shorter name leaves no old letters behind");
    }

    #[test]
    fn adding_and_deleting_channels() {
        let a = area();
        let new = Channel { number: 5, rx: 14_500_000, tx: 14_500_000, name: "NUEVO".into() };
        let out = apply_channels(&a, &[new.clone()]).unwrap();
        assert_eq!(parse_channels(&out), vec![new]);
        assert!(out[..32].iter().all(|&b| b == 0xFF), "Channel 1 was removed from the list");
        assert_eq!(out[4 * 32 + FLAGS_OFFSET], DEFAULT_FLAGS);
    }

    #[test]
    fn invalid_edits_are_refused() {
        let a = area();
        let ok = Channel { number: 2, rx: 14_500_000, tx: 14_500_000, name: String::new() };
        for bad in [
            Channel { number: 0, ..ok.clone() },
            Channel { number: 1000, ..ok.clone() },
            Channel { rx: 5_000_000, ..ok.clone() },
            Channel { tx: 100_000_000, ..ok.clone() },
            Channel { name: "Demasiado largo".into(), ..ok.clone() },
            Channel { name: "Señal".into(), ..ok.clone() },
        ] {
            assert!(apply_channels(&a, &[bad.clone()]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn csv_round_trip_and_spreadsheet_format() {
        let list = vec![
            Channel { number: 1, rx: 14_492_500, tx: 14_492_500, name: "AWJverse".into() },
            Channel { number: 7, rx: 43_152_500, tx: 44_152_500, name: String::new() },
        ];
        assert_eq!(from_csv(&to_csv(&list)).unwrap(), list);
        let excel = "\u{feff}canal;rx_mhz;tx_mhz;nombre\r\n2;145,5;;Repetidor\r\n";
        assert_eq!(
            from_csv(excel).unwrap(),
            vec![Channel { number: 2, rx: 14_550_000, tx: 14_550_000, name: "Repetidor".into() }]
        );
        for bad in ["1,145.5", "abc,145,145", "1,145.5,145.5\n1,146,146", "0,145,145", "1,50,50"] {
            assert!(from_csv(bad).is_err(), "{bad}");
        }
    }
}
