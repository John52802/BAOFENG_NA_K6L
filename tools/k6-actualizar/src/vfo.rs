//! The A and B frequency-mode (VFO) records: STR_VFOMODE in src/Common/PublType.h.
//!
//! Like the settings, they live in a rotating log with a checksum at 0x8000, so they go through
//! the firmware's programming mode, where writing 0x8000 calls Flash_SaveVfoData. Only the fields
//! edited here change; anything else in a record is kept as it was.

use crate::channels::{self, format_mhz, parse_mhz};
use crate::settings::{self, ADDR_VFO, BLOCK_SIZE};
use crate::{emit, ensure_backup_dir, utc_stamp, Link, Msg, Res};
use std::fs;
use std::path::{Path, PathBuf};

pub const VFO_COUNT: usize = 2;
const RECORD: usize = 32;
const FREQ_DIGITS: usize = 8;
const RX_TONE: usize = 8;
const TX_TONE: usize = 10;
const DIR_AT: usize = 14; // dtmfgroup: the direction is its top bits, see S_SFTD in AppMenu.c.
const DIR_SHIFT: u8 = 5;
const POWER_AT: usize = 16;
const FLAGS_AT: usize = 17;
const NARROW_BIT: u8 = 6;
const LEARNED_CODE: u8 = 0x01;
const STEP_AT: usize = 19;
const OFFSET_AT: usize = 20;
const OFFSET_DIGITS: usize = 7;
const MUTE_AT: usize = 27;
// Stored in 100 Hz units, and this program works in 10 Hz units like the channels.
const OFFSET_UNIT: u32 = 10;
const OFFSET_MAX: u32 = 99_999_990;

// In the order of the radio's menu.
pub const DIRECTIONS: &[&str] = &["Sin desplaz.", "+", "-"];
pub const POWERS: &[&str] = &["Alta", "Baja"];
pub const WIDTHS: &[&str] = &["Ancha", "Estrecha"];
pub const STEPS: &[&str] = &["2.5 kHz", "5 kHz", "6.25 kHz", "10 kHz", "12.5 kHz", "20 kHz", "25 kHz", "50 kHz"];
pub const MUTES: &[&str] = &["Tono", "Tono y señal"];

#[derive(Clone, PartialEq, Debug)]
pub struct Vfo {
    pub freq: u32,
    pub rx_tone: u16,
    pub tx_tone: u16,
    pub direction: u8,
    pub offset: u32,
    pub power: u8,
    pub narrow: u8,
    pub step: u8,
    pub mute: u8,
}

fn from_digits(d: &[u8]) -> Option<u32> {
    d.iter().try_fold(0u32, |v, &x| (x <= 9).then(|| v * 10 + x as u32))
}

fn to_digits(mut v: u32, out: &mut [u8]) {
    for d in out.iter_mut().rev() {
        *d = (v % 10) as u8;
        v /= 10;
    }
}

/// The two records. A frequency that isn't valid digits reads as 0, which shows as empty.
pub fn parse(data: &[u8]) -> Vec<Vfo> {
    data.chunks_exact(RECORD)
        .take(VFO_COUNT)
        .map(|r| Vfo {
            freq: from_digits(&r[..FREQ_DIGITS]).unwrap_or(0),
            rx_tone: u16::from_le_bytes([r[RX_TONE], r[RX_TONE + 1]]),
            tx_tone: u16::from_le_bytes([r[TX_TONE], r[TX_TONE + 1]]),
            direction: r[DIR_AT] >> DIR_SHIFT,
            offset: from_digits(&r[OFFSET_AT..OFFSET_AT + OFFSET_DIGITS]).map_or(0, |v| v * OFFSET_UNIT),
            power: r[POWER_AT] & 0x0F,
            narrow: (r[FLAGS_AT] >> NARROW_BIT) & 1,
            step: r[STEP_AT],
            mute: r[MUTE_AT] & 3,
        })
        .collect()
}

pub fn format_freq(v: u32) -> String {
    if v == 0 { String::new() } else { format_mhz(v) }
}

