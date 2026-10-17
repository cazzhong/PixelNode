//! 動畫輸出：GIF、PNG 序列、Sprite Sheet（含 JSON 中繼資料）。一律整數倍最近鄰放大。

use crate::{Image, Runtime};
use pixelnode_compiler::{CompileOptions, compile_with};
use pixelnode_graph::Graph;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Gif,
    PngSequence,
    /// columns = 0 時自動取接近正方形的欄數。
    SpriteSheet { columns: u32 },
}

#[derive(Debug, Clone)]
pub struct ExportSpec {
    pub kind: ExportKind,
    /// GIF / Sprite Sheet 為檔案路徑；PNG 序列為資料夾。
    pub path: PathBuf,
    pub scale: u32,
}

/// 依時間軸渲染所有影格（讀回 CPU）。
pub fn render_frames(rt: &mut Runtime, graph: &Graph) -> Result<Vec<Image>, String> {
    (0..graph.timeline.frames.max(1))
        .map(|frame| {
            let plan = compile_with(graph, CompileOptions { frame, ..Default::default() }).map_err(|e| e.to_string())?;
            Ok(rt.execute(&plan, frame))
        })
        .collect()
}

/// 渲染並輸出，回傳寫入檔案的摘要。
pub fn export(rt: &mut Runtime, graph: &Graph, spec: &ExportSpec) -> Result<String, String> {
    let frames: Vec<Image> = render_frames(rt, graph)?.iter().map(|f| upscale(f, spec.scale.max(1))).collect();
    let io = |e: std::io::Error| e.to_string();
    match spec.kind {
        ExportKind::Gif => {
            write_gif(&spec.path, &frames, graph.timeline.fps)?;
            Ok(format!("GIF {} 格 → {}", frames.len(), spec.path.display()))
        }
        ExportKind::PngSequence => {
            std::fs::create_dir_all(&spec.path).map_err(io)?;
            for (i, f) in frames.iter().enumerate() {
                write_png(&spec.path.join(format!("frame_{i:04}.png")), f)?;
            }
            Ok(format!("PNG {} 張 → {}", frames.len(), spec.path.display()))
        }
        ExportKind::SpriteSheet { columns } => {
            let (sheet, cols) = sprite_sheet(&frames, columns);
            write_png(&spec.path, &sheet)?;
            let meta = sheet_meta(&frames, cols, graph.timeline.fps);
            std::fs::write(spec.path.with_extension("json"), meta).map_err(io)?;
            Ok(format!("Sprite Sheet {}×{}（{cols} 欄）→ {}", sheet.width, sheet.height, spec.path.display()))
        }
    }
}

pub fn upscale(img: &Image, s: u32) -> Image {
    if s == 1 {
        return img.clone();
    }
    let (w, h) = (img.width * s, img.height * s);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let src_row = &img.rgba[((y / s) * img.width * 4) as usize..][..(img.width * 4) as usize];
        for px in src_row.as_chunks::<4>().0.iter() {
            for _ in 0..s {
                rgba.extend_from_slice(px);
            }
        }
    }
    Image { width: w, height: h, rgba }
}

pub fn write_png(path: &Path, img: &Image) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&img.rgba).map_err(|e| e.to_string())
}

pub fn sprite_sheet(frames: &[Image], columns: u32) -> (Image, u32) {
    let n = frames.len() as u32;
    let cols = if columns == 0 { (n as f32).sqrt().ceil() as u32 } else { columns.min(n) }.max(1);
    let rows = n.div_ceil(cols);
    let (fw, fh) = (frames[0].width, frames[0].height);
    let (w, h) = (fw * cols, fh * rows);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for (i, f) in frames.iter().enumerate() {
        let (cx, cy) = (i as u32 % cols * fw, i as u32 / cols * fh);
        for y in 0..fh {
            let dst = (((cy + y) * w + cx) * 4) as usize;
            let src = (y * fw * 4) as usize;
            rgba[dst..dst + (fw * 4) as usize].copy_from_slice(&f.rgba[src..src + (fw * 4) as usize]);
        }
    }
    (Image { width: w, height: h, rgba }, cols)
}

fn sheet_meta(frames: &[Image], cols: u32, fps: u32) -> String {
    let (fw, fh) = (frames[0].width, frames[0].height);
    let duration = 1000 / fps.max(1);
    let list: Vec<String> = (0..frames.len() as u32)
        .map(|i| {
            format!(
                "{{\"x\":{},\"y\":{},\"w\":{fw},\"h\":{fh},\"duration\":{duration}}}",
                i % cols * fw,
                i / cols * fh
            )
        })
        .collect();
    format!("{{\"frameWidth\":{fw},\"frameHeight\":{fh},\"columns\":{cols},\"fps\":{fps},\"frames\":[{}]}}\n", list.join(","))
}

