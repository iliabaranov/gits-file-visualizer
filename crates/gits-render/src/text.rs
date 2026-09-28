//! Glyph atlas built at startup from the fonts bundled in `assets/fonts/`
//! (compiled into the binary; no system fonts are used).

use std::collections::HashMap;

const MONO_TTF: &[u8] = include_bytes!("../../../assets/fonts/hud-mono.ttf");
const CJK_OTF: &[u8] = include_bytes!("../../../assets/fonts/calligraphy-cjk.otf");

pub const ZEAMI_VERSE: [&str; 4] = ["生死去来", "棚頭傀儡", "一線断時", "落落磊磊"];
/// The verse in English, shown in place of one of the four copies on the wall.
pub const ZEAMI_ENGLISH: [&str; 4] =
    ["Life and death come and go;", "puppets on a shelf.", "When the one string is cut,", "they collapse and scatter."];
const EXTRA_CJK: &str = "防壁侵入警告攻性再構築完了第二三四五六七八九十〇層外部内接続拒否電脳";
const EXTRA_MONO: &str = "▸◂◆◇░▒│─┼█▌·×→←↑↓";

const ATLAS_W: usize = 1024;
const MONO_PX: f32 = 36.0;
const CJK_PX: f32 = 120.0;

#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub uv0: [f32; 2],
    pub uv1: [f32; 2],
    /// Bitmap size and offsets in raster pixels (at the face's raster size).
    pub w: f32,
    pub h: f32,
    pub xmin: f32,
    pub ymin: f32,
    pub advance: f32,
}

pub struct Face {
    pub raster_px: f32,
    pub glyphs: HashMap<char, Glyph>,
}

impl Face {
    pub fn glyph(&self, c: char) -> Option<&Glyph> {
        self.glyphs.get(&c).or_else(|| self.glyphs.get(&'?'))
    }

    /// Width of `s` in pixels at `size_px`.
    pub fn measure(&self, s: &str, size_px: f32) -> f32 {
        let k = size_px / self.raster_px;
        s.chars().filter_map(|c| self.glyph(c)).map(|g| g.advance * k).sum()
    }
}

pub struct GlyphAtlas {
    pub width: u32,
    pub height: u32,
    /// Mip chain, level 0 first (R8).
    pub mips: Vec<Vec<u8>>,
    pub mono: Face,
    pub cjk: Face,
}

struct Packer {
    x: usize,
    y: usize,
    row_h: usize,
    pixels: Vec<u8>,
    height: usize,
}

impl Packer {
    fn place(&mut self, w: usize, h: usize, pad: usize, bitmap: &[u8]) -> (usize, usize) {
        if self.x + w + 2 * pad > ATLAS_W {
            self.x = 0;
            self.y += self.row_h;
            self.row_h = 0;
        }
        let (ox, oy) = (self.x + pad, self.y + pad);
        let need = oy + h + pad;
        if need > self.height {
            self.height = need.next_power_of_two().max(64);
            self.pixels.resize(ATLAS_W * self.height, 0);
        }
        for row in 0..h {
            let dst = (oy + row) * ATLAS_W + ox;
            self.pixels[dst..dst + w].copy_from_slice(&bitmap[row * w..row * w + w]);
        }
        self.x += w + 2 * pad;
        self.row_h = self.row_h.max(h + 2 * pad);
        (ox, oy)
    }
}

fn rasterize_face(
    font: &fontdue::Font,
    px: f32,
    chars: impl Iterator<Item = char>,
    pad: usize,
    packer: &mut Packer,
    out: &mut Vec<(char, usize, usize, fontdue::Metrics)>,
) {
    for c in chars {
        if font.lookup_glyph_index(c) == 0 && c != ' ' {
            continue;
        }
        let (m, bitmap) = font.rasterize(c, px);
        let (x, y) = if m.width > 0 && m.height > 0 { packer.place(m.width, m.height, pad, &bitmap) } else { (0, 0) };
        out.push((c, x, y, m));
    }
}

impl GlyphAtlas {
    pub fn build() -> anyhow::Result<Self> {
        let mono_font = fontdue::Font::from_bytes(MONO_TTF, fontdue::FontSettings { scale: MONO_PX, ..Default::default() })
            .map_err(|e| anyhow::anyhow!("hud font: {e}"))?;
        let cjk_font = fontdue::Font::from_bytes(CJK_OTF, fontdue::FontSettings { scale: CJK_PX, ..Default::default() })
            .map_err(|e| anyhow::anyhow!("calligraphy font: {e}"))?;

        let mut packer = Packer { x: 0, y: 0, row_h: 0, pixels: vec![0; ATLAS_W * 64], height: 64 };
        let mut mono_raw = vec![];
        let mut cjk_raw = vec![];
        let mono_chars = (' '..='~').chain(EXTRA_MONO.chars());
        rasterize_face(&mono_font, MONO_PX, mono_chars, 3, &mut packer, &mut mono_raw);
        let mut cjk_chars: Vec<char> =
            ZEAMI_VERSE.concat().chars().chain(EXTRA_CJK.chars()).chain(ZEAMI_ENGLISH.concat().chars()).collect();
        cjk_chars.sort();
        cjk_chars.dedup();
        rasterize_face(&cjk_font, CJK_PX, cjk_chars.into_iter(), 12, &mut packer, &mut cjk_raw);

        let height = packer.height;
        let to_face = |raw: Vec<(char, usize, usize, fontdue::Metrics)>, px: f32| Face {
            raster_px: px,
            glyphs: raw
                .into_iter()
                .map(|(c, x, y, m)| {
                    (
                        c,
                        Glyph {
                            uv0: [x as f32 / ATLAS_W as f32, y as f32 / height as f32],
                            uv1: [(x + m.width) as f32 / ATLAS_W as f32, (y + m.height) as f32 / height as f32],
                            w: m.width as f32,
                            h: m.height as f32,
                            xmin: m.xmin as f32,
                            ymin: m.ymin as f32,
                            advance: m.advance_width,
                        },
                    )
                })
                .collect(),
        };
        let mono = to_face(mono_raw, MONO_PX);
        let cjk = to_face(cjk_raw, CJK_PX);

        let mut mips = vec![packer.pixels];
        let (mut w, mut h) = (ATLAS_W, height);
        while w > 16 && h > 16 && mips.len() < 6 {
            let src = mips.last().unwrap();
            let (nw, nh) = (w / 2, h / 2);
            let mut dst = vec![0u8; nw * nh];
            for y in 0..nh {
                for x in 0..nw {
                    let s = |dx: usize, dy: usize| src[(2 * y + dy) * w + 2 * x + dx] as u32;
                    dst[y * nw + x] = ((s(0, 0) + s(1, 0) + s(0, 1) + s(1, 1) + 2) / 4) as u8;
                }
            }
            mips.push(dst);
            (w, h) = (nw, nh);
        }
        Ok(Self { width: ATLAS_W as u32, height: height as u32, mips, mono, cjk })
    }
}
