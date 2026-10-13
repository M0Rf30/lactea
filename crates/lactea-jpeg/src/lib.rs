//! JPEG decoder with `stb_image` semantics.
//!
//! projectM decodes user textures (`clouds.jpg`, ...) with `stbi_load(.., 4)`. Its integer IDCT, its
//! chroma upsampling filters (`resample_row_{v,h,hv}_2`) and its fixed-point YCbCr->RGB conversion are
//! not the ones of libjpeg(-turbo) or of the `image` crate: pixels differ by up to 4 code values (12 on
//! saturated edges) and a comp shader with a gain of 3-12 turns that into visible noise. This crate
//! ports exactly the arithmetic of `stb_image.h` (v2.30, public domain / MIT, Sean Barrett) for the
//! `req_comp = 4` case: baseline and progressive Huffman JPEG, 1/3/4 components, restart intervals,
//! Adobe APP14 transforms. The SIMD kernels of stb are documented to be bit-identical to the scalar
//! ones; `tests/stb_reference.rs` pins both against decoded reference output.
//!
//! The entropy decoder keeps stb's 9-bit `fast` Huffman lookup (it decides symbols, so it is part of the
//! arithmetic) but leaves out the `fast_ac` shortcut table, which only changes speed, not the decoded values of
//! a valid stream.

#![warn(missing_docs)]

use std::fmt;

/// A decoded image, RGBA8, row 0 = top, alpha 255 (what `stbi_load(.., 4)` returns for a JPEG).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, RGBA8, row-major.
    pub rgba: Vec<u8>,
}

/// Why a stream was rejected (stb's `failure_reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(&'static str);

impl Error {
    /// stb's failure message (e.g. `"bad huffman code"`).
    pub fn reason(&self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "jpeg: {}", self.0)
    }
}

impl std::error::Error for Error {}

type Res<T> = Result<T, Error>;

fn err<T>(msg: &'static str) -> Res<T> {
    Err(Error(msg))
}

/// Upper bound on the memory one decode may allocate: component planes, progressive coefficient
/// planes, resampling lines and the RGBA output together. A header can declare far more than the
/// file holds, so this is checked before any allocation.
const MAX_DECODE_BYTES: u64 = 2 << 30;

/// `n` zeroed elements, or an error instead of an abort when the allocator refuses.
fn alloc_zeroed<T: Clone + Default>(n: usize) -> Res<Vec<T>> {
    let mut v = Vec::new();
    if v.try_reserve_exact(n).is_err() {
        return err("out of memory");
    }
    v.resize(n, T::default());
    Ok(v)
}

/// `true` if `data` starts with an SOI marker (the sniff `stbi_load` uses before trying the JPEG decoder). Cheap
/// pre-check so callers can fall back to another decoder without paying for a failed `decode_rgba`.
pub fn is_jpeg(data: &[u8]) -> bool {
    data.len() >= 3 && data[0] == 0xff && data[1] == 0xd8
}

/// Decode a JPEG to RGBA8 exactly like `stbi_load_from_memory(.., req_comp = 4)`.
#[must_use = "the decoded image is the only result"]
pub fn decode_rgba(data: &[u8]) -> Result<Image, Error> {
    let mut j = Jpeg::new(data);
    j.decode()
}

// -------------------------------------------------------------------------------------------------
// tables
// -------------------------------------------------------------------------------------------------

const FAST_BITS: usize = 9;
/// `todo` value of a scan without restart intervals: never reaches zero.
const NO_RESTART_INTERVAL: i32 = 0x7fff_ffff;
const MARKER_NONE: u8 = 0xff;

const BMASK: [u32; 17] = [
    0, 1, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
];
const JBIAS: [i32; 16] = [
    0, -1, -3, -7, -15, -31, -63, -127, -255, -511, -1023, -2047, -4095, -8191, -16383, -32767,
];
/// zigzag index -> row-major position (+15 entries so corrupt input cannot index past the end)
const DEZIGZAG: [u8; 64 + 15] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
    63, 63, 63, 63, 63,
];

// -------------------------------------------------------------------------------------------------
// Huffman
// -------------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Huffman {
    fast: [u8; 1 << FAST_BITS],
    code: [u16; 256],
    values: [u8; 256],
    size: [u8; 257],
    maxcode: [u32; 18],
    delta: [i32; 17],
}

impl Default for Huffman {
    fn default() -> Self {
        Self {
            fast: [255; 1 << FAST_BITS],
            code: [0; 256],
            values: [0; 256],
            size: [0; 257],
            maxcode: [0; 18],
            delta: [0; 17],
        }
    }
}

impl Huffman {
    /// `stbi__build_huffman`
    fn build(&mut self, count: &[i32; 16]) -> Res<()> {
        let mut k = 0usize;
        for (i, &c) in count.iter().enumerate() {
            for _ in 0..c {
                self.size[k] = (i + 1) as u8;
                k += 1;
                if k >= 257 {
                    return err("bad size list");
                }
            }
        }
        self.size[k] = 0;

        let mut code: u32 = 0;
        k = 0;
        let mut j = 1usize;
        while j <= 16 {
            self.delta[j] = (k as u32).wrapping_sub(code) as i32;
            if self.size[k] as usize == j {
                while self.size[k] as usize == j {
                    self.code[k] = code as u16;
                    k += 1;
                    code += 1;
                }
                if code > (1u32 << j) {
                    return err("bad code lengths");
                }
            }
            self.maxcode[j] = code << (16 - j);
            code <<= 1;
            j += 1;
        }
        self.maxcode[j] = 0xffff_ffff;

        self.fast = [255; 1 << FAST_BITS];
        for i in 0..k {
            let s = self.size[i] as usize;
            if s <= FAST_BITS {
                let c = (self.code[i] as usize) << (FAST_BITS - s);
                let m = 1usize << (FAST_BITS - s);
                for jj in 0..m {
                    self.fast[c + jj] = i as u8;
                }
            }
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------
// decoder state
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Comp {
    id: i32,
    h: i32,
    v: i32,
    tq: usize,
    hd: usize,
    ha: usize,
    dc_pred: i32,
    /// effective size in samples
    x: i32,
    y: i32,
    /// allocated (MCU padded) size
    w2: i32,
    h2: i32,
    data: Vec<u8>,
    /// progressive only: `w2 * h2` coefficients, 64 per block
    coeff: Vec<i16>,
    coeff_w: i32,
}

/// Byte + bit reader over the entropy-coded stream (`stbi__context` + the `code_buffer` of `stbi__jpeg`).
struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
    code_buffer: u32,
    code_bits: i32,
    marker: u8,
    nomore: bool,
}

struct Jpeg<'a> {
    rd: Reader<'a>,
    img_x: i32,
    img_y: i32,
    img_n: i32,

    huff_dc: [Huffman; 4],
    huff_ac: [Huffman; 4],
    dequant: [[u16; 64]; 4],

    img_h_max: i32,
    img_v_max: i32,
    img_mcu_x: i32,
    img_mcu_y: i32,
    comp: [Comp; 4],

    progressive: bool,
    spec_start: i32,
    spec_end: i32,
    succ_high: i32,
    succ_low: i32,
    eob_run: i32,
    jfif: bool,
    app14_color_transform: i32,
    rgb: i32,

    scan_n: usize,
    order: [usize; 4],
    restart_interval: i32,
    todo: i32,
}

impl<'a> Reader<'a> {
    // ---- byte reader (stb: reading past the end yields 0) -----------------------------------------

