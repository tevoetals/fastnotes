//! Imagens: decodificação PNG/JPEG/WebP em Rust puro, redução de tamanho e
//! gravação em WebP (sem perdas; o encoder puro não faz lossy).

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Largura máxima gravada em disco; acima disso a imagem é reduzida.
pub const MAX_STORED_WIDTH: u32 = 1600;

pub fn decode(bytes: &[u8]) -> Option<Bitmap> {
    if bytes.starts_with(b"\x89PNG") {
        decode_png(bytes)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        decode_jpeg(bytes)
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        decode_webp(bytes)
    } else {
        None
    }
}

fn decode_png(bytes: &[u8]) -> Option<Bitmap> {
    let mut d = png::Decoder::new(Cursor::new(bytes));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().ok()?;
    let mut buf = vec![0u8; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let n = (w * h) as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..n * 4].to_vec(),
        png::ColorType::Rgb => buf[..n * 3].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..n * 2].chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf[..n].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        _ => return None,
    };
    Some(Bitmap { w, h, rgba })
}

fn decode_jpeg(bytes: &[u8]) -> Option<Bitmap> {
    use zune_jpeg::zune_core::{colorspace::ColorSpace, options::DecoderOptions};
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(bytes, opts);
    let rgba = d.decode().ok()?;
    let info = d.info()?;
    Some(Bitmap { w: info.width as u32, h: info.height as u32, rgba })
}

fn decode_webp(bytes: &[u8]) -> Option<Bitmap> {
    let mut d = image_webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
    let (w, h) = d.dimensions();
    let mut buf = vec![0u8; d.output_buffer_size()?];
    d.read_image(&mut buf).ok()?;
    let rgba = if d.has_alpha() { buf } else { buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect() };
    Some(Bitmap { w, h, rgba })
}

pub fn encode_webp(bm: &Bitmap) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out).encode(&bm.rgba, bm.w, bm.h, image_webp::ColorType::Rgba8).ok()?;
    Some(out)
}

/// Redução por média de área (boa qualidade para diminuir).
pub fn resize(src: &Bitmap, tw: u32, th: u32) -> Bitmap {
    let (tw, th) = (tw.max(1), th.max(1));
    if tw == src.w && th == src.h {
        return Bitmap { w: src.w, h: src.h, rgba: src.rgba.clone() };
    }
    let mut out = vec![0u8; (tw * th * 4) as usize];
    let sx = src.w as f32 / tw as f32;
    let sy = src.h as f32 / th as f32;
    for ty in 0..th {
        let y0 = (ty as f32 * sy) as u32;
        let y1 = (((ty + 1) as f32 * sy) as u32).clamp(y0 + 1, src.h);
        for tx in 0..tw {
            let x0 = (tx as f32 * sx) as u32;
            let x1 = (((tx + 1) as f32 * sx) as u32).clamp(x0 + 1, src.w);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1 {
                let row = (y * src.w) as usize * 4;
                for x in x0..x1 {
                    let i = row + x as usize * 4;
                    let a = src.rgba[i + 3] as u32;
                    acc[0] += src.rgba[i] as u32 * a;
                    acc[1] += src.rgba[i + 1] as u32 * a;
                    acc[2] += src.rgba[i + 2] as u32 * a;
                    acc[3] += a;
                    n += 1;
                }
            }
            let o = ((ty * tw + tx) * 4) as usize;
            if acc[3] > 0 {
                out[o] = (acc[0] / acc[3]) as u8;
                out[o + 1] = (acc[1] / acc[3]) as u8;
                out[o + 2] = (acc[2] / acc[3]) as u8;
                out[o + 3] = (acc[3] / n.max(1)) as u8;
            }
        }
    }
    Bitmap { w: tw, h: th, rgba: out }
}

/// Cache de imagens decodificadas e de versões redimensionadas para a tela.
pub struct ImageCache {
    pub base: PathBuf,
    decoded: HashMap<String, Option<Rc<Bitmap>>>,
    scaled: HashMap<(String, u32), Rc<Bitmap>>,
}

impl ImageCache {
    pub fn new(base: PathBuf) -> ImageCache {
        ImageCache { base, decoded: HashMap::new(), scaled: HashMap::new() }
    }

    pub fn resolve(&self, path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            return std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest);
        }
        let p = Path::new(path);
        if p.is_absolute() { p.to_path_buf() } else { self.base.join(p) }
    }

    pub fn get(&mut self, path: &str) -> Option<Rc<Bitmap>> {
        if let Some(v) = self.decoded.get(path) {
            return v.clone();
        }
        let full = self.resolve(path);
        let bm = std::fs::read(full).ok().and_then(|b| decode(&b)).map(Rc::new);
        self.decoded.insert(path.to_string(), bm.clone());
        bm
    }

    /// Esquece uma imagem (depois de regravar o arquivo, por exemplo).
    pub fn forget(&mut self, path: &str) {
        self.decoded.remove(path);
        self.scaled.retain(|(p, _), _| p != path);
    }

    /// Versão redimensionada para caber em `width` px (nunca amplia).
    pub fn scaled(&mut self, path: &str, width: u32) -> Option<Rc<Bitmap>> {
        let bm = self.get(path)?;
        let tw = width.max(1);
        if tw == bm.w {
            return Some(bm);
        }
        let key = (path.to_string(), tw);
        if let Some(s) = self.scaled.get(&key) {
            return Some(s.clone());
        }
        let th = (bm.h as u64 * tw as u64 / bm.w as u64).max(1) as u32;
        let s = Rc::new(resize(&bm, tw, th));
        self.scaled.insert(key, s.clone());
        Some(s)
    }

}

/// Converte bytes de imagem para WebP (reduzindo a 1600 px de largura) e grava
/// em `dir/<nome>.webp`. Devolve o caminho relativo `images/<nome>.webp`.
pub fn import(bytes: &[u8], dir: &Path, stamp: &str) -> Option<String> {
    let bm = decode(bytes)?;
    let bm = if bm.w > MAX_STORED_WIDTH {
        let th = (bm.h as u64 * MAX_STORED_WIDTH as u64 / bm.w as u64).max(1) as u32;
        resize(&bm, MAX_STORED_WIDTH, th)
    } else {
        bm
    };
    let webp = encode_webp(&bm)?;
    std::fs::create_dir_all(dir).ok()?;
    let mut name = format!("{stamp}.webp");
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{stamp}-{n}.webp");
        n += 1;
    }
    std::fs::write(dir.join(&name), webp).ok()?;
    Some(format!("images/{name}"))
}

/// Decodifica `%XX` de um `file://` URI.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn is_image_path(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    l.ends_with(".png") || l.ends_with(".jpg") || l.ends_with(".jpeg") || l.ends_with(".webp")
}
