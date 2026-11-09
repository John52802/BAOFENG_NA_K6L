//! Putting a saved copy back, and whole radio profiles.
//!
//! Every write goes through the same module as editing by hand, so each one keeps its own copy
//! first and checks what the radio saved. Nothing here can touch calibration.

use crate::{channels, fm, logo, settings, vfo, Link, Res, FLASH_SIZE, SECTOR};
use std::path::Path;

const FULL_LOGO_AT: usize = 0xC_0000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Channels,
    Settings,
    Vfo,
    Fm,
    Logo,
    /// A whole flash copy. Only its channels and logo can be used: settings and VFO are stored
    /// there in the radio's rotating logs, and its calibration must never travel back.
    FullFlash,
}

/// Works out what a copy holds from its size, which each kind has exactly, and its name where
/// the size alone could mislead.
pub fn classify(name: &str, len: usize) -> Res<Kind> {
    let name = name.to_lowercase();
    let kind = match len {
        channels::CHANNEL_AREA => Kind::Channels,
        settings::SETTINGS_SIZE => Kind::Settings,
        // VFO and FM copies have the same size, so here the name decides.
        settings::BLOCK_SIZE if name.contains("_fm_") => Kind::Fm,
        settings::BLOCK_SIZE => Kind::Vfo,
        SECTOR => Kind::Logo,
        FLASH_SIZE => Kind::FullFlash,
        _ => return Err(format!("Este archivo no es una copia conocida ({len} bytes).")),
    };
    let expected = match kind {
        Kind::Channels => "canales",
        Kind::Settings => "ajustes",
        Kind::Vfo => "vfo",
        Kind::Fm => "fm",
        Kind::Logo => "logo",
        Kind::FullFlash => "flash",
    };
    if !name.contains(expected) {
        return Err(format!("El tamaño es de una copia de {expected}, pero el nombre no lo dice. Por seguridad no se usa."));
    }
    Ok(kind)
}

fn restore_channels(link: &mut dyn Link, data: &[u8], dir: &Path) -> Res<String> {
    let loaded = channels::read_from_radio(link)?;
    if loaded == data {
        return Ok("Los canales ya eran iguales a la copia.".into());
    }
    channels::write_to_radio(link, &loaded, data, dir)?;
    Ok("Canales restaurados y verificados.".into())
}

fn restore_logo(link: &mut dyn Link, screen: &[u8], dir: &Path) -> Res<String> {
    let loaded = logo::read_from_radio(link)?;
    if loaded[..logo::SCREEN_SIZE] == *screen {
        return Ok("El logo ya era igual a la copia.".into());
    }
    logo::write_to_radio(link, &loaded, screen, dir)?;
    Ok("Logo restaurado y verificado.".into())
}

pub fn restore(link: &mut dyn Link, kind: Kind, data: &[u8], dir: &Path) -> Res<String> {
    match kind {
        Kind::Channels => restore_channels(link, data, dir),
        Kind::Settings => {
            let loaded = settings::read_from_radio(link)?;
            if loaded == data {
                return Ok("Los ajustes ya eran iguales a la copia.".into());
            }
            settings::write_to_radio(link, &loaded, data, dir)?;
            Ok("Ajustes restaurados y comprobados tras reiniciar el radio.".into())
        }
        Kind::Vfo => {
            let loaded = vfo::read_from_radio(link)?;
            if loaded == data {
                return Ok("Los VFO ya eran iguales a la copia.".into());
            }
            vfo::write_to_radio(link, &loaded, data, dir)?;
            Ok("VFO restaurados y comprobados tras reiniciar el radio.".into())
        }
        Kind::Fm => {
            let loaded = fm::read_from_radio(link)?;
            if loaded == data {
                return Ok("Las emisoras ya eran iguales a la copia.".into());
            }
            fm::write_to_radio(link, &loaded, data, dir)?;
            Ok("Emisoras restauradas y comprobadas tras reiniciar el radio.".into())
        }
        Kind::Logo => restore_logo(link, &data[..logo::SCREEN_SIZE], dir),
        Kind::FullFlash => {
            let first = restore_channels(link, &data[..channels::CHANNEL_AREA], dir)?;
            let second = restore_logo(link, &data[FULL_LOGO_AT..FULL_LOGO_AT + logo::SCREEN_SIZE], dir)?;
            Ok(format!("{first}\n{second}\nDe una copia completa solo se usan los canales y el logo."))
        }
    }
}

const SECTION_CHANNELS: &str = "[canales]";
const SECTION_VFO: &str = "[vfo]";
const SECTION_SETTINGS: &str = "[ajustes]";
const SECTION_FM: &str = "[fm]"; // Optional: profiles saved before it existed don't have it.

/// What a radio holds, as read from it.
pub struct RadioData {
    pub channels: Vec<u8>,
    pub vfo: Vec<u8>,
    pub settings: Vec<u8>,
    pub fm: Vec<u8>,
}

pub fn read_all(link: &mut dyn Link) -> Res<RadioData> {
    let channels = channels::read_from_radio(link)?;
    let vfo = vfo::read_from_radio(link)?;
    let settings = settings::read_from_radio(link)?;
    let fm = fm::read_from_radio(link)?;
    Ok(RadioData { channels, vfo, settings, fm })
}