    /// Next byte; 0 past the end of the data, as stb's reader does (truncated files decode to what they have).
    fn get8(&mut self) -> u8 {
        match self.src.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                b
            }
            None => 0,
        }
    }
    fn get16be(&mut self) -> i32 {
        let a = self.get8() as i32;
        (a << 8) + self.get8() as i32
    }
    fn at_eof(&self) -> bool {
        self.pos >= self.src.len()
    }
    fn skip(&mut self, n: i32) {
        if n > 0 {
            self.pos = self.pos.saturating_add(n as usize);
        }
    }

    // ---- bit reader ----------------------------------------------------------------------------

    /// `stbi__grow_buffer_unsafe`. Once a marker is hit, `nomore` is set and zero bits are fed, so the
    /// loop always terminates and `code_bits` never exceeds 32 even though the source keeps no more data.
    fn grow_buffer(&mut self) {
        loop {
            let b: u32 = if self.nomore { 0 } else { self.get8() as u32 };
            if b == 0xff {
                let mut c = self.get8();
                while c == 0xff {
                    c = self.get8();
                }
                if c != 0 {
                    self.marker = c;
                    self.nomore = true;
                    return;
                }
            }
            self.code_buffer |= b << (24 - self.code_bits);
            self.code_bits += 8;
            if self.code_bits > 24 {
                break;
            }
        }
    }

    /// `stbi__jpeg_huff_decode`: symbol or -1
    fn huff_decode(&mut self, h: &Huffman) -> i32 {
        if self.code_bits < 16 {
            self.grow_buffer();
        }
        let c = ((self.code_buffer >> (32 - FAST_BITS)) & ((1 << FAST_BITS) - 1)) as usize;
        let k = h.fast[c];
        if k < 255 {
            let s = h.size[k as usize] as i32;
            if s > self.code_bits {
                return -1;
            }
            self.code_buffer <<= s;
            self.code_bits -= s;
            return h.values[k as usize] as i32;
        }
        let temp = self.code_buffer >> 16;
        let mut k = FAST_BITS + 1;
        while k < 17 && temp >= h.maxcode[k] {
            k += 1;
        }
        if k == 17 {
            self.code_bits -= 16;
            return -1;
        }
        if k as i32 > self.code_bits {
            return -1;
        }
        let c = ((self.code_buffer >> (32 - k)) & BMASK[k]) as i32 + h.delta[k];
        if !(0..256).contains(&c) {
            return -1;
        }
        self.code_bits -= k as i32;
        self.code_buffer <<= k;
        h.values[c as usize] as i32
    }

    /// `stbi__extend_receive` (`n` in 1..=15)
    fn extend_receive(&mut self, n: i32) -> i32 {
        debug_assert!((1..=15).contains(&n));
        if self.code_bits < n {
            self.grow_buffer();
        }
        if self.code_bits < n {
            return 0;
        }
        let sgn = (self.code_buffer >> 31) as i32;
        let mut k = self.code_buffer.rotate_left(n as u32);
        self.code_buffer = k & !BMASK[n as usize];
        k &= BMASK[n as usize];
        self.code_bits -= n;
        k as i32 + (JBIAS[n as usize] & (sgn - 1))
    }

    /// `stbi__jpeg_get_bits` (`n` in 0..=16)
    fn get_bits(&mut self, n: i32) -> i32 {
        debug_assert!((0..=16).contains(&n));
        if self.code_bits < n {
            self.grow_buffer();
        }
        if self.code_bits < n {
            return 0;
        }
        let mut k = self.code_buffer.rotate_left(n as u32);
        self.code_buffer = k & !BMASK[n as usize];
        k &= BMASK[n as usize];
        self.code_bits -= n;
        k as i32
    }

    fn get_bit(&mut self) -> bool {
        if self.code_bits < 1 {
            self.grow_buffer();
        }
        if self.code_bits < 1 {
            return false;
        }
        let k = self.code_buffer;
        self.code_buffer <<= 1;
        self.code_bits -= 1;
        k & 0x8000_0000 != 0
    }
}

