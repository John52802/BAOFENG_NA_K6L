//! Searching for active frequencies from the PC, with the firmware's PC mode (AppSpectrum.c).
//!
//! The radio only receives here: the protocol has no way to ask for anything else. It measures
//! the level along a sweep and sends one byte per frequency.
//!
//! The level reading lags the tuning, so a sweep upwards shows a signal too high and one
//! downwards too low. Each round sweeps both ways and takes the middle, then looks closer and
//! more slowly at the strongest signals it hasn't pinned down yet.

use crate::{channels, emit, open_link, pause, read_exact, Link, Msg, Res, MS_PAUSE, MS_REPLY, MS_WAIT_BOOT};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub static STOP: AtomicBool = AtomicBool::new(false);

pub const STEPS: [(&str, u32); 7] =
    [("5 kHz", 500), ("6.25 kHz", 625), ("10 kHz", 1000), ("12.5 kHz", 1250), ("25 kHz", 2500), ("50 kHz", 5000), ("100 kHz", 10000)];
/// How far above the usual level a signal must be, in units of the reading, and how long each
/// point of a sweep waits in ms. A weak signal hardly moves the slow reading in a short wait, so
/// the high setting is also the slow one.
pub const SENSITIVITY: [(&str, u8, u16); 3] = [("Alta", 6, 75), ("Media", 10, 25), ("Baja", 16, 25)];

// In units of 10 Hz, as in the firmware. The same limits it checks in PcSweep.
const RX_MIN: u32 = 10_000_000;
const RX_MAX: u32 = 100_000_000;
const MAX_POINTS: u32 = 8000;
const MIN_POINTS: u32 = 8;

const ACK: u8 = 0x06;
const CLOSE_DWELL_MS: u16 = 50;
const CLOSE_STEP: u32 = 1250; // 12.5 kHz.
const CLOSE_HALF: u32 = 20_000; // 200 kHz to each side.
const GRID: u32 = 500; // Exact results land on 5 kHz.
const CLOSE_PER_ROUND: usize = 3; // A closer look takes about 4 seconds.
const LAG_TYPICAL_MS: u32 = 300; // Measured on the radio: between about 200 and 500 ms.
const LAG_MOST_MS: u32 = 600;
const SAME_EXACT: u32 = 2000; // 20 kHz: what the exact search can vary between rounds.
const SAME_ROUGH: u32 = 30_000; // 300 kHz: what a sweep in one direction can be off by.
const SEND_EVERY: Duration = Duration::from_millis(500);
const MS_ENTER: u64 = if cfg!(test) { 5 } else { 4000 };
const MS_SWEEP_EXTRA: u64 = if cfg!(test) { 5 } else { 5000 };
const MS_VERSION: u64 = if cfg!(test) { 5 } else { 500 };
const TONE_WAIT_MS: u16 = 1500;
const TONES_PER_ROUND: usize = 3;
/// The firmware's PC_VERSION from which it detects tones.
const VERSION_TONES: u8 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub low: u32,
    pub step: u32,
    pub count: u16,
    pub margin: u8,
    pub dwell: u16,
}

