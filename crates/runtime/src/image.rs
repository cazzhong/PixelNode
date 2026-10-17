//! Image In：解碼 PNG、依模式貼合畫布（最近鄰），以路徑 + 修改時間快取。

use crate::Image;
use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

#[derive(Default)]
pub(crate) struct ImageCache {
    decoded: HashMap<String, (SystemTime, Image)>,
}

pub(crate) fn mtime(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl ImageCache {
    pub fn get(&mut self, path: &str) -> Result<&Image, String> {
        let t = mtime(path).ok_or_else(|| format!("找不到圖片：{path}"))?;
        let stale = self.decoded.get(path).is_none_or(|(old, _)| *old != t);
        if stale {
            let img = decode_png(Path::new(path))?;
            self.decoded.insert(path.to_string(), (t, img));
        }
        Ok(&self.decoded[path].1)
    }
}

pub fn decode_png(path: &Path) -> Result<Image, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("圖片過大")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let (w, h) = (info.width, info.height);
    let src = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => src.to_vec(),
        png::ColorType::Rgb => src.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => src.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => src.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("未展開的調色盤 PNG".into()),
    };
    Ok(Image { width: w, height: h, rgba })
}

type PixelMap = Box<dyn Fn(i64, i64) -> Option<(i64, i64)>>;

/// 貼合模式：0 原尺寸置中、1 整數縮放置中、2 拉伸、3 左上原尺寸。
pub fn fit(img: &Image, w: u32, h: u32, mode: u32) -> Image {
    let mut out = vec![0u8; (w * h * 4) as usize];
    let (iw, ih) = (img.width as i64, img.height as i64);
    let (cw, ch) = (w as i64, h as i64);
    // 目標像素 → 來源像素（以整數運算，保證最近鄰、不模糊）。
    let map: PixelMap = match mode {
        2 => Box::new(move |x, y| Some((x * iw / cw, y * ih / ch))),
        1 => {
            let s = (cw / iw).min(ch / ih).max(1);
            let (ox, oy) = ((cw - iw * s) / 2, (ch - ih * s) / 2);
            Box::new(move |x, y| Some(((x - ox).div_euclid(s), (y - oy).div_euclid(s))))
        }
        3 => Box::new(|x, y| Some((x, y))),
        _ => {
            let (ox, oy) = ((cw - iw) / 2, (ch - ih) / 2);
            Box::new(move |x, y| Some((x - ox, y - oy)))
        }
    };
    for y in 0..ch {
        for x in 0..cw {
            if let Some((sx, sy)) = map(x, y)
                && (0..iw).contains(&sx)
                && (0..ih).contains(&sy)
            {
                let s = ((sy * iw + sx) * 4) as usize;
                let d = ((y * cw + x) * 4) as usize;
                out[d..d + 4].copy_from_slice(&img.rgba[s..s + 4]);
            }
        }
    }
    Image { width: w, height: h, rgba: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img2x1() -> Image {
        Image { width: 2, height: 1, rgba: vec![255, 0, 0, 255, 0, 0, 255, 255] }
    }

    #[test]
    fn integer_fit_centers_and_scales() {
        let out = fit(&img2x1(), 6, 4, 1);
        // 倍率 min(6/2, 4/1) = 3 → 6×3，垂直置中 offset 0
        assert_eq!(&out.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&out.rgba[(3 * 4)..(3 * 4 + 4)], &[0, 0, 255, 255]);
        assert_eq!(out.rgba[(3 * 6 * 4) + 3], 0, "第 4 列在圖片外，應透明");
    }

    #[test]
    fn center_mode_keeps_size() {
        let out = fit(&img2x1(), 4, 1, 0);
        assert_eq!(out.rgba[3], 0);
        assert_eq!(&out.rgba[4..8], &[255, 0, 0, 255]);
        assert_eq!(&out.rgba[8..12], &[0, 0, 255, 255]);
    }

    #[test]
    fn png_roundtrip() {
        let path = std::env::temp_dir().join(format!("pixelnode-img-{}.png", std::process::id()));
        crate::export::write_png(&path, &img2x1()).unwrap();
        let back = decode_png(&path).unwrap();
        assert_eq!(back.rgba, img2x1().rgba);
        let _ = std::fs::remove_file(path);
    }
}