/// A profile: the three CSV formats in one text file. It never holds the password.
pub fn to_profile(data: &RadioData) -> String {
    let choices: Vec<Option<usize>> = settings::FIELDS.iter().map(|f| settings::selected(&data.settings, f)).collect();
    format!(
        "{SECTION_CHANNELS}\r\n{}{SECTION_VFO}\r\n{}{SECTION_SETTINGS}\r\n{}{SECTION_FM}\r\n{}",
        channels::to_csv(&channels::parse_channels(&data.channels)),
        vfo::to_csv(&vfo::parse(&data.vfo)),
        settings::to_full_csv(&choices, &settings::message(&data.settings), &settings::scan_range(&data.settings)),
        fm::to_csv(&fm::parse(&data.fm)),
    )
}

fn sections(text: &str) -> Res<(String, String, String, Option<String>)> {
    let mut parts = [String::new(), String::new(), String::new(), String::new()];
    let mut seen = [false; 4];
    let mut current = None;
    for line in text.lines() {
        let clean = line.trim().trim_start_matches('\u{feff}');
        let at = [SECTION_CHANNELS, SECTION_VFO, SECTION_SETTINGS, SECTION_FM].iter().position(|s| clean.eq_ignore_ascii_case(s));
        if let Some(i) = at {
            if seen[i] {
                return Err(format!("El perfil repite la sección {clean}."));
            }
            seen[i] = true;
            current = Some(i);
        } else if let Some(i) = current {
            parts[i].push_str(clean);
            parts[i].push('\n');
        } else if !clean.is_empty() {
            return Err("Este archivo no es un perfil: debe empezar con [canales].".into());
        }
    }
    if seen[..3] != [true; 3] {
        return Err("El perfil debe tener las secciones [canales], [vfo] y [ajustes].".into());
    }
    let [a, b, c, d] = parts;
    Ok((a, b, c, seen[3].then_some(d)))
}

/// What to write for a profile over what the radio holds. Checks the whole file before anything
/// is written, so a mistake in its last line can't leave a radio half changed.
pub fn plan_profile(text: &str, now: &RadioData) -> Res<RadioData> {
    let (channel_text, vfo_text, settings_text, fm_text) = sections(text)?;
    let list = channels::from_csv(&channel_text, &channels::parse_channels(&now.channels)).map_err(|e| format!("Canales: {e}"))?;
    let new_channels = channels::apply_channels(&now.channels, &list).map_err(|e| format!("Canales: {e}"))?;
    let vfos = vfo::from_csv(&vfo_text, &vfo::parse(&now.vfo)).map_err(|e| format!("VFO: {e}"))?;
    let new_vfo = vfo::apply(&now.vfo, &vfos).map_err(|e| format!("VFO: {e}"))?;
    let (choices, message) = settings::from_csv(&settings_text).map_err(|e| format!("Ajustes: {e}"))?;
    let shown = message.unwrap_or_else(|| settings::message(&now.settings));
    let applied = settings::apply(&now.settings, &choices, &shown).map_err(|e| format!("Ajustes: {e}"))?;
    let scan = settings::scan_from_csv(&settings_text).unwrap_or_else(|| settings::scan_range(&now.settings));
    let new_settings = settings::apply_scan_range(&applied, &scan).map_err(|e| format!("Ajustes: {e}"))?;
    let new_fm = match fm_text {
        Some(list) => {
            let list = fm::parse_list(&list).map_err(|e| format!("FM: {e}"))?;
            fm::apply(&now.fm, &list).map_err(|e| format!("FM: {e}"))?
        }
        None => now.fm.clone(),
    };
    Ok(RadioData { channels: new_channels, vfo: new_vfo, settings: new_settings, fm: new_fm })
}

