//! The power-on logo: 128x64 dots in external flash, drawn by UI_DisplayPowerOn in
//! src/Gui/DisplayPowerOn.c. The screen is 8 rows of 128 bytes, and in each byte bit 0 is the
//! top dot; a set bit is a dark dot.

use crate::{enter_flash_mode, ensure_backup_dir, leave_flash_mode, read_range, send_erase, send_write, utc_stamp};
use crate::{emit, Link, Msg, Res, MS_WAIT_BOOT, SECTOR};
use std::fs;
use std::path::{Path, PathBuf};

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;
pub const SCREEN_SIZE: usize = 1024;
const LOGO_ADDR: usize = 0xC_0000; // FLASH_PON_MSG_ADDR in src/Driver/FlashDataMap.h.

pub fn dot(screen: &[u8], x: usize, y: usize) -> bool {
    screen[x + y / 8 * WIDTH] >> (y % 8) & 1 != 0
}

fn set_dot(screen: &mut [u8], x: usize, y: usize) {
    screen[x + y / 8 * WIDTH] |= 1 << (y % 8);
}

/// Size of an image scaled to fill the screen without changing its shape.
pub fn fit_size(w: u32, h: u32) -> (u32, u32) {
    let (sw, sh) = (WIDTH as u64, HEIGHT as u64);
    let (w64, h64) = (w.max(1) as u64, h.max(1) as u64);
    if w64 * sh >= h64 * sw {
        (sw as u32, ((h64 * sw / w64) as u32).max(1))
    } else {
        (((w64 * sh / h64) as u32).max(1), sh as u32)
    }
}

/// Places a scaled image, centered, on a white screen of brightness values.
pub fn center_on_white(gray: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![255u8; WIDTH * HEIGHT];
    let (left, top) = (WIDTH.saturating_sub(w) / 2, HEIGHT.saturating_sub(h) / 2);
    for y in 0..h.min(HEIGHT) {
        for x in 0..w.min(WIDTH) {
            out[(top + y) * WIDTH + left + x] = gray[y * w + x];
        }
    }
    out
}

/// Turns brightness values into screen bytes. Dithering suits photos; without it, lines stay clean.
pub fn to_screen(gray: &[u8], invert: bool, dither: bool) -> Vec<u8> {
    let mut screen = vec![0u8; SCREEN_SIZE];
    let mut level: Vec<i32> = gray.iter().map(|&g| if invert { 255 - g as i32 } else { g as i32 }).collect();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let at = y * WIDTH + x;
            let dark = level[at] < 128;
            if dark {
                set_dot(&mut screen, x, y);
            }
            if dither {
                // Floyd-Steinberg: passes the rounding error on to the neighbours.
                let error = level[at] - if dark { 0 } else { 255 };
                let mut spread = |dx: i32, dy: usize, part: i32| {
                    let nx = x as i32 + dx;
                    if (0..WIDTH as i32).contains(&nx) && y + dy < HEIGHT {
                        level[(y + dy) * WIDTH + nx as usize] += error * part / 16;
                    }
                };
                spread(1, 0, 7);
                spread(-1, 1, 3);
                spread(0, 1, 5);
                spread(1, 1, 1);
            }
        }
    }
    screen
}

/// A 24-bit BMP of the screen, enlarged `scale` times, in the given colors (red, green, blue).
pub fn to_bmp(screen: &[u8], scale: usize, dark: [u8; 3], light: [u8; 3]) -> Vec<u8> {
    let (w, h) = (WIDTH * scale, HEIGHT * scale);
    let row = (w * 3 + 3) / 4 * 4;
    let size = row * h;
    let mut out = Vec::with_capacity(54 + size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + size) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for y in (0..h).rev() {
        let start = out.len();
        for x in 0..w {
            let [r, g, b] = if dot(screen, x / scale, y / scale) { dark } else { light };
            out.extend_from_slice(&[b, g, r]);
        }
        out.resize(start + row, 0);
    }
    out
}

pub fn read_from_radio(link: &mut dyn Link) -> Res<Vec<u8>> {
    say!("Leyendo el logo...");
    enter_flash_mode(link, MS_WAIT_BOOT)?;
    let result = read_range(link, LOGO_ADDR, SECTOR, "Lectura del logo");
    leave_flash_mode(link);
    result
}