pub fn format_offset(v: u32) -> String {
    format_mhz(v)
}

/// An offset in MHz, down to the 100 Hz the radio stores. Empty means none.
pub fn parse_offset(text: &str) -> Res<u32> {
    let t = text.trim().replace(',', ".");
    let bad = || format!("Desplazamiento no válido: \"{text}\". Por ejemplo 0.6, con 4 decimales como máximo.");
    if t.is_empty() {
        return Ok(0);
    }
    let (whole, frac) = t.split_once('.').unwrap_or((&t, ""));
    if whole.is_empty() || frac.len() > 4 || !whole.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let mut digits = frac.to_string();
    digits.push_str(&"0".repeat(5 - frac.len()));
    let w: u32 = whole.parse().map_err(|_| bad())?;
    let f: u32 = digits.parse().map_err(|_| bad())?;
    let v = w.checked_mul(100_000).and_then(|x| x.checked_add(f)).ok_or_else(bad)?;
    if v > OFFSET_MAX {
        return Err(bad());
    }
    Ok(v)
}

fn check_choice(line: &str, label: &str, value: u8, old: u8, options: &[&str]) -> Res<()> {
    if value as usize >= options.len() && value != old {
        return Err(format!("VFO {line}: valor no válido en {label}"));
    }
    Ok(())
}

/// Applies the edited records over what was read. Fields not edited here stay byte for byte.
pub fn apply(loaded: &[u8], vfos: &[Vfo]) -> Res<Vec<u8>> {
    if loaded.len() != BLOCK_SIZE || vfos.len() != VFO_COUNT {
        return Err("Datos de VFO de tamaño inesperado".into());
    }
    let old = parse(loaded);
    let mut out = loaded.to_vec();
    for (i, (v, o)) in vfos.iter().zip(&old).enumerate() {
        let line = ["A", "B"][i];
        let r = &mut out[i * RECORD..(i + 1) * RECORD];
        if v.freq != o.freq {
            // Goes through the same check as a typed frequency.
            parse_mhz(&format_mhz(v.freq)).map_err(|e| format!("VFO {line}: {e}"))?;
            to_digits(v.freq, &mut r[..FREQ_DIGITS]);
        }
        for (label, new, before, at) in [("Tono RX", v.rx_tone, o.rx_tone, RX_TONE), ("Tono TX", v.tx_tone, o.tx_tone, TX_TONE)] {
            if new != before {
                if !channels::is_standard_tone(new) {
                    return Err(format!("VFO {line}: {label} \"{}\" no es un tono estándar", channels::format_tone(new)));
                }
                r[at..at + 2].copy_from_slice(&new.to_le_bytes());
                // Same as choosing a tone on the radio: see SaveRadioCtcss in src/App/AppMenu.c.
                r[FLAGS_AT] &= !LEARNED_CODE;
            }
        }
        check_choice(line, "Dirección", v.direction, o.direction, DIRECTIONS)?;
        check_choice(line, "Potencia", v.power, o.power, POWERS)?;
        check_choice(line, "Ancho", v.narrow, o.narrow, WIDTHS)?;
        check_choice(line, "Paso", v.step, o.step, STEPS)?;
        check_choice(line, "Silencio", v.mute, o.mute, MUTES)?;
        if v.offset != o.offset {
            if v.offset % OFFSET_UNIT != 0 || v.offset > OFFSET_MAX {
                return Err(format!("VFO {line}: desplazamiento no válido"));
            }
            to_digits(v.offset / OFFSET_UNIT, &mut r[OFFSET_AT..OFFSET_AT + OFFSET_DIGITS]);
        }
        r[DIR_AT] = (r[DIR_AT] & 0x1F) | (v.direction << DIR_SHIFT);
        r[POWER_AT] = (r[POWER_AT] & 0xF0) | (v.power & 0x0F);
        r[FLAGS_AT] = (r[FLAGS_AT] & !(1 << NARROW_BIT)) | ((v.narrow & 1) << NARROW_BIT);
        r[STEP_AT] = v.step;
        r[MUTE_AT] = (r[MUTE_AT] & !3) | (v.mute & 3);
    }
    Ok(out)
}