/// 顏色數 ≤ 255 時使用精確的全域調色盤（像素藝術不失真）；否則逐格量化。
pub fn write_gif(path: &Path, frames: &[Image], fps: u32) -> Result<(), String> {
    let (w, h) = (frames[0].width as u16, frames[0].height as u16);
    let delay = (100 / fps.max(1)).max(2) as u16;
    let err = |e: gif::EncodingError| e.to_string();

    let mut colors: HashMap<[u8; 3], u8> = HashMap::new();
    let mut exact = true;
    'scan: for f in frames {
        for px in f.rgba.as_chunks::<4>().0.iter() {
            if px[3] >= 128 && !colors.contains_key(&[px[0], px[1], px[2]]) {
                if colors.len() == 255 {
                    exact = false;
                    break 'scan;
                }
                let idx = colors.len() as u8 + 1; // 0 保留給透明
                colors.insert([px[0], px[1], px[2]], idx);
            }
        }
    }

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = BufWriter::new(File::create(path).map_err(|e| format!("{}: {e}", path.display()))?);

    if exact {
        let mut palette = vec![0u8; 3 * (colors.len() + 1)];
        for (c, &i) in &colors {
            palette[i as usize * 3..i as usize * 3 + 3].copy_from_slice(c);
        }
        let mut enc = gif::Encoder::new(file, w, h, &palette).map_err(err)?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(err)?;
        for f in frames {
            let indices: Vec<u8> = f
                .rgba
                .as_chunks::<4>().0.iter()
                .map(|px| if px[3] < 128 { 0 } else { colors[&[px[0], px[1], px[2]]] })
                .collect();
            let frame = gif::Frame {
                width: w,
                height: h,
                delay,
                transparent: Some(0),
                dispose: gif::DisposalMethod::Background,
                buffer: indices.into(),
                ..Default::default()
            };
            enc.write_frame(&frame).map_err(err)?;
        }
    } else {
        let mut enc = gif::Encoder::new(file, w, h, &[]).map_err(err)?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(err)?;
        for f in frames {
            let mut rgba = f.rgba.clone();
            let mut frame = gif::Frame::from_rgba_speed(w, h, &mut rgba, 10);
            frame.delay = delay;
            frame.dispose = gif::DisposalMethod::Background;
            enc.write_frame(&frame).map_err(err)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, c: [u8; 4]) -> Image {
        Image { width: w, height: h, rgba: c.repeat((w * h) as usize) }
    }

    #[test]
    fn upscale_is_nearest_integer() {
        let mut a = img(2, 1, [0, 0, 0, 255]);
        a.rgba[4..8].copy_from_slice(&[255, 255, 255, 255]);
        let b = upscale(&a, 3);
        assert_eq!((b.width, b.height), (6, 3));
        assert_eq!(&b.rgba[8..12], &[0, 0, 0, 255]);
        assert_eq!(&b.rgba[12..16], &[255, 255, 255, 255]);
        assert_eq!(&b.rgba[(6 * 2 + 5) * 4..][..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn sheet_layout() {
        let frames: Vec<_> = (0..5).map(|i| img(4, 2, [i * 40, 0, 0, 255])).collect();
        let (sheet, cols) = sprite_sheet(&frames, 0);
        assert_eq!((cols, sheet.width, sheet.height), (3, 12, 4));
        // 第 5 格（索引 4）位於第 2 列第 2 欄
        let px = ((2 * 12 + 4) * 4) as usize;
        assert_eq!(sheet.rgba[px], 160);
    }

    #[test]
    fn writes_files() {
        let dir = std::env::temp_dir().join(format!("pixelnode-export-{}", std::process::id()));
        let frames: Vec<_> = (0..3).map(|i| img(8, 8, [i * 80, 10, 200, if i == 2 { 0 } else { 255 }])).collect();
        write_gif(&dir.join("a.gif"), &frames, 12).unwrap();
        write_png(&dir.join("b.png"), &frames[0]).unwrap();
        let gif = std::fs::read(dir.join("a.gif")).unwrap();
        assert_eq!(&gif[..6], b"GIF89a");
        let decoder = png::Decoder::new(std::io::BufReader::new(File::open(dir.join("b.png")).unwrap()));
        let reader = decoder.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (8, 8));
        let _ = std::fs::remove_dir_all(dir);
    }
}