impl<'a> Jpeg<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            rd: Reader {
                src,
                pos: 0,
                code_buffer: 0,
                code_bits: 0,
                marker: MARKER_NONE,
                nomore: false,
            },
            img_x: 0,
            img_y: 0,
            img_n: 0,
            huff_dc: Default::default(),
            huff_ac: Default::default(),
            dequant: [[0; 64]; 4],
            img_h_max: 1,
            img_v_max: 1,
            img_mcu_x: 0,
            img_mcu_y: 0,
            comp: Default::default(),
            progressive: false,
            spec_start: 0,
            spec_end: 0,
            succ_high: 0,
            succ_low: 0,
            eob_run: 0,
            jfif: false,
            app14_color_transform: -1,
            rgb: 0,
            scan_n: 0,
            order: [0; 4],
            restart_interval: 0,
            todo: 0,
        }
    }

    // ---- block decoders -------------------------------------------------------------------------

    /// `stbi__jpeg_decode_block`: baseline, dequantizes into `data` (row-major).
    fn decode_block(&mut self, data: &mut [i16; 64], b: usize) -> Res<()> {
        if self.rd.code_bits < 16 {
            self.rd.grow_buffer();
        }
        let t = self.rd.huff_decode(&self.huff_dc[self.comp[b].hd]);
        if !(0..=15).contains(&t) {
            return err("bad huffman code");
        }
        *data = [0; 64];
        let diff = if t != 0 { self.rd.extend_receive(t) } else { 0 };
        let dc = match self.comp[b].dc_pred.checked_add(diff) {
            Some(v) => v,
            None => return err("bad delta"),
        };
        self.comp[b].dc_pred = dc;
        let dq = self.dequant[self.comp[b].tq];
        let prod = dc as i64 * dq[0] as i64;
        if prod < i16::MIN as i64 || prod > i16::MAX as i64 {
            return err("can't merge dc and ac");
        }
        data[0] = prod as i16;

        let ha = self.comp[b].ha;
        let mut k = 1usize;
        loop {
            let rs = self.rd.huff_decode(&self.huff_ac[ha]);
            if rs < 0 {
                return err("bad huffman code");
            }
            let s = rs & 15;
            let r = (rs >> 4) as usize;
            if s == 0 {
                if rs != 0xf0 {
                    break;
                }
                k += 16;
            } else {
                k += r;
                // `k` can run past 63 on a corrupt stream; DEZIGZAG's padding (and this clamp) keep the index in
                // bounds like stb's table does, and the `k >= 64` check below ends the block.
                let zig = DEZIGZAG[k.min(DEZIGZAG.len() - 1)] as usize;
                k += 1;
                let v = self.rd.extend_receive(s);
                // stb stores the dequantised coefficient in a `short`: wrap-around on overflow is intended
                data[zig] = (v.wrapping_mul(dq[zig] as i32)) as i16;
            }
            if k >= 64 {
                break;
            }
        }
        Ok(())
    }

    /// `stbi__jpeg_decode_block_prog_dc`
    fn decode_block_prog_dc(&mut self, data: &mut [i16], b: usize) -> Res<()> {
        if self.spec_end != 0 {
            return err("can't merge dc and ac");
        }
        if self.rd.code_bits < 16 {
            self.rd.grow_buffer();
        }
        if self.succ_high == 0 {
            data[..64].fill(0);
            let t = self.rd.huff_decode(&self.huff_dc[self.comp[b].hd]);
            if !(0..=15).contains(&t) {
                return err("can't merge dc and ac");
            }
            let diff = if t != 0 { self.rd.extend_receive(t) } else { 0 };
            let dc = match self.comp[b].dc_pred.checked_add(diff) {
                Some(v) => v,
                None => return err("bad delta"),
            };
            self.comp[b].dc_pred = dc;
            let prod = dc as i64 * (1i64 << self.succ_low);
            if prod < i16::MIN as i64 || prod > i16::MAX as i64 {
                return err("can't merge dc and ac");
            }
            data[0] = prod as i16;
        } else if self.rd.get_bit() {
            data[0] = data[0].wrapping_add((1i32 << self.succ_low) as i16);
        }
        Ok(())
    }

    /// `stbi__jpeg_decode_block_prog_ac`
    fn decode_block_prog_ac(&mut self, data: &mut [i16], b: usize) -> Res<()> {
        if self.spec_start == 0 {
            return err("can't merge dc and ac");
        }
        let ha = self.comp[b].ha;
        if self.succ_high == 0 {
            let shift = self.succ_low;
            if self.eob_run != 0 {
                self.eob_run -= 1;
                return Ok(());
            }
            let mut k = self.spec_start as usize;
            loop {
                let rs = self.rd.huff_decode(&self.huff_ac[ha]);
                if rs < 0 {
                    return err("bad huffman code");
                }
                let s = rs & 15;
                let r = (rs >> 4) as usize;
                if s == 0 {
                    if r < 15 {
                        // first scan: this block's EOB counts as part of the run (`1 << r` blocks in all)
                        self.eob_run = 1 << r;
                        if r != 0 {
                            self.eob_run += self.rd.get_bits(r as i32);
                        }
                        self.eob_run -= 1;
                        break;
                    }
                    k += 16;
                } else {
                    k += r;
                    // `k` can run past 63 on a corrupt stream; DEZIGZAG's 15 padding entries (and this clamp)
                    // keep the index in bounds exactly like stb's table does, and the `k > spec_end` check
                    // below ends the block.
                    let zig = DEZIGZAG[k.min(DEZIGZAG.len() - 1)] as usize;
                    k += 1;
                    let v = self.rd.extend_receive(s);
                    data[zig] = (v.wrapping_mul(1 << shift)) as i16;
                }
                if k > self.spec_end as usize {
                    break;
                }
            }
        } else {
            // refinement scan
            let bit = (1i32 << self.succ_low) as i16;
            if self.eob_run != 0 {
                self.eob_run -= 1;
                for k in self.spec_start..=self.spec_end {
                    let p = &mut data[DEZIGZAG[k as usize] as usize];
                    if *p != 0 && self.rd.get_bit() && (*p & bit) == 0 {
                        if *p > 0 {
                            *p = p.wrapping_add(bit);
                        } else {
                            *p = p.wrapping_sub(bit);
                        }
                    }
                }
            } else {
                let mut k = self.spec_start;
                loop {
                    let rs = self.rd.huff_decode(&self.huff_ac[ha]);
                    if rs < 0 {
                        return err("bad huffman code");
                    }
                    let mut s = rs & 15;
                    let mut r = rs >> 4;
                    if s == 0 {
                        if r < 15 {
                            // refinement scans count the current block out of the run: `(1 << r) - 1` more
                            // blocks are skipped here, versus `1 << r` for the first scan above (`eob_run`
                            // is decremented once per block in both; keep the two conventions apart).
                            self.eob_run = (1 << r) - 1;
                            if r != 0 {
                                self.eob_run += self.rd.get_bits(r);
                            }
                            r = 64; // force end of block
                        }
                        // r == 15, s == 0: a run of 16 zeros, handled by the advance below
                    } else {
                        if s != 1 {
                            return err("bad huffman code");
                        }
                        s = if self.rd.get_bit() {
                            bit as i32
                        } else {
                            -(bit as i32)
                        };
                    }
                    // advance by r
                    while k <= self.spec_end {
                        let idx = DEZIGZAG[k as usize] as usize;
                        k += 1;
                        let nz = data[idx] != 0;
                        if nz {
                            if self.rd.get_bit() && (data[idx] & bit) == 0 {
                                if data[idx] > 0 {
                                    data[idx] = data[idx].wrapping_add(bit);
                                } else {
                                    data[idx] = data[idx].wrapping_sub(bit);
                                }
                            }
                        } else {
                            if r == 0 {
                                data[idx] = s as i16;
                                break;
                            }
                            r -= 1;
                        }
                    }
                    if k > self.spec_end {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    // ---- scans ----------------------------------------------------------------------------------

    fn jpeg_reset(&mut self) {
        self.rd.code_bits = 0;
        self.rd.code_buffer = 0;
        self.rd.nomore = false;
        for c in &mut self.comp {
            c.dc_pred = 0;
        }
        self.rd.marker = MARKER_NONE;
        self.todo = if self.restart_interval != 0 {
            self.restart_interval
        } else {
            NO_RESTART_INTERVAL
        };
        self.eob_run = 0;
    }

    /// After a restart interval: `true` = continue, `false` = stop the scan (not a restart marker).
    fn restart_check(&mut self) -> bool {
        self.todo -= 1;
        if self.todo <= 0 {
            if self.rd.code_bits < 24 {
                self.rd.grow_buffer();
            }
            if !(0xd0..=0xd7).contains(&self.rd.marker) {
                return false;
            }
            self.jpeg_reset();
        }
        true
    }

    /// `stbi__parse_entropy_coded_data`
    fn parse_entropy_coded_data(&mut self) -> Res<()> {
        self.jpeg_reset();
        let mut data = [0i16; 64];
        if !self.progressive {
            if self.scan_n == 1 {
                let n = self.order[0];
                let w = (self.comp[n].x + 7) >> 3;
                let h = (self.comp[n].y + 7) >> 3;
                for j in 0..h {
                    for i in 0..w {
                        self.decode_block(&mut data, n)?;
                        let c = &mut self.comp[n];
                        let off = (c.w2 * j * 8 + i * 8) as usize;
                        idct_block(&mut c.data[off..], c.w2 as usize, &data);
                        if !self.restart_check() {
                            return Ok(());
                        }
                    }
                }
            } else {
                for j in 0..self.img_mcu_y {
                    for i in 0..self.img_mcu_x {
                        for k in 0..self.scan_n {
                            let n = self.order[k];
                            for y in 0..self.comp[n].v {
                                for x in 0..self.comp[n].h {
                                    let x2 = (i * self.comp[n].h + x) * 8;
                                    let y2 = (j * self.comp[n].v + y) * 8;
                                    self.decode_block(&mut data, n)?;
                                    let c = &mut self.comp[n];
                                    let off = (c.w2 * y2 + x2) as usize;
                                    idct_block(&mut c.data[off..], c.w2 as usize, &data);
                                }
                            }
                        }
                        if !self.restart_check() {
                            return Ok(());
                        }
                    }
                }
            }
        } else if self.scan_n == 1 {
            let n = self.order[0];
            let w = (self.comp[n].x + 7) >> 3;
            let h = (self.comp[n].y + 7) >> 3;
            for j in 0..h {
                for i in 0..w {
                    let off = 64 * (i + j * self.comp[n].coeff_w) as usize;
                    // detach the block so the decoder can borrow `self` mutably
                    let mut blk = [0i16; 64];
                    blk.copy_from_slice(&self.comp[n].coeff[off..off + 64]);
                    if self.spec_start == 0 {
                        self.decode_block_prog_dc(&mut blk, n)?;
                    } else {
                        self.decode_block_prog_ac(&mut blk, n)?;
                    }
                    self.comp[n].coeff[off..off + 64].copy_from_slice(&blk);
                    if !self.restart_check() {
                        return Ok(());
                    }
                }
            }
        } else {
            for j in 0..self.img_mcu_y {
                for i in 0..self.img_mcu_x {
                    for k in 0..self.scan_n {
                        let n = self.order[k];
                        for y in 0..self.comp[n].v {
                            for x in 0..self.comp[n].h {
                                let x2 = i * self.comp[n].h + x;
                                let y2 = j * self.comp[n].v + y;
                                let off = 64 * (x2 + y2 * self.comp[n].coeff_w) as usize;
                                let mut blk = [0i16; 64];
                                blk.copy_from_slice(&self.comp[n].coeff[off..off + 64]);
                                self.decode_block_prog_dc(&mut blk, n)?;
                                self.comp[n].coeff[off..off + 64].copy_from_slice(&blk);
                            }
                        }
                    }
                    if !self.restart_check() {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// `stbi__jpeg_finish`: progressive images are dequantized and transformed once all scans are in.
    fn finish(&mut self) {
        if !self.progressive {
            return;
        }
        for n in 0..self.img_n as usize {
            let w = (self.comp[n].x + 7) >> 3;
            let h = (self.comp[n].y + 7) >> 3;
            let dq = self.dequant[self.comp[n].tq];
            for j in 0..h {
                for i in 0..w {
                    let c = &mut self.comp[n];
                    let off = 64 * (i + j * c.coeff_w) as usize;
                    let mut blk = [0i16; 64];
                    for (t, v) in blk.iter_mut().enumerate() {
                        *v = ((c.coeff[off + t] as i32).wrapping_mul(dq[t] as i32)) as i16;
                    }
                    let o = (c.w2 * j * 8 + i * 8) as usize;
                    idct_block(&mut c.data[o..], c.w2 as usize, &blk);
                }
            }
        }
    }

    // ---- markers --------------------------------------------------------------------------------

    fn get_marker(&mut self) -> u8 {
        if self.rd.marker != MARKER_NONE {
            let x = self.rd.marker;
            self.rd.marker = MARKER_NONE;
            return x;
        }
        let mut x = self.rd.get8();
        if x != 0xff {
            return MARKER_NONE;
        }
        while x == 0xff {
            x = self.rd.get8();
        }
        x
    }

    fn process_marker(&mut self, m: u8) -> Res<()> {
        match m {
            MARKER_NONE => return err("expected marker"),
            0xdd => {
                if self.rd.get16be() != 4 {
                    return err("bad DRI len");
                }
                self.restart_interval = self.rd.get16be();
                return Ok(());
            }
            0xdb => {
                let mut l = self.rd.get16be() - 2;
                while l > 0 {
                    let q = self.rd.get8() as i32;
                    let p = q >> 4;
                    let sixteen = p != 0;
                    let t = (q & 15) as usize;
                    if p != 0 && p != 1 {
                        return err("bad DQT type");
                    }
                    if t > 3 {
                        return err("bad DQT table");
                    }
                    for &z in DEZIGZAG.iter().take(64) {
                        let v = if sixteen {
                            self.rd.get16be() as u16
                        } else {
                            self.rd.get8() as u16
                        };
                        self.dequant[t][z as usize] = v;
                    }
                    l -= if sixteen { 129 } else { 65 };
                }
                return if l == 0 { Ok(()) } else { err("bad DQT") };
            }
            0xc4 => {
                let mut l = self.rd.get16be() - 2;
                while l > 0 {
                    let q = self.rd.get8() as usize;
                    let tc = q >> 4;
                    let th = q & 15;
                    if tc > 1 || th > 3 {
                        return err("bad DHT header");
                    }
                    let mut sizes = [0i32; 16];
                    let mut n = 0i32;
                    for s in &mut sizes {
                        *s = self.rd.get8() as i32;
                        n += *s;
                    }
                    if n > 256 {
                        return err("bad DHT header");
                    }
                    l -= 17;
                    let h = if tc == 0 {
                        &mut self.huff_dc[th]
                    } else {
                        &mut self.huff_ac[th]
                    };
                    h.build(&sizes)?;
                    for i in 0..n as usize {
                        let b = match self.rd.src.get(self.rd.pos) {
                            Some(&b) => {
                                self.rd.pos += 1;
                                b
                            }
                            None => 0,
                        };
                        let h = if tc == 0 {
                            &mut self.huff_dc[th]
                        } else {
                            &mut self.huff_ac[th]
                        };
                        h.values[i] = b;
                    }
                    l -= n;
                }
                return if l == 0 { Ok(()) } else { err("bad DHT") };
            }
            _ => {}
        }
        // comment or APPn
        if (0xe0..=0xef).contains(&m) || m == 0xfe {
            let mut l = self.rd.get16be();
            if l < 2 {
                return err(if m == 0xfe {
                    "bad COM len"
                } else {
                    "bad APP len"
                });
            }
            l -= 2;
            if m == 0xe0 && l >= 5 {
                let mut ok = true;
                for t in b"JFIF\0" {
                    if self.rd.get8() != *t {
                        ok = false;
                    }
                }
                l -= 5;
                if ok {
                    self.jfif = true;
                }
            } else if m == 0xee && l >= 12 {
                let mut ok = true;
                for t in b"Adobe\0" {
                    if self.rd.get8() != *t {
                        ok = false;
                    }
                }
                l -= 6;
                if ok {
                    self.rd.get8(); // version
                    self.rd.get16be(); // flags0
                    self.rd.get16be(); // flags1
                    self.app14_color_transform = self.rd.get8() as i32;
                    l -= 6;
                }
            }
            self.rd.skip(l);
            return Ok(());
        }
        err("unknown marker")
    }

    fn process_scan_header(&mut self) -> Res<()> {
        let ls = self.rd.get16be();
        let n = self.rd.get8() as i32;
        if !(1..=4).contains(&n) || n > self.img_n {
            return err("bad SOS component count");
        }
        self.scan_n = n as usize;
        if ls != 6 + 2 * n {
            return err("bad SOS len");
        }
        for i in 0..self.scan_n {
            let id = self.rd.get8() as i32;
            let q = self.rd.get8() as usize;
            let which = match (0..self.img_n as usize).find(|&w| self.comp[w].id == id) {
                Some(w) => w,
                None => return err("no matching component"),
            };
            self.comp[which].hd = q >> 4;
            if self.comp[which].hd > 3 {
                return err("bad DC huff");
            }
            self.comp[which].ha = q & 15;
            if self.comp[which].ha > 3 {
                return err("bad AC huff");
            }
            self.order[i] = which;
        }
        self.spec_start = self.rd.get8() as i32;
        self.spec_end = self.rd.get8() as i32;
        let aa = self.rd.get8() as i32;
        self.succ_high = aa >> 4;
        self.succ_low = aa & 15;
        if self.progressive {
            if self.spec_start > 63
                || self.spec_end > 63
                || self.spec_start > self.spec_end
                || self.succ_high > 13
                || self.succ_low > 13
            {
                return err("bad SOS");
            }
        } else {
            if self.spec_start != 0 {
                return err("bad SOS");
            }
            if self.succ_high != 0 || self.succ_low != 0 {
                return err("bad SOS");
            }
            self.spec_end = 63;
        }
        Ok(())
    }

    fn process_frame_header(&mut self) -> Res<()> {
        let lf = self.rd.get16be();
        if lf < 11 {
            return err("bad SOF len");
        }
        if self.rd.get8() != 8 {
            return err("only 8-bit");
        }
        self.img_y = self.rd.get16be();
        if self.img_y == 0 {
            return err("no header height");
        }
        self.img_x = self.rd.get16be();
        if self.img_x == 0 {
            return err("0 width");
        }
        let c = self.rd.get8() as i32;
        if c != 3 && c != 1 && c != 4 {
            return err("bad component count");
        }
        self.img_n = c;
        if lf != 8 + 3 * c {
            return err("bad SOF len");
        }
        self.rgb = 0;
        for i in 0..c as usize {
            self.comp[i].id = self.rd.get8() as i32;
            if c == 3 && self.comp[i].id == b"RGB"[i] as i32 {
                self.rgb += 1;
            }
            let q = self.rd.get8() as i32;
            self.comp[i].h = q >> 4;
            if self.comp[i].h == 0 || self.comp[i].h > 4 {
                return err("bad H");
            }
            self.comp[i].v = q & 15;
            if self.comp[i].v == 0 || self.comp[i].v > 4 {
                return err("bad V");
            }
            self.comp[i].tq = self.rd.get8() as usize;
            if self.comp[i].tq > 3 {
                return err("bad TQ");
            }
        }
        if self.img_x as u64 * self.img_y as u64 * 4 > (1 << 30) {
            return err("image too large");
        }
        let (mut h_max, mut v_max) = (1, 1);
        for i in 0..c as usize {
            h_max = h_max.max(self.comp[i].h);
            v_max = v_max.max(self.comp[i].v);
        }
        for i in 0..c as usize {
            if h_max % self.comp[i].h != 0 {
                return err("bad H");
            }
            if v_max % self.comp[i].v != 0 {
                return err("bad V");
            }
        }
        self.img_h_max = h_max;
        self.img_v_max = v_max;
        let (mcu_w, mcu_h) = (h_max * 8, v_max * 8);
        self.img_mcu_x = (self.img_x + mcu_w - 1) / mcu_w;
        self.img_mcu_y = (self.img_y + mcu_h - 1) / mcu_h;
        // The whole decode must fit the budget before anything is allocated: every component plane
        // (plus 16-bit coefficients when progressive), the resampling lines and the RGBA output.
        let mut total = self.img_x as u64 * self.img_y as u64 * 4;
        for cm in &self.comp[..c as usize] {
            let w2 = (self.img_mcu_x * cm.h * 8) as u64;
            let h2 = (self.img_mcu_y * cm.v * 8) as u64;
            total += w2 * h2 * if self.progressive { 3 } else { 1 };
            total += w2 + 8 * self.img_h_max as u64 + 4;
        }
        if total > MAX_DECODE_BYTES {
            return err("image too large");
        }
        for i in 0..c as usize {
            let cm = &mut self.comp[i];
            cm.x = (self.img_x * cm.h + h_max - 1) / h_max;
            cm.y = (self.img_y * cm.v + v_max - 1) / v_max;
            cm.w2 = self.img_mcu_x * cm.h * 8;
            cm.h2 = self.img_mcu_y * cm.v * 8;
            cm.data = alloc_zeroed(cm.w2 as usize * cm.h2 as usize)?;
            if self.progressive {
                cm.coeff_w = cm.w2 / 8;
                cm.coeff = alloc_zeroed(cm.w2 as usize * cm.h2 as usize)?;
            }
        }
        Ok(())
    }

    fn decode_header(&mut self) -> Res<()> {
        self.jfif = false;
        self.app14_color_transform = -1;
        self.rd.marker = MARKER_NONE;
        let m = self.get_marker();
        if m != 0xd8 {
            return err("no SOI");
        }
        let mut m = self.get_marker();
        while !matches!(m, 0xc0..=0xc2) {
            self.process_marker(m)?;
            m = self.get_marker();
            while m == MARKER_NONE {
                if self.rd.at_eof() {
                    return err("no SOF");
                }
                m = self.get_marker();
            }
        }
        self.progressive = m == 0xc2;
        self.process_frame_header()
    }

    fn skip_junk_at_end(&mut self) -> u8 {
        while !self.rd.at_eof() {
            let mut x = self.rd.get8();
            while x == 0xff {
                if self.rd.at_eof() {
                    return MARKER_NONE;
                }
                x = self.rd.get8();
                if x != 0x00 && x != 0xff {
                    return x;
                }
            }
        }
        MARKER_NONE
    }

    /// `stbi__decode_jpeg_image`: all scans, components left in YCbCr (or whatever the file stores)
    fn decode_image(&mut self) -> Res<()> {
        self.restart_interval = 0;
        self.decode_header()?;
        let mut m = self.get_marker();
        let mut scans = 0u32;
        while m != 0xd9 {
            if m == 0xda {
                scans += 1;
                self.process_scan_header()?;
                self.parse_entropy_coded_data()?;
                if self.rd.marker == MARKER_NONE {
                    self.rd.marker = self.skip_junk_at_end();
                }
                m = self.get_marker();
                if (0xd0..=0xd7).contains(&m) {
                    m = self.get_marker();
                }
            } else if m == 0xdc {
                let ld = self.rd.get16be();
                let nl = self.rd.get16be();
                if ld != 4 {
                    return err("bad DNL len");
                }
                if nl != self.img_y {
                    return err("bad DNL height");
                }
                m = self.get_marker();
            } else {
                self.process_marker(m)?;
                m = self.get_marker();
            }
        }
        if scans == 0 {
            return err("no SOS");
        }
        if self.progressive {
            self.finish();
        }
        Ok(())
    }

    // ---- resample + color convert (`load_jpeg_image` with req_comp = 4) --------------------------

    fn decode(&mut self) -> Res<Image> {
        self.decode_image()?;
        let n = 4usize;
        let img_n = self.img_n;
        let is_rgb =
            img_n == 3 && (self.rgb == 3 || (self.app14_color_transform == 0 && !self.jfif));
        let (w, h) = (self.img_x as usize, self.img_y as usize);

        struct Rs {
            hs: i32,
            vs: i32,
            ystep: i32,
            w_lores: usize,
            ypos: i32,
            line0: usize,
            line1: usize,
            linebuf: Vec<u8>,
        }
        let mut rs: Vec<Rs> = Vec::new();
        for k in 0..img_n as usize {
            let hs = self.img_h_max / self.comp[k].h;
            let vs = self.img_v_max / self.comp[k].v;
            let w_lores = (self.img_x + hs - 1) / hs;
            rs.push(Rs {
                hs,
                vs,
                ystep: vs >> 1,
                w_lores: w_lores as usize,
                ypos: 0,
                line0: 0,
                line1: 0,
                linebuf: alloc_zeroed::<u8>((w + 4).max(w_lores as usize * hs as usize + 4))?,
            });
        }

        let mut out = alloc_zeroed::<u8>(w * h * n)?;
        let mut rows: [Vec<u8>; 4] = Default::default();
        for j in 0..h {
            for k in 0..img_n as usize {
                let r = &mut rs[k];
                let c = &self.comp[k];
                let y_bot = r.ystep >= (r.vs >> 1);
                let (near, far) = if y_bot {
                    (r.line1, r.line0)
                } else {
                    (r.line0, r.line1)
                };
                let lw = r.w_lores;
                let in_near = &c.data[near..near + lw];
                let in_far = &c.data[far..far + lw];
                match (r.hs, r.vs) {
                    (1, 1) => r.linebuf[..lw].copy_from_slice(in_near),
                    (1, 2) => resample_v_2(&mut r.linebuf, in_near, in_far, lw),
                    (2, 1) => resample_h_2(&mut r.linebuf, in_near, lw),
                    (2, 2) => resample_hv_2(&mut r.linebuf, in_near, in_far, lw),
                    _ => resample_generic(&mut r.linebuf, in_near, lw, r.hs as usize),
                }
                r.ystep += 1;
                if r.ystep >= r.vs {
                    r.ystep = 0;
                    r.line0 = r.line1;
                    r.ypos += 1;
                    if r.ypos < c.y {
                        r.line1 += c.w2 as usize;
                    }
                }
                rows[k].clear();
                rows[k].extend_from_slice(&r.linebuf[..w]);
            }
            let o = &mut out[j * w * n..(j + 1) * w * n];
            match img_n {
                3 => {
                    if is_rgb {
                        for i in 0..w {
                            o[i * 4] = rows[0][i];
                            o[i * 4 + 1] = rows[1][i];
                            o[i * 4 + 2] = rows[2][i];
                            o[i * 4 + 3] = 255;
                        }
                    } else {
                        ycbcr_to_rgb_row(o, &rows[0], &rows[1], &rows[2], w);
                    }
                }
                4 => {
                    if self.app14_color_transform == 0 {
                        // CMYK
                        for i in 0..w {
                            let m = rows[3][i];
                            o[i * 4] = blinn_8x8(rows[0][i], m);
                            o[i * 4 + 1] = blinn_8x8(rows[1][i], m);
                            o[i * 4 + 2] = blinn_8x8(rows[2][i], m);
                            o[i * 4 + 3] = 255;
                        }
                    } else if self.app14_color_transform == 2 {
                        // YCCK
                        ycbcr_to_rgb_row(o, &rows[0], &rows[1], &rows[2], w);
                        for i in 0..w {
                            let m = rows[3][i];
                            o[i * 4] = blinn_8x8(255 - o[i * 4], m);
                            o[i * 4 + 1] = blinn_8x8(255 - o[i * 4 + 1], m);
                            o[i * 4 + 2] = blinn_8x8(255 - o[i * 4 + 2], m);
                        }
                    } else {
                        ycbcr_to_rgb_row(o, &rows[0], &rows[1], &rows[2], w);
                    }
                }
                _ => {
                    for i in 0..w {
                        let y = rows[0][i];
                        o[i * 4] = y;
                        o[i * 4 + 1] = y;
                        o[i * 4 + 2] = y;
                        o[i * 4 + 3] = 255;
                    }
                }
            }
        }
        Ok(Image {
            width: w as u32,
            height: h as u32,
            rgba: out,
        })
    }
}

// -------------------------------------------------------------------------------------------------
// IDCT (`stbi__idct_block`, derived from jidctint's DCT_ISLOW)
// -------------------------------------------------------------------------------------------------

/// `stbi__f2f`
const fn f2f(x: f32) -> i64 {
    ((x as f64) * 4096.0 + 0.5) as i64
}
const fn fsh(x: i64) -> i64 {
    x * 4096
}

// The constants are stb's decimal literals verbatim (`stbi__f2f(1.175875602f)`); they round to the same f32 there.
#[allow(clippy::excessive_precision)]
fn idct_1d(s: [i64; 8]) -> ([i64; 4], [i64; 4]) {
    let (mut p2, mut p3) = (s[2], s[6]);
    let mut p1 = (p2 + p3) * f2f(0.541_196_1);
    let t2 = p1 + p3 * f2f(-1.847_759_065);
    let t3 = p1 + p2 * f2f(0.765_366_865);
    p2 = s[0];
    p3 = s[4];
    let t0 = fsh(p2 + p3);
    let t1 = fsh(p2 - p3);
    let x0 = t0 + t3;
    let x3 = t0 - t3;
    let x1 = t1 + t2;
    let x2 = t1 - t2;
    let (mut t0, mut t1, mut t2, mut t3) = (s[7], s[5], s[3], s[1]);
    p3 = t0 + t2;
    let mut p4 = t1 + t3;
    p1 = t0 + t3;
    p2 = t1 + t2;
    let p5 = (p3 + p4) * f2f(1.175_875_602);
    t0 *= f2f(0.298_631_336);
    t1 *= f2f(2.053_119_869);
    t2 *= f2f(3.072_711_026);
    t3 *= f2f(1.501_321_110);
    p1 = p5 + p1 * f2f(-0.899_976_223);
    p2 = p5 + p2 * f2f(-2.562_915_447);
    p3 *= f2f(-1.961_570_560);
    p4 *= f2f(-0.390_180_644);
    t3 += p1 + p4;
    t2 += p2 + p3;
    t1 += p2 + p4;
    t0 += p1 + p3;
    ([x0, x1, x2, x3], [t0, t1, t2, t3])
}

fn clamp_u8(x: i64) -> u8 {
    x.clamp(0, 255) as u8
}

/// Inverse DCT of one dequantized block into 8 rows of `out` (stride `stride`).
fn idct_block(out: &mut [u8], stride: usize, d: &[i16; 64]) {
    let mut val = [0i64; 64];
    for i in 0..8 {
        let col = |r: usize| d[r * 8 + i] as i64;
        if (1..8).all(|r| d[r * 8 + i] == 0) {
            let dc = col(0) * 4;
            for r in 0..8 {
                val[r * 8 + i] = dc;
            }
        } else {
            let (mut x, t) = idct_1d([
                col(0),
                col(1),
                col(2),
                col(3),
                col(4),
                col(5),
                col(6),
                col(7),
            ]);
            for v in &mut x {
                *v += 512;
            }
            val[i] = (x[0] + t[3]) >> 10;
            val[56 + i] = (x[0] - t[3]) >> 10;
            val[8 + i] = (x[1] + t[2]) >> 10;
            val[48 + i] = (x[1] - t[2]) >> 10;
            val[16 + i] = (x[2] + t[1]) >> 10;
            val[40 + i] = (x[2] - t[1]) >> 10;
            val[24 + i] = (x[3] + t[0]) >> 10;
            val[32 + i] = (x[3] - t[0]) >> 10;
        }
    }
    for i in 0..8 {
        let v = &val[i * 8..i * 8 + 8];
        let (mut x, t) = idct_1d([v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]]);
        for e in &mut x {
            *e += 65536 + (128 << 17);
        }
        let o = &mut out[i * stride..i * stride + 8];
        o[0] = clamp_u8((x[0] + t[3]) >> 17);
        o[7] = clamp_u8((x[0] - t[3]) >> 17);
        o[1] = clamp_u8((x[1] + t[2]) >> 17);
        o[6] = clamp_u8((x[1] - t[2]) >> 17);
        o[2] = clamp_u8((x[2] + t[1]) >> 17);
        o[5] = clamp_u8((x[2] - t[1]) >> 17);
        o[3] = clamp_u8((x[3] + t[0]) >> 17);
        o[4] = clamp_u8((x[3] - t[0]) >> 17);
    }
}

// -------------------------------------------------------------------------------------------------
// chroma upsampling and color conversion
// -------------------------------------------------------------------------------------------------

#[inline]
fn div4(x: i32) -> u8 {
    (x >> 2) as u8
}
#[inline]
fn div16(x: i32) -> u8 {
    (x >> 4) as u8
}

fn resample_v_2(out: &mut [u8], near: &[u8], far: &[u8], w: usize) {
    for i in 0..w {
        out[i] = div4(3 * near[i] as i32 + far[i] as i32 + 2);
    }
}

fn resample_h_2(out: &mut [u8], input: &[u8], w: usize) {
    if w == 1 {
        out[0] = input[0];
        out[1] = input[0];
        return;
    }
    out[0] = input[0];
    out[1] = div4(input[0] as i32 * 3 + input[1] as i32 + 2);
    for i in 1..w - 1 {
        let n = 3 * input[i] as i32 + 2;
        out[i * 2] = div4(n + input[i - 1] as i32);
        out[i * 2 + 1] = div4(n + input[i + 1] as i32);
    }
    let i = w - 1;
    out[i * 2] = div4(input[w - 2] as i32 * 3 + input[w - 1] as i32 + 2);
    out[i * 2 + 1] = input[w - 1];
}

fn resample_hv_2(out: &mut [u8], near: &[u8], far: &[u8], w: usize) {
    if w == 1 {
        let v = div4(3 * near[0] as i32 + far[0] as i32 + 2);
        out[0] = v;
        out[1] = v;
        return;
    }
    let mut t1 = 3 * near[0] as i32 + far[0] as i32;
    out[0] = div4(t1 + 2);
    for i in 1..w {
        let t0 = t1;
        t1 = 3 * near[i] as i32 + far[i] as i32;
        out[i * 2 - 1] = div16(3 * t0 + t1 + 8);
        out[i * 2] = div16(3 * t1 + t0 + 8);
    }
    out[w * 2 - 1] = div4(t1 + 2);
}

fn resample_generic(out: &mut [u8], input: &[u8], w: usize, hs: usize) {
    for i in 0..w {
        for j in 0..hs {
            out[i * hs + j] = input[i];
        }
    }
}

/// `stbi__float2fixed`
const fn float2fixed(x: f32) -> i32 {
    ((x * 4096.0f32 + 0.5f32) as i32) << 8
}

/// `stbi__YCbCr_to_RGB_row` with `step = 4`
fn ycbcr_to_rgb_row(out: &mut [u8], y: &[u8], cb: &[u8], cr: &[u8], count: usize) {
    const CR_R: i32 = float2fixed(1.40200f32);
    const CR_G: i32 = -float2fixed(0.71414f32);
    const CB_G: i32 = -float2fixed(0.34414f32);
    const CB_B: i32 = float2fixed(1.77200f32);
    for i in 0..count {
        let y_fixed = ((y[i] as i32) << 20) + (1 << 19);
        let crv = cr[i] as i32 - 128;
        let cbv = cb[i] as i32 - 128;
        let r = y_fixed.wrapping_add(crv.wrapping_mul(CR_R));
        let g = y_fixed
            .wrapping_add(crv.wrapping_mul(CR_G))
            .wrapping_add(cbv.wrapping_mul(CB_G) & (0xffff_0000u32 as i32));
        let b = y_fixed.wrapping_add(cbv.wrapping_mul(CB_B));
        out[i * 4] = (r >> 20).clamp(0, 255) as u8;
        out[i * 4 + 1] = (g >> 20).clamp(0, 255) as u8;
        out[i * 4 + 2] = (b >> 20).clamp(0, 255) as u8;
        out[i * 4 + 3] = 255;
    }
}

/// `stbi__blinn_8x8`: rounded `x * y / 255`
fn blinn_8x8(x: u8, y: u8) -> u8 {
    let t = x as u32 * y as u32 + 128;
    ((t + (t >> 8)) >> 8) as u8
}
