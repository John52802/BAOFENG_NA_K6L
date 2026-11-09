//! The FM radio's stations: STR_FMINFOS in src/Common/PublType.h.
//!
//! Like the settings, they live in a rotating log with a checksum at 0xC000, so they go through
//! the firmware's programming mode, where writing that address calls Flash_SaveFmData. The radio
//! keeps only frequencies, without names.

use crate::settings::{self, ADDR_FM, BLOCK_SIZE};
use crate::{emit, ensure_backup_dir, utc_stamp, Link, Msg, Res};
use std::fs;
use std::path::{Path, PathBuf};

pub const MAX_STATIONS: usize = 30; // FM_MAX_CH_NUM.
const STATIONS_AT: usize = 2; // FmCHs, after FmCurFreq.
// In tenths of a MHz: the range FmCheckChannelActive in src/App/AppFm.c accepts.
const FREQ_MIN: u16 = 650;
const FREQ_MAX: u16 = 1080;

/// The stations the radio treats as valid, in the order it holds them.
pub fn parse(data: &[u8]) -> Vec<u16> {
    (0..MAX_STATIONS)
        .map(|i| u16::from_le_bytes([data[STATIONS_AT + i * 2], data[STATIONS_AT + i * 2 + 1]]))
        .filter(|f| (FREQ_MIN..=FREQ_MAX).contains(f))
        .collect()
}

pub fn format_freq(v: u16) -> String {
    format!("{}.{}", v / 10, v % 10)
}

fn parse_freq(text: &str) -> Res<u16> {
    let t = text.trim().replace(',', ".");
    let bad = || format!("Emisora no válida: \"{}\". Debe ser como 88.9, entre 65.0 y 108.0 MHz.", text.trim());
    let (whole, frac) = t.split_once('.').unwrap_or((&t, "0"));
    if whole.is_empty() || frac.len() != 1 || !whole.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let tenths = whole.parse::<u16>().ok().and_then(|w| w.checked_mul(10)).and_then(|w| w.checked_add(frac.parse().ok()?));
    tenths.filter(|v| (FREQ_MIN..=FREQ_MAX).contains(v)).ok_or_else(bad)
}

/// One station per line. Also reads a CSV from `to_csv`, or any list with the frequency in its
/// last column, and spreadsheets' ';'. Empty lines are skipped, and the header too.
pub fn parse_list(text: &str) -> Res<Vec<u16>> {
    let mut out: Vec<u16> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || (i == 0 && line.to_lowercase().starts_with("emisora")) {
            continue;
        }
        let value = if line.contains(';') {
            line.rsplit(';').next().unwrap_or("")
        } else if line.matches(',').count() == 1 && line.split(',').nth(1).is_some_and(|d| d.trim().len() == 1) {
            line // A decimal comma: 88,9.
        } else {
            line.rsplit(',').next().unwrap_or("")
        };
        let freq = parse_freq(value)?;
        if out.contains(&freq) {
            return Err(format!("La emisora {} está repetida.", format_freq(freq)));
        }
        out.push(freq);
    }
    if out.len() > MAX_STATIONS {
        return Err(format!("El radio guarda {MAX_STATIONS} emisoras como máximo, y la lista tiene {}.", out.len()));
    }
    Ok(out)
}

pub fn to_text(list: &[u16]) -> String {
    list.iter().map(|f| format_freq(*f)).collect::<Vec<_>>().join("\r\n")
}

pub fn to_csv(list: &[u16]) -> String {
    let mut s = String::from("emisora,frecuencia_mhz\r\n");
    for (i, f) in list.iter().enumerate() {
        s.push_str(&format!("{},{}\r\n", i + 1, format_freq(*f)));
    }
    s
}

/// Writes the list over what was read. An unchanged list changes no bytes at all, and only the
/// stations ever change: the current frequency and mode stay.
pub fn apply(loaded: &[u8], list: &[u16]) -> Res<Vec<u8>> {
    if loaded.len() != BLOCK_SIZE || list.len() > MAX_STATIONS {
        return Err("Datos de FM de tamaño inesperado".into());
    }
    if parse(loaded) == list {
        return Ok(loaded.to_vec());
    }
    let mut out = loaded.to_vec();
    for i in 0..MAX_STATIONS {
        let value = list.get(i).copied().unwrap_or(0);
        out[STATIONS_AT + i * 2..STATIONS_AT + i * 2 + 2].copy_from_slice(&value.to_le_bytes());
    }
    Ok(out)
}

pub fn read_from_radio(link: &mut dyn Link) -> Res<Vec<u8>> {
    say!("Leyendo emisoras de FM...");
    settings::enter(link)?;
    let result = settings::read_block(link, ADDR_FM);
    settings::leave(link);
    result
}