/// `loaded` is the whole flash sector. Only the screen part changes; the rest of it is kept.
pub fn write_to_radio(link: &mut dyn Link, loaded: &[u8], screen: &[u8], dir: &Path) -> Res<PathBuf> {
    if loaded.len() != SECTOR || screen.len() != SCREEN_SIZE {
        return Err("Datos del logo de tamaño inesperado".into());
    }
    let mut wanted = loaded.to_vec();
    wanted[..SCREEN_SIZE].copy_from_slice(screen);
    say!("Comprobando el radio...");
    enter_flash_mode(link, MS_WAIT_BOOT)?;
    let result = (|| {
        let current = read_range(link, LOGO_ADDR, SECTOR, "Lectura del logo")?;
        if current != loaded {
            return Err("El logo del radio cambió desde que lo leíste. Léelo de nuevo.".to_string());
        }
        ensure_backup_dir(dir)?;
        let path = dir.join(format!("Baofeng_K6_logo_{}.bin", utc_stamp()));
        fs::write(&path, &current).map_err(|e| format!("No se pudo guardar la copia: {e}"))?;
        say!("Copia del logo: {}", path.display());
        say!("Guardando el logo...");
        // These two are the only flash writes this module makes, at this one fixed address.
        send_erase(link, LOGO_ADDR)?;
        send_write(link, LOGO_ADDR, &wanted)?;
        let check = read_range(link, LOGO_ADDR, SECTOR, "Verificando")?;
        if check != wanted {
            return Err(format!("El logo no coincide al leerlo. Vuelve a guardar; la copia está en {}", path.display()));
        }
        Ok(path)
    })();
    leave_flash_mode(link);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_use_the_radio_layout() {
        let mut gray = vec![255u8; WIDTH * HEIGHT];
        gray[0] = 0; // Top left.
        gray[WIDTH + 2] = 0; // x 2, second row of dots.
        gray[9 * WIDTH + 127] = 0; // x 127, row 9: second byte row, bit 1.
        let s = to_screen(&gray, false, false);
        assert_eq!(s[0], 0b01);
        assert_eq!(s[2], 0b10);
        assert_eq!(s[WIDTH + 127], 0b10);
        assert_eq!(s.iter().map(|b| b.count_ones()).sum::<u32>(), 3);
        assert!(dot(&s, 2, 1) && dot(&s, 127, 9) && !dot(&s, 1, 0));
    }

    #[test]
    fn invert_swaps_dark_and_light() {
        let white = vec![255u8; WIDTH * HEIGHT];
        assert!(to_screen(&white, false, false).iter().all(|&b| b == 0));
        assert!(to_screen(&white, true, false).iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn dithering_keeps_mid_gray_near_half_dark() {
        let gray = vec![128u8; WIDTH * HEIGHT];
        assert_eq!(to_screen(&gray, false, false).iter().map(|b| b.count_ones()).sum::<u32>(), 0);
        let dark: u32 = to_screen(&gray, false, true).iter().map(|b| b.count_ones()).sum();
        let total = (WIDTH * HEIGHT) as u32;
        assert!(dark > total * 2 / 5 && dark < total * 3 / 5, "{dark}");
    }

    #[test]
    fn images_fit_without_changing_shape() {
        assert_eq!(fit_size(128, 64), (128, 64));
        assert_eq!(fit_size(256, 256), (64, 64));
        assert_eq!(fit_size(1000, 100), (128, 12));
        assert_eq!(fit_size(10, 1000), (1, 64));
        assert_eq!(fit_size(5000, 1), (128, 1));

        let out = center_on_white(&[0; 4], 2, 2);
        assert_eq!(out.len(), WIDTH * HEIGHT);
        assert_eq!(out[31 * WIDTH + 63], 0);
        assert_eq!(out[32 * WIDTH + 64], 0);
        assert_eq!(out[0], 255);
        assert_eq!(out.iter().filter(|&&g| g == 0).count(), 4);
    }

    #[test]
    fn bmp_is_a_valid_top_to_bottom_picture() {
        let mut s = vec![0u8; SCREEN_SIZE];
        s[0] = 1; // Top left dot.
        let b = to_bmp(&s, 2, [1, 2, 3], [9, 8, 7]);
        let (w, h) = (WIDTH * 2, HEIGHT * 2);
        let row = (w * 3 + 3) / 4 * 4;
        assert_eq!(b.len(), 54 + row * h);
        assert_eq!(&b[..2], b"BM");
        assert_eq!(i32::from_le_bytes([b[18], b[19], b[20], b[21]]), w as i32);
        // BMP rows run bottom to top and colors are blue, green, red.
        let top = 54 + row * (h - 1);
        assert_eq!(b[top..top + 3], [3, 2, 1]);
        assert_eq!(b[top + 6..top + 9], [7, 8, 9]);
        assert_eq!(b[54..57], [7, 8, 9]);
    }
}