pub fn make_plan(from: &str, to: &str, step: u32, margin: u8, dwell: u16) -> Res<Plan> {
    let low = channels::parse_mhz(from)?;
    let high = channels::parse_mhz(to)?;
    if high <= low {
        return Err("La frecuencia \"Hasta\" debe ser mayor que \"Desde\".".into());
    }
    let points = (high - low) / step + 1;
    if points < MIN_POINTS {
        return Err("El rango es muy pequeño para ese paso.".into());
    }
    if points > MAX_POINTS {
        return Err(format!("El rango tiene {points} puntos y el máximo es {MAX_POINTS}. Reduce el rango o sube el paso."));
    }
    Ok(Plan { low, step, count: points as u16, margin, dwell })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub freq: u32,
    pub level: u8,
    pub times: u32,
    pub last: String,
    /// Found by the closer look, not only estimated from the sweeps.
    pub exact: bool,
    pub tone: Tone,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tone {
    Unchecked,
    None,
    /// As a channel stores it: CTCSS in tenths of a Hz, or the DCS number from 1 to 105.
    Code(u16),
    /// A code outside the standard lists.
    Other,
}

pub fn tone_text(t: Tone) -> String {
    match t {
        Tone::Unchecked => String::new(),
        Tone::None => "Sin tono".into(),
        Tone::Code(v) => channels::format_tone(v),
        Tone::Other => "No estándar".into(),
    }
}

pub fn to_csv(hits: &[Hit]) -> String {
    let mut s = String::from("frecuencia_mhz,tono,nivel,veces,ultima_vez,exactitud\r\n");
    for h in hits {
        s.push_str(&format!(
            "{},{},{},{},{},{}\r\n",
            channels::format_mhz(h.freq),
            tone_text(h.tone),
            h.level,
            h.times,
            h.last,
            accuracy(h)
        ));
    }
    s
}

pub fn accuracy(h: &Hit) -> &'static str {
    if h.exact { "Afinada" } else { "Aproximada" }
}

/// The request for a sweep, with its checksum. See PcSweep in AppSpectrum.c.
fn sweep_frame(start: u32, step: u32, count: u16, dwell: u16, down: bool) -> Vec<u8> {
    let mut f = vec![b'S'];
    f.extend_from_slice(&start.to_le_bytes());
    f.extend_from_slice(&step.to_le_bytes());
    f.extend_from_slice(&count.to_le_bytes());
    f.extend_from_slice(&dwell.to_le_bytes());
    f.push(down as u8);
    f.push(f.iter().fold(0, |a, b| a ^ b));
    f
}

/// One sweep, returned from the lowest frequency up whichever way it went. `shown` receives what
/// has been measured so far, with zeros for the rest. None if asked to stop.
fn sweep(
    link: &mut dyn Link,
    low: u32,
    step: u32,
    count: u16,
    dwell: u16,
    down: bool,
    stop: &AtomicBool,
    shown: &mut dyn FnMut(&[u8]),
) -> Res<Option<Vec<u8>>> {
    let n = count as usize;
    let start = if down { low + step * (count as u32 - 1) } else { low };
    link.send(&sweep_frame(start, step, count, dwell, down))?;
    match read_exact(link, 1, MS_REPLY)?[0] {
        b'S' => {}
        b'N' => return Err("El radio rechazó la búsqueda.".into()),
        other => return Err(format!("Respuesta inesperada del radio: {other:#04X}")),
    }

    let mut got = vec![0u8; n];
    let mut filled = 0;
    let deadline = Instant::now() + Duration::from_millis(n as u64 * (dwell as u64 + 30) + MS_SWEEP_EXTRA);
    let mut next_show = Instant::now() + SEND_EVERY;
    while filled < n {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        filled += link.recv(&mut got[filled..])?;
        if filled < n && Instant::now() > deadline {
            return Err(format!("El radio dejó de responder ({filled} de {n} puntos)."));
        }
        if Instant::now() > next_show {
            next_show = Instant::now() + SEND_EVERY;
            shown(&ascending(&got, down));
        }
    }
    Ok(Some(ascending(&got, down)))
}

fn ascending(levels: &[u8], down: bool) -> Vec<u8> {
    let mut v = levels.to_vec();
    if down {
        v.reverse();
    }
    v
}

/// The usual level, ignoring points not measured yet.
fn median(levels: &[u8]) -> u8 {
    let mut v: Vec<u8> = levels.iter().copied().filter(|l| *l > 0).collect();
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

pub fn threshold(levels: &[u8], margin: u8) -> u8 {
    median(levels).saturating_add(margin)
}

/// The highest point of each stretch at or above the threshold.
fn peaks(levels: &[u8], limit: u8) -> Vec<usize> {
    let mut out = Vec::new();
    let mut best: Option<usize> = None;
    for (i, l) in levels.iter().enumerate() {
        if *l >= limit {
            if best.map_or(true, |b| *l > levels[b]) {
                best = Some(i);
            }
        } else if let Some(b) = best.take() {
            out.push(b);
        }
    }
    out.extend(best);
    out
}

#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    freq: u32,
    level: u8,
    /// Seen in both directions, so the lag cancels. Otherwise it's only corrected on average.
    both: bool,
}