/// Keeps a copy, writes, then restarts the radio and reads again to check what it really saved.
pub fn write_to_radio(link: &mut dyn Link, loaded: &[u8], wanted: &[u8], dir: &Path) -> Res<(Vec<u8>, PathBuf)> {
    if loaded.len() != BLOCK_SIZE || wanted.len() != BLOCK_SIZE {
        return Err("Datos de FM de tamaño inesperado".into());
    }
    say!("Comprobando el radio...");
    settings::enter(link)?;
    let written = (|| {
        let current = settings::read_block(link, ADDR_FM)?;
        if current != loaded {
            return Err("Las emisoras del radio cambiaron desde que las leíste. Léelas de nuevo.".to_string());
        }
        ensure_backup_dir(dir)?;
        let path = dir.join(format!("Baofeng_K6_fm_{}.bin", utc_stamp()));
        fs::write(&path, &current).map_err(|e| format!("No se pudo guardar la copia: {e}"))?;
        say!("Copia de las emisoras: {}", path.display());
        say!("Guardando emisoras...");
        settings::write_block(link, ADDR_FM, wanted)?;
        Ok(path)
    })();
    settings::leave(link);
    let path = written?;

    say!("Reiniciando el radio para comprobar lo guardado...");
    settings::enter(link)?;
    let check = settings::read_block(link, ADDR_FM);
    settings::leave(link);
    let check = check?;
    if check != wanted {
        return Err(format!("El radio no conservó las emisoras al reiniciar. La copia anterior está en {}", path.display()));
    }
    Ok((check, path))
}

pub fn publish_read(data: Vec<u8>) {
    emit(Msg::FmRead(data));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real record: current frequency 88.9, no stations.
    fn real() -> Vec<u8> {
        let mut d = vec![0u8; BLOCK_SIZE];
        d[0..2].copy_from_slice(&889u16.to_le_bytes());
        d
    }

    #[test]
    fn layout_matches_the_firmware() {
        let src = String::from_utf8_lossy(include_bytes!("../../../src/Common/PublType.h")).replace(char::is_whitespace, "");
        assert!(src.contains("U16FmCurFreq;"));
        assert!(src.contains(&format!("U16FmCHs[{MAX_STATIONS}];")));
        assert!(src.contains("U8fmChNum;"));
        let fm = String::from_utf8_lossy(include_bytes!("../../../src/App/AppFm.c")).replace(char::is_whitespace, "");
        assert!(fm.contains(&format!("g_FMInform.FmCHs[i]>={FREQ_MIN}&&g_FMInform.FmCHs[i]<={FREQ_MAX}")));
    }

    #[test]
    fn lists_are_read_in_the_ways_people_write_them() {
        assert_eq!(parse_list("88.9\r\n96,5\r\n\r\n100\r\n").unwrap(), [889, 965, 1000]);
        assert_eq!(parse_list("emisora,frecuencia_mhz\r\n1,88.9\r\n2,96.5\r\n").unwrap(), [889, 965]);
        assert_eq!(parse_list("\u{feff}emisora;frecuencia_mhz\r\n1;96,5\r\n").unwrap(), [965]);
        assert_eq!(parse_list("Caracol,96.5\n").unwrap(), [965]);
        assert_eq!(parse_list("").unwrap(), Vec::<u16>::new());
        for bad in ["abc", "50", "120", "88.95", "88.9\n88.9", "-88.9"] {
            assert!(parse_list(bad).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..31).map(|i| format!("{}.0", 70 + i)).collect();
        assert!(parse_list(&many.join("\n")).is_err());
    }

    #[test]
    fn a_list_round_trips_and_only_stations_change() {
        let mut d = real();
        d[62] = 3; // fmChNum.
        d[63] = 1; // fmChVfo.
        let out = apply(&d, &[889, 965]).unwrap();
        assert_eq!(out[..2], d[..2]);
        assert_eq!(out[2..6], [0x79, 0x03, 0xC5, 0x03]);
        assert_eq!(out[6..62], [0; 56]);
        assert_eq!(out[62..], [3, 1]);
        assert_eq!(parse(&out), [889, 965]);
        assert_eq!(parse_list(&to_text(&parse(&out))).unwrap(), [889, 965]);
        assert_eq!(parse_list(&to_csv(&parse(&out))).unwrap(), [889, 965]);

        // Removing them works, and an unchanged list leaves the bytes alone, gaps included.
        assert_eq!(parse(&apply(&out, &[]).unwrap()), Vec::<u16>::new());
        let mut gap = real();
        gap[6..8].copy_from_slice(&965u16.to_le_bytes());
        assert_eq!(apply(&gap, &[965]).unwrap(), gap);
    }

    #[test]
    fn stored_values_outside_the_range_are_not_stations() {
        let mut d = real();
        d[2..4].copy_from_slice(&0xFFFFu16.to_le_bytes());
        d[4..6].copy_from_slice(&1000u16.to_le_bytes());
        assert_eq!(parse(&d), [1000]);
    }
}