pub fn read_from_radio(link: &mut dyn Link) -> Res<Vec<u8>> {
    say!("Leyendo VFO...");
    settings::enter(link)?;
    let result = settings::read_block(link, ADDR_VFO);
    settings::leave(link);
    result
}

/// Keeps a copy, writes, then restarts the radio and reads again to check what it really saved.
pub fn write_to_radio(link: &mut dyn Link, loaded: &[u8], wanted: &[u8], dir: &Path) -> Res<(Vec<u8>, PathBuf)> {
    if loaded.len() != BLOCK_SIZE || wanted.len() != BLOCK_SIZE {
        return Err("Datos de VFO de tamaño inesperado".into());
    }
    say!("Comprobando el radio...");
    settings::enter(link)?;
    let written = (|| {
        let current = settings::read_block(link, ADDR_VFO)?;
        if current != loaded {
            return Err("Los VFO del radio cambiaron desde que los leíste. Léelos de nuevo.".to_string());
        }
        ensure_backup_dir(dir)?;
        let path = dir.join(format!("Baofeng_K6_vfo_{}.bin", utc_stamp()));
        fs::write(&path, &current).map_err(|e| format!("No se pudo guardar la copia: {e}"))?;
        say!("Copia de los VFO: {}", path.display());
        say!("Guardando VFO...");
        settings::write_block(link, ADDR_VFO, wanted)?;
        Ok(path)
    })();
    settings::leave(link);
    let path = written?;

    say!("Reiniciando el radio para comprobar lo guardado...");
    settings::enter(link)?;
    let check = settings::read_block(link, ADDR_VFO);
    settings::leave(link);
    let check = check?;
    if check != wanted {
        return Err(format!("El radio no conservó los VFO al reiniciar. La copia anterior está en {}", path.display()));
    }
    Ok((check, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Two records from a real radio: 435.625 and 145.525 MHz.
    fn real() -> Vec<u8> {
        let mut d = vec![0u8; BLOCK_SIZE];
        d[..8].copy_from_slice(&[4, 3, 5, 6, 2, 5, 0, 0]);
        d[18] = 1;
        d[32..40].copy_from_slice(&[1, 4, 5, 5, 2, 5, 0, 0]);
        d
    }

    #[test]
    fn real_records_are_read() {
        let v = parse(&real());
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].freq, 43_562_500);
        assert_eq!(v[1].freq, 14_552_500);
        assert_eq!(format_freq(v[0].freq), "435.625");
        assert_eq!((v[0].direction, v[0].offset, v[0].power, v[0].narrow, v[0].step, v[0].mute), (0, 0, 0, 0, 0, 0));
        assert_eq!(apply(&real(), &v).unwrap(), real(), "No change, no bytes changed");
    }

    #[test]
    fn layout_matches_the_firmware_struct() {
        let src = String::from_utf8_lossy(include_bytes!("../../../src/Common/PublType.h")).into_owned();
        let end = src.find("}STR_VFOMODE").unwrap();
        let start = src[..end].rfind("typedef struct").unwrap();
        let mut at = 0;
        let mut seen = Vec::new();
        for line in src[start..end].lines().skip(1) {
            let code = line.split("//").next().unwrap().trim();
            let Some((kind, rest)) = code.split_once(char::is_whitespace) else { continue };
            let width = match kind {
                "U8" | "VfoParaFlag1" => 1,
                "U16" => 2,
                "U32" => 4,
                _ => continue,
            };
            let rest = rest.split(';').next().unwrap().trim();
            let (name, count) = match rest.split_once('[') {
                Some((n, c)) => (n.trim().to_string(), c.trim_end_matches(']').parse::<usize>().unwrap()),
                None => (rest.to_string(), 1),
            };
            seen.push((name, at));
            at += width * count;
        }
        assert_eq!(at, RECORD);
        let offset = |n: &str| seen.iter().find(|(s, _)| s == n).unwrap().1;
        assert_eq!(offset("rxDCSCTSNum"), RX_TONE);
        assert_eq!(offset("txDCSCTSNum"), TX_TONE);
        assert_eq!(offset("dtmfgroup"), DIR_AT);
        assert_eq!(offset("txPower"), POWER_AT);
        assert_eq!(offset("vfoFlag"), FLAGS_AT);
        assert_eq!(offset("STEP"), STEP_AT);
        assert_eq!(offset("Offset"), OFFSET_AT);
        assert_eq!(offset("spMute"), MUTE_AT);

        // Same choices as the radio's menu.
        let menu = String::from_utf8_lossy(include_bytes!("../../../src/App/AppMenu.c")).into_owned();
        for (case, n) in [("S_SFTD", DIRECTIONS.len()), ("S_STEP", STEPS.len()), ("S_WN", WIDTHS.len()), ("S_TXPR", POWERS.len()), ("S_SPMUTE", MUTES.len())] {
            let at = menu.find(&format!("case {case}:")).unwrap();
            let body = &menu[at..at + menu[at..].find("break;").unwrap()];
            assert!(body.contains(&format!("subMaxItem = {n};")), "{case}");
        }
    }

    #[test]
    fn editing_writes_the_radio_format_and_keeps_the_rest() {
        let mut d = real();
        d[14] = 0b0001_0101; // DTMF group, below the direction.
        d[15] = 0x77; // ANI.
        d[16] = 0x20; // Scrambler.
        d[17] = 0b0000_0011; // Learned code flag and another bit.
        d[28..32].copy_from_slice(&[1, 2, 3, 4]); // Learned code.
        let mut v = parse(&d);
        v[0].freq = parse_mhz("145.5").unwrap();
        v[0].direction = 2;
        v[0].offset = parse_offset("0.6").unwrap();
        v[0].power = 1;
        v[0].narrow = 1;
        v[0].step = 6;
        v[0].mute = 1;
        let out = apply(&d, &v).unwrap();
        assert_eq!(out[..8], [1, 4, 5, 5, 0, 0, 0, 0]);
        assert_eq!(out[14], 0b0101_0101);
        assert_eq!(out[15], 0x77);
        assert_eq!(out[16], 0x21);
        assert_eq!(out[17], 0b0100_0011, "Tones untouched: the learned code stays on");
        assert_eq!(out[19], 6);
        assert_eq!(out[20..27], [0, 0, 0, 6, 0, 0, 0], "0.6 MHz in 100 Hz units");
        assert_eq!(out[27], 1);
        assert_eq!(out[28..32], [1, 2, 3, 4]);
        assert_eq!(out[32..], d[32..], "Line B stays");
        assert_eq!(parse(&out)[0], v[0]);
    }

    #[test]
    fn a_new_tone_clears_the_learned_code_like_the_radio() {
        let mut d = real();
        d[17] = 0b0100_0001;
        let mut v = parse(&d);
        v[1].rx_tone = 885;
        assert_eq!(apply(&d, &v).unwrap()[17], 0b0100_0001, "Line A wasn't edited");
        let mut d2 = real();
        d2[32 + 17] = 0b0100_0001;
        let mut v = parse(&d2);
        v[1].rx_tone = 885;
        let out = apply(&d2, &v).unwrap();
        assert_eq!(out[32 + 8..32 + 10], [0x75, 0x03]);
        assert_eq!(out[32 + 17], 0b0100_0000);
    }

    #[test]
    fn invalid_edits_are_refused_and_odd_stored_values_stay() {
        let d = real();
        let ok = parse(&d);
        let mut cases = Vec::new();
        for f in [|v: &mut Vfo| v.freq = 5_000_000, |v: &mut Vfo| v.freq = 100_000_000, |v: &mut Vfo| v.rx_tone = 643, |v: &mut Vfo| v.tx_tone = 230, |v: &mut Vfo| v.direction = 3, |v: &mut Vfo| v.power = 2, |v: &mut Vfo| v.step = 8, |v: &mut Vfo| v.mute = 2, |v: &mut Vfo| v.offset = 5] {
            let mut v = ok.clone();
            f(&mut v[0]);
            cases.push(v);
        }
        for bad in cases {
            assert!(apply(&d, &bad).is_err(), "{:?}", bad[0]);
        }

        let mut odd = real();
        odd[19] = 8; // A step the firmware has but the menu doesn't offer.
        odd[8..10].copy_from_slice(&643u16.to_le_bytes());
        assert_eq!(apply(&odd, &parse(&odd)).unwrap(), odd);
    }

    #[test]
    fn offsets_are_parsed_exactly() {
        assert_eq!(parse_offset("").unwrap(), 0);
        assert_eq!(parse_offset("0.6").unwrap(), 60_000);
        assert_eq!(parse_offset("5").unwrap(), 500_000);
        assert_eq!(parse_offset("7,6").unwrap(), 760_000);
        assert_eq!(parse_offset("0.0125").unwrap(), 1_250);
        assert_eq!(parse_offset("999.9999").unwrap(), OFFSET_MAX);
        for bad in ["abc", "0.12345", "1000", "-1", "1.2.3"] {
            assert!(parse_offset(bad).is_err(), "{bad}");
        }
        assert_eq!(format_offset(60_000), "0.6");
        assert_eq!(parse_offset(&format_offset(760_000)).unwrap(), 760_000);
    }

    /// The firmware's programming mode for this one address.
    struct FakeRadio {
        ram: Vec<u8>,
        saved: Vec<u8>,
        out: VecDeque<u8>,
        writes: Vec<u16>,
        restarts: usize,
        forgets: bool,
    }

    impl Link for FakeRadio {
        fn send(&mut self, p: &[u8]) -> Res<()> {
            if p.starts_with(b"PROGRAM") {
                self.out.push_back(0x06);
                return Ok(());
            }
            match p[0] {
                b'F' => self.out.extend([0u8; 16]),
                b'R' => {
                    assert_eq!(u16::from_be_bytes([p[1], p[2]]), ADDR_VFO);
                    self.out.extend(&p[..4]);
                    self.out.extend(&self.ram);
                }
                b'W' => {
                    let addr = u16::from_be_bytes([p[1], p[2]]);
                    self.writes.push(addr);
                    self.ram.copy_from_slice(&p[4..]);
                    if !self.forgets {
                        self.saved = self.ram.clone();
                    }
                    self.out.push_back(0x06);
                }
                b'E' => {
                    self.out.push_back(0x06);
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

    fn radio() -> FakeRadio {
        FakeRadio { ram: real(), saved: real(), out: VecDeque::new(), writes: vec![], restarts: 0, forgets: false }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("k6_vfo_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn saving_writes_one_block_and_checks_after_a_restart() {
        let mut r = radio();
        let loaded = read_from_radio(&mut r).unwrap();
        assert_eq!(loaded, real());
        let mut v = parse(&loaded);
        v[0].power = 1;
        let wanted = apply(&loaded, &v).unwrap();
        let dir = temp_dir("save");
        let (check, copy) = write_to_radio(&mut r, &loaded, &wanted, &dir).unwrap();
        assert_eq!(check, wanted);
        assert_eq!(r.saved, wanted);
        assert_eq!(r.writes, [ADDR_VFO]);
        assert_eq!(r.restarts, 3);
        assert_eq!(fs::read(copy).unwrap(), loaded);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_radio_that_changed_or_forgot_is_reported() {
        let mut r = radio();
        let loaded = read_from_radio(&mut r).unwrap();
        r.ram[0] = 1;
        r.saved[0] = 1;
        let dir = temp_dir("changed");
        assert!(write_to_radio(&mut r, &loaded, &real(), &dir).is_err());
        assert!(r.writes.is_empty());

        let mut r = radio();
        r.forgets = true;
        let loaded = read_from_radio(&mut r).unwrap();
        let mut v = parse(&loaded);
        v[0].power = 1;
        let wanted = apply(&loaded, &v).unwrap();
        let e = write_to_radio(&mut r, &loaded, &wanted, &dir).unwrap_err();
        assert!(e.contains("no conservó"), "{e}");
        fs::remove_dir_all(dir).unwrap();
    }
}