/// Matches the peaks of the two sweeps, strongest first.
fn candidates(up: &[u8], down: &[u8], low: u32, step: u32, margin: u8, dwell: u16) -> Vec<Candidate> {
    let mut all = up.to_vec();
    all.extend_from_slice(down);
    let limit = threshold(&all, margin);
    let most = (2 * LAG_MOST_MS / dwell as u32) as usize;
    let typical = (LAG_TYPICAL_MS / dwell as u32) as usize;
    let freq_at = |i: usize| low + step * i as u32;

    let (peaks_up, peaks_down) = (peaks(up, limit), peaks(down, limit));
    let mut used = vec![false; peaks_down.len()];
    let mut out = Vec::new();
    for &u in &peaks_up {
        // A sweep upwards shows the signal higher up, so its match is at or below it.
        let found = (0..peaks_down.len())
            .filter(|&k| !used[k] && peaks_down[k] <= u + 2 && u.saturating_sub(peaks_down[k]) <= most)
            .max_by_key(|&k| peaks_down[k]);
        match found {
            Some(k) => {
                used[k] = true;
                let d = peaks_down[k];
                out.push(Candidate { freq: low + step * (u + d) as u32 / 2, level: up[u].max(down[d]), both: true });
            }
            None => out.push(Candidate { freq: freq_at(u.saturating_sub(typical)), level: up[u], both: false }),
        }
    }
    for (k, &d) in peaks_down.iter().enumerate() {
        if !used[k] {
            out.push(Candidate { freq: freq_at((d + typical).min(down.len() - 1)), level: down[d], both: false });
        }
    }
    out.sort_by(|a, b| b.level.cmp(&a.level).then(a.freq.cmp(&b.freq)));
    out
}

/// The frequency between the peaks of a slow sweep each way, if both really stand out.
fn exact_from(up: &[u8], down: &[u8], low: u32, step: u32, margin: u8) -> Option<u32> {
    let top = |levels: &[u8]| {
        let mut best = 0;
        for (i, l) in levels.iter().enumerate() {
            if *l > levels[best] {
                best = i;
            }
        }
        best
    };
    let (u, d) = (top(up), top(down));
    let needed = (margin / 2).max(3);
    if up[u] < threshold(up, needed) || down[d] < threshold(down, needed) {
        return None;
    }
    let middle = low + step * (u + d) as u32 / 2;
    Some((middle + GRID / 2) / GRID * GRID)
}

fn near(a: u32, b: u32, distance: u32) -> bool {
    a.abs_diff(b) <= distance
}

fn tone_frame(freq: u32, wait: u16) -> Vec<u8> {
    let mut f = vec![b'T'];
    f.extend_from_slice(&freq.to_le_bytes());
    f.extend_from_slice(&wait.to_le_bytes());
    f.push(f.iter().fold(0, |a, b| a ^ b));
    f
}

fn tone_check(link: &mut dyn Link, freq: u32) -> Res<Tone> {
    link.send(&tone_frame(freq, TONE_WAIT_MS))?;
    let reply = read_exact(link, 1, TONE_WAIT_MS as u64 + MS_REPLY)?;
    match reply[0] {
        b'T' => {}
        b'N' => return Err("El radio rechazó la búsqueda de tono.".into()),
        other => return Err(format!("Respuesta inesperada del radio: {other:#04X}")),
    }
    let r = read_exact(link, 3, MS_REPLY)?;
    let value = u16::from_le_bytes([r[1], r[2]]);
    Ok(match r[0] {
        1 | 2 => Tone::Code(value),
        3 => Tone::Other,
        _ => Tone::None,
    })
}