pub fn write_changes(link: &mut dyn Link, now: &RadioData, wanted: &RadioData, dir: &Path) -> Res<String> {
    let mut done = Vec::new();
    if wanted.channels != now.channels {
        channels::write_to_radio(link, &now.channels, &wanted.channels, dir)?;
        done.push("canales");
    }
    if wanted.vfo != now.vfo {
        vfo::write_to_radio(link, &now.vfo, &wanted.vfo, dir)?;
        done.push("VFO");
    }
    if wanted.settings != now.settings {
        settings::write_to_radio(link, &now.settings, &wanted.settings, dir)?;
        done.push("ajustes");
    }
    if wanted.fm != now.fm {
        fm::write_to_radio(link, &now.fm, &wanted.fm, dir)?;
        done.push("emisoras de FM");
    }
    Ok(if done.is_empty() {
        "El radio ya tenía ese perfil.".to_string()
    } else {
        format!("Perfil aplicado y comprobado: {}.\nLas copias de lo anterior están en backups.", done.join(", "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn radio() -> RadioData {
        let mut c = vec![0xFFu8; channels::CHANNEL_AREA];
        let new = channels::Channel { number: 3, rx: 14_652_000, tx: 14_652_000, name: "VHF".into(), rx_tone: 885, ..Default::default() };
        c = channels::apply_channels(&c, &[new]).unwrap();
        let mut v = vec![0u8; settings::BLOCK_SIZE];
        v[..8].copy_from_slice(&[1, 4, 5, 5, 2, 5, 0, 0]);
        v[32..40].copy_from_slice(&[4, 3, 5, 6, 2, 5, 0, 0]);
        let mut s = vec![0u8; settings::SETTINGS_SIZE];
        s[0] = 3;
        s[44..46].copy_from_slice(&400u16.to_le_bytes());
        s[46..48].copy_from_slice(&470u16.to_le_bytes());
        s[60] = 1; // Password on.
        s[61..64].copy_from_slice(&[0x12, 0x34, 0x56]);
        s[64..68].copy_from_slice(b"HOLA");
        let mut f = vec![0u8; settings::BLOCK_SIZE];
        f[2..4].copy_from_slice(&889u16.to_le_bytes());
        RadioData { channels: c, vfo: v, settings: s, fm: f }
    }

    #[test]
    fn copies_are_recognized_by_size_and_name() {
        assert_eq!(classify("Baofeng_K6_canales_1.bin", channels::CHANNEL_AREA).unwrap(), Kind::Channels);
        assert_eq!(classify("Baofeng_K6_ajustes_1.bin", settings::SETTINGS_SIZE).unwrap(), Kind::Settings);
        assert_eq!(classify("Baofeng_K6_vfo_1.bin", settings::BLOCK_SIZE).unwrap(), Kind::Vfo);
        assert_eq!(classify("Baofeng_K6_fm_1.bin", settings::BLOCK_SIZE).unwrap(), Kind::Fm);
        assert_eq!(classify("Baofeng_K6_logo_1.bin", SECTOR).unwrap(), Kind::Logo);
        assert_eq!(classify("Baofeng_K6_flash_backup.bin", FLASH_SIZE).unwrap(), Kind::FullFlash);
        // A firmware file, say, is neither the right size nor the right name.
        assert!(classify("bf_uvk6.bin", 80_000).is_err());
        assert!(classify("otro.bin", channels::CHANNEL_AREA).is_err());
        assert!(classify("Baofeng_K6_ajustes_1.bin", channels::CHANNEL_AREA).is_err());
    }

    #[test]
    fn a_profile_of_the_same_radio_changes_nothing() {
        let now = radio();
        let text = to_profile(&now);
        let wanted = plan_profile(&text, &now).unwrap();
        assert_eq!(wanted.channels, now.channels);
        assert_eq!(wanted.vfo, now.vfo);
        assert_eq!(wanted.settings, now.settings);
        assert_eq!(wanted.fm, now.fm);
    }

    #[test]
    fn a_profile_carries_the_stations_and_old_profiles_leave_them_alone() {
        let source = radio();
        let text = to_profile(&source);
        let mut other = radio();
        other.fm[2..4].copy_from_slice(&965u16.to_le_bytes());
        assert_eq!(fm::parse(&plan_profile(&text, &other).unwrap().fm), [889]);

        let old = &text[..text.find("[fm]").unwrap()];
        assert_eq!(plan_profile(old, &other).unwrap().fm, other.fm);
        let bad = format!("{old}[fm]\r\n1,50\r\n");
        assert!(plan_profile(&bad, &other).is_err());
    }

    #[test]
    fn a_profile_carries_to_another_radio_without_its_private_parts() {
        let source = radio();
        let text = to_profile(&source);
        assert!(!text.contains("123456") && !text.contains("pwr"), "No password in a profile");

        let mut other = radio();
        other.settings[0] = 7;
        other.settings[61..64].copy_from_slice(&[9, 9, 9]);
        other.settings[44..46].copy_from_slice(&118u16.to_le_bytes());
        other.vfo[0..8].copy_from_slice(&[1, 1, 8, 3, 0, 0, 0, 0]);
        other.channels[..32].fill(0xFF);
        let wanted = plan_profile(&text, &other).unwrap();
        assert_eq!(wanted.settings[0], 3);
        assert_eq!(settings::scan_range(&wanted.settings), "400-470");
        assert_eq!(wanted.settings[61..64], [9, 9, 9], "Its own password stays");
        assert_eq!(vfo::parse(&wanted.vfo)[0].freq, 14_552_500);
        assert_eq!(channels::parse_channels(&wanted.channels), channels::parse_channels(&source.channels));
    }

    #[test]
    fn a_bad_profile_is_refused_as_a_whole() {
        let now = radio();
        let good = to_profile(&now);
        for bad in [
            String::new(),
            "1,145,145,X\n".to_string(),
            good.replace("[vfo]", "[otra]"),
            good.replace("[ajustes]", "[vfo]"),
            good.replace("400-470", "470-400"),
            good.replace("146.52,146.52,VHF", "50,50,VHF"),
            good.replace("435.625", "1"),
            format!("{good}sqlLevel,Silenciador,99,\r\n"),
        ] {
            assert!(plan_profile(&bad, &now).is_err(), "{bad}");
        }
    }
}