/// Keeps what is known: a tone found is kept over a later reading without one.
fn merge_tone(old: Tone, new: Tone) -> Tone {
    match (old, new) {
        (_, Tone::Code(_)) | (Tone::Unchecked, _) => new,
        (Tone::None, Tone::Other) => new,
        _ => old,
    }
}

/// Counts it on the entry for that frequency, or adds one. An exact result replaces an estimate.
/// Returns where the entry is.
fn record(hits: &mut Vec<Hit>, freq: u32, level: u8, exact: bool, now: &str) -> u32 {
    let found = hits.iter().position(|h| near(h.freq, freq, if h.exact && exact { SAME_EXACT } else { SAME_ROUGH }));
    match found {
        Some(i) => {
            let h = &mut hits[i];
            h.times += 1;
            h.level = h.level.max(level);
            h.last = now.to_string();
            if exact && !h.exact {
                h.freq = freq;
                h.exact = true;
            }
        }
        None => hits.push(Hit { freq, level, times: 1, last: now.to_string(), exact, tone: Tone::Unchecked }),
    }
    let at = found.map_or(freq, |i| hits[i].freq);
    hits.sort_by_key(|h| h.freq);
    at
}

fn set_tone(hits: &mut [Hit], at: u32, tone: Tone) {
    if let Some(h) = hits.iter_mut().find(|h| h.freq == at) {
        h.tone = merge_tone(h.tone, tone);
    }
}

/// Whether a closer look would only find again what the list already has.
fn already_exact(hits: &[Hit], c: &Candidate, step: u32) -> bool {
    let distance = if c.both { (step * 3).max(2500) } else { SAME_ROUGH };
    hits.iter().any(|h| h.exact && near(h.freq, c.freq, distance))
}

#[repr(C)]
#[derive(Default)]
struct SystemTime {
    year: u16,
    month: u16,
    weekday: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}

extern "system" {
    fn GetLocalTime(time: *mut SystemTime);
}

fn local_time() -> String {
    let mut t = SystemTime::default();
    unsafe { GetLocalTime(&mut t) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

fn close_look(link: &mut dyn Link, around: u32, margin: u8, stop: &AtomicBool) -> Res<Option<Option<u32>>> {
    let low = around.saturating_sub(CLOSE_HALF).max(RX_MIN);
    let high = (around + CLOSE_HALF).min(RX_MAX - CLOSE_STEP);
    let count = ((high - low) / CLOSE_STEP + 1) as u16;
    let mut quiet = |_: &[u8]| {};
    let Some(up) = sweep(link, low, CLOSE_STEP, count, CLOSE_DWELL_MS, false, stop, &mut quiet)? else { return Ok(None) };
    let Some(down) = sweep(link, low, CLOSE_STEP, count, CLOSE_DWELL_MS, true, stop, &mut quiet)? else { return Ok(None) };
    Ok(Some(exact_from(&up, &down, low, CLOSE_STEP, margin)))
}

/// Rounds until asked to stop.
fn rounds(link: &mut dyn Link, plan: &Plan, stop: &AtomicBool, hits: &mut Vec<Hit>, tones: bool) -> Res<()> {
    let mut round = 0u32;
    loop {
        round += 1;
        let mut show = |levels: &[u8]| emit(Msg::ScanLevels(levels.to_vec(), threshold(levels, plan.margin)));
        let Some(up) = sweep(link, plan.low, plan.step, plan.count, plan.dwell, false, stop, &mut show)? else { return Ok(()) };
        show(&up);
        let Some(down) = sweep(link, plan.low, plan.step, plan.count, plan.dwell, true, stop, &mut show)? else { return Ok(()) };
        show(&down);

        let mut looked = 0;
        let mut toned = 0;
        for c in candidates(&up, &down, plan.low, plan.step, plan.margin, plan.dwell) {
            let now = local_time();
            if already_exact(hits, &c, plan.step) {
                let at = record(hits, c.freq, c.level, false, &now);
                let unknown = hits.iter().any(|h| h.freq == at && matches!(h.tone, Tone::Unchecked | Tone::None));
                if tones && unknown && toned < TONES_PER_ROUND {
                    toned += 1;
                    if stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let tone = tone_check(link, at)?;
                    set_tone(hits, at, tone);
                    emit(Msg::ScanHits(hits.clone()));
                }
                continue;
            }
            let mut exact = None;
            if looked < CLOSE_PER_ROUND {
                looked += 1;
                say!("Vuelta {round}: revisando {} MHz...", channels::format_mhz(c.freq));
                match close_look(link, c.freq, plan.margin, stop)? {
                    Some(found) => exact = found,
                    None => return Ok(()),
                }
            }
            let at = record(hits, exact.unwrap_or(c.freq), c.level, exact.is_some(), &now);
            // Only where the closer look found the signal still there, on its exact frequency.
            if tones && exact.is_some() && !stop.load(Ordering::Relaxed) {
                toned += 1;
                say!("Vuelta {round}: buscando el tono de {} MHz...", channels::format_mhz(at));
                let tone = tone_check(link, at)?;
                set_tone(hits, at, tone);
            }
            emit(Msg::ScanHits(hits.clone()));
        }
        emit(Msg::ScanHits(hits.clone()));
        say!("Vuelta {round} terminada: {} frecuencias en la lista.", hits.len());
    }
}

/// Puts the radio in PC mode and searches until `stop` is set. The radio restarts at the end, and
/// also by itself after a few seconds if the program goes away.
pub fn run(link: &mut dyn Link, plan: &Plan, stop: &AtomicBool) -> Res<String> {
    open_link(link, MS_WAIT_BOOT)?;
    pause(MS_PAUSE);
    link.clear_input();
    link.send(b"S")?;
    let entered = read_exact(link, 1, MS_ENTER);
    if !matches!(entered.as_deref(), Ok([ACK])) {
        return Err("El radio no entró en modo PC. Instala el firmware de este programa (L1.8a o posterior) y prueba de nuevo.".into());
    }
    // L1.8a sends no version, and doesn't detect tones.
    let tones = matches!(read_exact(link, 1, MS_VERSION).as_deref(), Ok([v]) if *v >= VERSION_TONES);
    if !tones {
        say!("El firmware del radio no detecta tonos: instala el de este programa para verlos.");
    }
    say!("El radio está en modo PC. Con el cable conectado no se oye por su altavoz.");
    say!("Para escuchar una frecuencia, detén la búsqueda y desconecta el cable.");

    let mut hits = Vec::new();
    let result = rounds(link, plan, stop, &mut hits, tones);
    let _ = link.send(b"E");
    pause(MS_PAUSE);
    result?;
    Ok(format!("Búsqueda detenida. {} frecuencias en la lista. El radio se reinició.", hits.len()))
}

/// A picture of the levels: a 24-bit BMP, with a line where a signal starts to count.
pub fn graph_bmp(levels: &[u8], limit: u8, width: usize, height: usize) -> Vec<u8> {
    const BACK: [u8; 3] = [24, 28, 32];
    const BAR: [u8; 3] = [90, 200, 120];
    const STRONG: [u8; 3] = [240, 190, 70];
    const LINE: [u8; 3] = [200, 90, 80];

    let (width, height) = (width.max(1), height.max(1));
    let measured = levels.iter().copied().filter(|l| *l > 0);
    let bottom = measured.clone().min().unwrap_or(0).saturating_sub(4) as usize;
    let top = (measured.max().unwrap_or(0) as usize).max(limit as usize + 8).max(bottom + 20);
    let dots = |l: u8| (l as usize).saturating_sub(bottom) * (height - 1) / (top - bottom);
    let line = dots(limit);

    let row = (width * 3 + 3) / 4 * 4;
    let size = row * height;
    let mut out = Vec::with_capacity(54 + size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + size) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    // A BMP goes from the bottom row up, so `y` is already the height above the bottom.
    for y in 0..height {
        let start = out.len();
        for x in 0..width {
            // Several points can fall on one column: the strongest one shows.
            let mut level = 0;
            if !levels.is_empty() {
                let from = x * levels.len() / width;
                let to = ((x + 1) * levels.len() / width).max(from + 1).min(levels.len());
                level = levels[from..to].iter().copied().max().unwrap_or(0);
            }
            let color = if level > 0 && y <= dots(level) {
                if level >= limit { STRONG } else { BAR }
            } else if y == line {
                LINE
            } else {
                BACK
            };
            out.extend_from_slice(&[color[2], color[1], color[0]]);
        }
        out.resize(start + row, 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const FAST_DWELL_MS: u16 = 25;

    #[test]
    fn plans_are_checked() {
        let p = make_plan("144", "148", 1250, 10, 25).unwrap();
        assert_eq!(p, Plan { low: 14_400_000, step: 1250, count: 321, margin: 10, dwell: 25 });
        assert!(make_plan("148", "144", 1250, 10, 25).is_err());
        assert!(make_plan("144", "144.01", 1250, 10, 25).is_err());
        assert!(make_plan("100", "400", 500, 10, 25).is_err());
        assert!(make_plan("abc", "148", 1250, 10, 25).is_err());
    }

    #[test]
    fn the_limits_match_the_firmware() {
        let src = String::from_utf8_lossy(include_bytes!("../../../src/App/AppSpectrum.c")).replace(char::is_whitespace, "");
        assert!(src.contains(&format!("#definePC_MAX_POINTS{MAX_POINTS}")));
        assert!(src.contains("#definePC_FRAME_LEN15"));
        assert!(src.contains("#definePC_TONE_LEN8"));
        assert!(src.contains(&format!("#definePC_VERSION{VERSION_TONES}")));
        assert_eq!(tone_frame(14_652_000, TONE_WAIT_MS).len(), 8);
        let radio = String::from_utf8_lossy(include_bytes!("../../../src/Core/Radio.h")).replace(char::is_whitespace, "");
        assert!(radio.contains(&format!("#defineRX_MIN_100KHZ{}UL", RX_MIN / 10000)));
        assert!(radio.contains(&format!("#defineRX_MAX_100KHZ{}UL", RX_MAX / 10000)));
        assert_eq!(sweep_frame(14_400_000, 1250, 321, 25, true).len(), 15);
    }

    #[test]
    fn the_frame_has_its_checksum() {
        let f = sweep_frame(0x0102_0304, 0x0506_0708, 0x090A, 0x0B0C, true);
        assert_eq!(f[..14], [b'S', 4, 3, 2, 1, 8, 7, 6, 5, 0x0A, 9, 0x0C, 0x0B, 1]);
        assert_eq!(f[..14].iter().fold(0, |a, b| a ^ b), f[14]);
    }

    /// The radio's reading: follows the signal late, measured as about 300 ms. `real` is how
    /// strong the signal really is, over a usual level of 50.
    fn lagging(freqs: impl Iterator<Item = u32>, signal: u32, real: f32, dwell: u16) -> Vec<(u32, u8)> {
        let alpha = dwell as f32 / LAG_TYPICAL_MS as f32;
        let mut reading = 50.0;
        freqs
            .map(|f| {
                let now = if near(f, signal, 1000) { real } else { 50.0 };
                reading += (now - reading) * alpha;
                (f, reading as u8)
            })
            .collect()
    }

    fn sweeps(low: u32, step: u32, count: u32, signal: u32, real: f32, dwell: u16) -> (Vec<u8>, Vec<u8>) {
        let freqs = |i: u32| low + step * i;
        let up: Vec<u8> = lagging((0..count).map(freqs), signal, real, dwell).iter().map(|p| p.1).collect();
        let mut down: Vec<u8> = lagging((0..count).rev().map(freqs), signal, real, dwell).iter().map(|p| p.1).collect();
        down.reverse();
        (up, down)
    }

    #[test]
    fn the_lag_cancels_between_the_two_directions() {
        let signal = 14_652_000;
        // A radio nearby, as in the measurements.
        let (up, down) = sweeps(14_400_000, 1250, 321, signal, 170.0, FAST_DWELL_MS);
        let found = candidates(&up, &down, 14_400_000, 1250, 10, FAST_DWELL_MS);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].both);
        assert!(near(found[0].freq, signal, 2500), "{}", found[0].freq);

        let (up, down) = sweeps(signal - CLOSE_HALF, CLOSE_STEP, 33, signal, 170.0, CLOSE_DWELL_MS);
        let exact = exact_from(&up, &down, signal - CLOSE_HALF, CLOSE_STEP, 10).unwrap();
        assert!(near(exact, signal, 1250), "{exact}");
        assert_eq!(exact % GRID, 0);
    }

    #[test]
    fn the_high_sensitivity_finds_what_the_fast_sweep_misses() {
        let signal = 14_652_000;
        let weak = 75.0;
        let [(_, high_margin, high_dwell), (_, medium_margin, medium_dwell), _] = SENSITIVITY;
        let (up, down) = sweeps(14_400_000, 1250, 321, signal, weak, medium_dwell);
        assert_eq!(candidates(&up, &down, 14_400_000, 1250, medium_margin, medium_dwell), []);
        let (up, down) = sweeps(14_400_000, 1250, 321, signal, weak, high_dwell);
        let found = candidates(&up, &down, 14_400_000, 1250, high_margin, high_dwell);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(near(found[0].freq, signal, 2500), "{}", found[0].freq);
    }

    #[test]
    fn a_signal_in_one_direction_only_is_corrected_on_average() {
        let mut up = vec![50u8; 100];
        up[60] = 100;
        let found = candidates(&up, &vec![50u8; 100], 14_400_000, 1250, 10, FAST_DWELL_MS);
        assert_eq!(found, [Candidate { freq: 14_400_000 + 1250 * 48, level: 100, both: false }]);
        assert_eq!(candidates(&vec![50u8; 100], &vec![50u8; 100], 14_400_000, 1250, 10, FAST_DWELL_MS), []);
        assert_eq!(exact_from(&vec![50u8; 33], &vec![50u8; 33], 14_400_000, 1250, 10), None);
    }

    #[test]
    fn hits_merge_and_an_exact_result_replaces_an_estimate() {
        let mut hits = Vec::new();
        record(&mut hits, 14_660_000, 90, false, "a");
        record(&mut hits, 14_652_000, 80, true, "b");
        assert_eq!(hits, [Hit { freq: 14_652_000, level: 90, times: 2, last: "b".into(), exact: true, tone: Tone::Unchecked }]);
        record(&mut hits, 14_652_500, 70, true, "c");
        assert_eq!(hits.len(), 1);
        record(&mut hits, 14_600_000, 70, true, "d");
        assert_eq!(hits.iter().map(|h| h.freq).collect::<Vec<_>>(), [14_600_000, 14_652_000]);
        assert!(already_exact(&hits, &Candidate { freq: 14_652_500, level: 1, both: true }, 1250));
        assert!(!already_exact(&hits, &Candidate { freq: 14_500_000, level: 1, both: true }, 1250));
        set_tone(&mut hits, 14_652_000, Tone::Code(885));
        set_tone(&mut hits, 14_652_000, Tone::None);
        assert_eq!(hits[1].tone, Tone::Code(885));
        assert!(to_csv(&hits).contains("146.52,88.5,90,3,c,Afinada\r\n"));
        assert_eq!(merge_tone(Tone::Unchecked, Tone::None), Tone::None);
        assert_eq!(merge_tone(Tone::None, Tone::Other), Tone::Other);
        assert_eq!(merge_tone(Tone::Other, Tone::None), Tone::Other);
    }

    struct FakeRadio {
        sent: Vec<Vec<u8>>,
        rx: VecDeque<u8>,
        answer: u8,
        supported: bool,
        version: Option<u8>,
    }

    impl Link for FakeRadio {
        fn send(&mut self, data: &[u8]) -> Res<()> {
            self.sent.push(data.to_vec());
            match data[0] {
                b'P' => self.rx.push_back(ACK),
                b'S' if data.len() == 1 && self.supported => {
                    self.rx.push_back(ACK);
                    self.rx.extend(self.version);
                }
                b'T' if data.len() == 8 => self.rx.extend([b'T', 2, 23, 0]),
                b'S' if data.len() == 15 => {
                    self.rx.push_back(self.answer);
                    if self.answer == b'S' {
                        let count = u16::from_le_bytes([data[9], data[10]]);
                        self.rx.extend((0..count).map(|i| 50 + (i % 3) as u8));
                    }
                }
                _ => {}
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
        fn set_lines(&mut self, _on: bool) {}
    }

    fn radio(answer: u8, supported: bool) -> FakeRadio {
        FakeRadio { sent: Vec::new(), rx: VecDeque::new(), answer, supported, version: Some(VERSION_TONES) }
    }

    #[test]
    fn a_tone_comes_back_as_a_channel_stores_it() {
        let mut r = radio(b'S', true);
        assert_eq!(tone_check(&mut r, 14_652_000).unwrap(), Tone::Code(23));
        let f = &r.sent[0];
        assert_eq!(f[..7], [b'T', 0x60, 0x92, 0xDF, 0x00, 0xDC, 0x05]);
        assert_eq!(f[..7].iter().fold(0, |a, b| a ^ b), f[7]);
        assert_eq!(tone_text(Tone::Code(23)), channels::format_tone(23));
    }

    #[test]
    fn firmware_without_tones_still_searches() {
        let plan = make_plan("144", "145", 10000, 10, 25).unwrap();
        let mut r = radio(b'S', true);
        r.version = None;
        assert!(run(&mut r, &plan, &AtomicBool::new(true)).is_ok());
    }

    #[test]
    fn a_sweep_downwards_comes_back_in_order() {
        let mut r = radio(b'S', true);
        let levels = sweep(&mut r, 14_400_000, 1250, 4, 25, true, &AtomicBool::new(false), &mut |_| {}).unwrap().unwrap();
        assert_eq!(levels, [50, 52, 51, 50]);
        assert_eq!(r.sent[0][1..5], (14_400_000u32 + 3750).to_le_bytes());
        assert_eq!(r.sent[0][13], 1);
    }

    #[test]
    fn a_refused_or_stopped_sweep_says_so() {
        let stop = AtomicBool::new(false);
        assert!(sweep(&mut radio(b'N', true), 14_400_000, 1250, 4, 25, false, &stop, &mut |_| {}).is_err());
        stop.store(true, Ordering::Relaxed);
        assert_eq!(sweep(&mut radio(b'S', true), 14_400_000, 1250, 4, 25, false, &stop, &mut |_| {}).unwrap(), None);
    }

    #[test]
    fn old_firmware_is_told_apart_and_stopping_ends_the_session() {
        let plan = make_plan("144", "145", 10000, 10, 25).unwrap();
        let stop = AtomicBool::new(true);
        let e = run(&mut radio(b'S', false), &plan, &stop).unwrap_err();
        assert!(e.contains("firmware"), "{e}");

        let mut r = radio(b'S', true);
        assert!(run(&mut r, &plan, &stop).is_ok());
        assert_eq!(r.sent.last().unwrap(), b"E");
    }

    #[test]
    fn the_picture_has_the_size_it_says() {
        let bmp = graph_bmp(&[50, 60, 0, 120], 70, 201, 50);
        assert_eq!(bmp.len(), 54 + (201 * 3 + 3) / 4 * 4 * 50);
        assert_eq!(graph_bmp(&[], 0, 10, 10).len(), 54 + 32 * 10);
    }
}
