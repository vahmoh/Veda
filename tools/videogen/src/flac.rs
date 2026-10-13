// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A small FLAC encoder (RFC 9639) for the videos' sound: 16-bit PCM in
//! blocks of [`BLOCK`] samples, each channel coded with the smallest of the
//! fixed predictors of order 0 to 4 (or verbatim, or as a constant), its
//! residual Rice-coded in the best number of partitions, and stereo as
//! left/right, left/side, side/right or mid/side, whichever is smallest.
//! Every frame carries its CRC-8 and CRC-16; STREAMINFO has the MD5 of the
//! samples.
//!
//! A decoder for the tests checks that every stream comes back exactly.

use crate::md5::Md5;

/// Samples per channel in a block (the last block may be shorter).
pub const BLOCK: usize = 4096;

/// The finest Rice partitioning tried (2^8 partitions).
const MAX_PARTITION_ORDER: u32 = 8;

/// The highest fixed predictor order.
const MAX_ORDER: usize = 4;

/// An encoded stream: STREAMINFO and the frames, one per block.
#[derive(Clone, Debug)]
pub struct Flac {
    pub rate: u32,
    pub channels: usize,
    /// The STREAMINFO block's body.
    pub streaminfo: [u8; 34],
    /// The frames; frame `i` starts at sample `i * BLOCK`.
    pub frames: Vec<Vec<u8>>,
}

impl Flac {
    /// The `fLaC` marker and the STREAMINFO block (flagged last): the start
    /// of a `.flac` file and Matroska's CodecPrivate for `A_FLAC`.
    pub fn header(&self) -> Vec<u8> {
        let mut out = b"fLaC".to_vec();
        out.extend_from_slice(&[0x80, 0, 0, 34]);
        out.extend_from_slice(&self.streaminfo);
        out
    }

    /// The whole stream as a `.flac` file.
    #[cfg(test)]
    pub fn file(&self) -> Vec<u8> {
        let mut out = self.header();
        for f in &self.frames {
            out.extend_from_slice(f);
        }
        out
    }
}

/// Encodes interleaved 16-bit samples of `channels` (1 to 8) channels.
///
/// # Panics
///
/// If `channels` is not 1 to 8, or the samples do not fill whole frames of
/// them.
pub fn encode(pcm: &[i16], channels: usize, rate: u32) -> Flac {
    assert!((1..=8).contains(&channels) && pcm.len().is_multiple_of(channels), "{channels} channels");
    assert!(rate > 0 && rate < 1 << 20, "sample rate {rate}");
    let samples = pcm.len() / channels;
    let mut md5 = Md5::new();
    for s in pcm {
        md5.update(&s.to_le_bytes());
    }
    let mut frames = Vec::with_capacity(samples.div_ceil(BLOCK));
    for (number, block) in pcm.chunks(BLOCK * channels).enumerate() {
        let n = block.len() / channels;
        let chans: Vec<Vec<i32>> =
            (0..channels).map(|c| (0..n).map(|i| block[i * channels + c] as i32).collect()).collect();
        frames.push(encode_frame(number as u64, &chans, rate, None));
    }
    let (min_frame, max_frame) = frames.iter().fold((u32::MAX, 0), |(lo, hi), f| {
        let len = f.len() as u32;
        (lo.min(len), hi.max(len))
    });
    let mut w = BitWriter::default();
    w.put(16, BLOCK as u64);
    w.put(16, BLOCK as u64);
    w.put(24, if frames.is_empty() { 0 } else { min_frame as u64 });
    w.put(24, max_frame as u64);
    w.put(20, rate as u64);
    w.put(3, channels as u64 - 1);
    w.put(5, 15);
    w.put(36, samples as u64);
    for b in md5.finish() {
        w.put(8, b as u64);
    }
    let streaminfo = w.finish().try_into().expect("STREAMINFO is 34 bytes");
    Flac { rate, channels, streaminfo, frames }
}

/// How the two channels of a stereo frame are coded (the frame header's
/// channel assignment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stereo {
    LeftRight = 1,
    LeftSide = 8,
    SideRight = 9,
    MidSide = 10,
}

/// Encodes one frame. `force` picks the stereo coding (for the tests).
fn encode_frame(number: u64, chans: &[Vec<i32>], rate: u32, force: Option<Stereo>) -> Vec<u8> {
    let n = chans[0].len();
    // The subframes, with the channel assignment.
    let (assignment, subframes) = if chans.len() == 2 {
        let (l, r) = (&chans[0], &chans[1]);
        let mid: Vec<i32> = l.iter().zip(r).map(|(a, b)| (a + b) >> 1).collect();
        let side: Vec<i32> = l.iter().zip(r).map(|(a, b)| a - b).collect();
        let (pl, pr) = (plan(l, 16), plan(r, 16));
        let (pm, ps) = (plan(&mid, 16), plan(&side, 17));
        let options = [
            (Stereo::LeftRight, pl.bits + pr.bits),
            (Stereo::LeftSide, pl.bits + ps.bits),
            (Stereo::SideRight, ps.bits + pr.bits),
            (Stereo::MidSide, pm.bits + ps.bits),
        ];
        let best = match force {
            Some(s) => s,
            None => options.iter().min_by_key(|o| o.1).map(|o| o.0).unwrap_or(Stereo::LeftRight),
        };
        let pair = match best {
            Stereo::LeftRight => [(l, pl), (r, pr)],
            Stereo::LeftSide => [(l, pl), (&side, ps)],
            Stereo::SideRight => [(&side, ps), (r, pr)],
            Stereo::MidSide => [(&mid, pm), (&side, ps)],
        };
        let subframes: Vec<(Vec<i32>, Plan)> = pair.into_iter().map(|(s, p)| (s.clone(), p)).collect();
        (best as u64, subframes)
    } else {
        (chans.len() as u64 - 1, chans.iter().map(|c| (c.clone(), plan(c, 16))).collect())
    };

    let mut w = BitWriter::default();
    // Sync code, reserved bit, fixed block size.
    w.put(15, 0b111_1111_1111_1100);
    w.put(1, 0);
    let (size_code, size_extra) = match n {
        4096 => (12, None),
        _ if n <= 256 => (6, Some((8, n as u64 - 1))),
        _ => (7, Some((16, n as u64 - 1))),
    };
    w.put(4, size_code);
    w.put(4, rate_code(rate));
    w.put(4, assignment);
    // 16 bits per sample, reserved bit.
    w.put(3, 0b100);
    w.put(1, 0);
    for b in utf8_number(number) {
        w.put(8, b as u64);
    }
    if let Some((bits, v)) = size_extra {
        w.put(bits, v);
    }
    let crc = crc8(&w.bytes);
    w.put(8, crc as u64);
    for (samples, p) in &subframes {
        write_subframe(&mut w, samples, p);
    }
    let mut bytes = w.finish();
    let crc = crc16(&bytes);
    bytes.extend_from_slice(&crc.to_be_bytes());
    bytes
}

/// The frame header's code for a sample rate (0 means "see STREAMINFO").
fn rate_code(rate: u32) -> u64 {
    match rate {
        88_200 => 1,
        176_400 => 2,
        192_000 => 3,
        8_000 => 4,
        16_000 => 5,
        22_050 => 6,
        24_000 => 7,
        32_000 => 8,
        44_100 => 9,
        48_000 => 10,
        96_000 => 11,
        _ => 0,
    }
}

/// A frame number in FLAC's UTF-8-like coding.
fn utf8_number(v: u64) -> Vec<u8> {
    if v < 0x80 {
        return vec![v as u8];
    }
    // n bytes hold 5n + 1 bits.
    let mut n = 2;
    while n < 7 && v >> (5 * n + 1) != 0 {
        n += 1;
    }
    let mut out = vec![(0xFF00u16 >> n) as u8 | (v >> (6 * (n - 1))) as u8];
    for i in (0..n - 1).rev() {
        out.push(0x80 | ((v >> (6 * i)) & 0x3F) as u8);
    }
    out
}

/// How a subframe will be coded, and its size in bits.
#[derive(Clone, Debug)]
struct Plan {
    kind: Kind,
    bits: u64,
    /// Bits per sample of this channel (17 for a side channel).
    bps: u32,
}

#[derive(Clone, Debug)]
enum Kind {
    Constant,
    Verbatim,
    Fixed { order: usize, rice: Rice },
}

/// A Rice coding of a residual.
#[derive(Clone, Debug)]
struct Rice {
    /// 5-bit parameters (method 1) instead of 4-bit ones.
    wide: bool,
    partition_order: u32,
    params: Vec<u32>,
}

/// The smallest coding of one channel's samples.
fn plan(samples: &[i32], bps: u32) -> Plan {
    let n = samples.len();
    if samples.iter().all(|&s| s == samples[0]) {
        return Plan { kind: Kind::Constant, bits: 8 + bps as u64, bps };
    }
    let mut best = Plan { kind: Kind::Verbatim, bits: 8 + n as u64 * bps as u64, bps };
    for order in 0..=MAX_ORDER.min(n - 1) {
        let residual = fixed_residual(samples, order);
        let (rice, bits) = rice_plan(&residual, n, order);
        let total = 8 + order as u64 * bps as u64 + bits;
        if total < best.bits {
            best = Plan { kind: Kind::Fixed { order, rice }, bits: total, bps };
        }
    }
    best
}

/// The residual of the fixed predictor of `order` (after the warm-up samples).
fn fixed_residual(x: &[i32], order: usize) -> Vec<i32> {
    (order..x.len())
        .map(|i| match order {
            0 => x[i],
            1 => x[i] - x[i - 1],
            2 => x[i] - 2 * x[i - 1] + x[i - 2],
            3 => x[i] - 3 * x[i - 1] + 3 * x[i - 2] - x[i - 3],
            _ => x[i] - 4 * x[i - 1] + 6 * x[i - 2] - 4 * x[i - 3] + x[i - 4],
        })
        .collect()
}

/// Folds a signed residual into an unsigned number (0, -1, 1, -2, ...).
#[inline]
fn fold(r: i32) -> u32 {
    ((r << 1) ^ (r >> 31)) as u32
}

/// The best Rice coding of a residual of a block of `n` samples after
/// `order` warm-up samples, and its size in bits (method, partition order,
/// parameters and codes).
fn rice_plan(residual: &[i32], n: usize, order: usize) -> (Rice, u64) {
    let folded: Vec<u32> = residual.iter().map(|&r| fold(r)).collect();
    let max = folded.iter().copied().max().unwrap_or(0);
    // A larger parameter than the largest value's length never helps.
    let kmax = (32 - max.leading_zeros()).min(30) as usize;
    let mut finest = 0;
    while finest < MAX_PARTITION_ORDER && n.is_multiple_of(2 << finest) && (n >> (finest + 1)) > order {
        finest += 1;
    }
    // cost[p][k]: the sum of (u >> k) over the partition's values, at the
    // finest partitioning; coarser ones add pairs of partitions together.
    let parts = 1usize << finest;
    let len = n >> finest;
    let mut cost = vec![vec![0u64; kmax + 1]; parts];
    let mut counts = vec![0u64; parts];
    let mut at = 0;
    for (p, c) in cost.iter_mut().enumerate() {
        let take = if p == 0 { len - order } else { len };
        for &u in &folded[at..at + take] {
            for (k, sum) in c.iter_mut().enumerate() {
                *sum += (u >> k) as u64;
            }
        }
        counts[p] = take as u64;
        at += take;
    }
    let mut best: Option<(Rice, u64)> = None;
    let mut level = finest;
    loop {
        for wide in [false, true] {
            let (param_bits, limit) = if wide { (5, 30) } else { (4, 14) };
            let mut bits = 2 + 4;
            let mut params = Vec::with_capacity(cost.len());
            for (c, &count) in cost.iter().zip(&counts) {
                let (k, b) = (0..=kmax.min(limit))
                    .map(|k| (k as u32, count * (k as u64 + 1) + c[k]))
                    .min_by_key(|&(_, b)| b)
                    .unwrap_or((0, count));
                bits += param_bits + b;
                params.push(k);
            }
            if best.as_ref().is_none_or(|(_, b)| bits < *b) {
                best = Some((Rice { wide, partition_order: level, params }, bits));
            }
        }
        if level == 0 {
            break;
        }
        level -= 1;
        cost = cost.chunks(2).map(|pair| pair[0].iter().zip(&pair[1]).map(|(a, b)| a + b).collect()).collect();
        counts = counts.chunks(2).map(|pair| pair[0] + pair[1]).collect();
    }
    best.expect("one partitioning at least")
}

fn write_subframe(w: &mut BitWriter, samples: &[i32], p: &Plan) {
    // Padding bit, type, no wasted bits.
    match &p.kind {
        Kind::Constant => {
            w.put(8, 0);
            w.put_signed(p.bps, samples[0]);
        }
        Kind::Verbatim => {
            w.put(8, 0b0000_0010);
            for &s in samples {
                w.put_signed(p.bps, s);
            }
        }
        Kind::Fixed { order, rice } => {
            w.put(8, (0b00_1000 | *order as u64) << 1);
            for &s in &samples[..*order] {
                w.put_signed(p.bps, s);
            }
            let residual = fixed_residual(samples, *order);
            w.put(2, rice.wide as u64);
            w.put(4, rice.partition_order as u64);
            let len = samples.len() >> rice.partition_order;
            let mut at = 0;
            for (i, &k) in rice.params.iter().enumerate() {
                let take = if i == 0 { len - order } else { len };
                w.put(if rice.wide { 5 } else { 4 }, k as u64);
                for &r in &residual[at..at + take] {
                    w.rice(k, fold(r));
                }
                at += take;
            }
        }
    }
}

/// Writes bits most significant first.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    /// Bits waiting in `acc` (fewer than 8 between calls).
    n: u32,
}

impl BitWriter {
    /// The low `bits` (at most 56) bits of `v`.
    fn put(&mut self, bits: u32, v: u64) {
        debug_assert!(bits <= 56);
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | (v & ((1u64 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.bytes.push((self.acc >> self.n) as u8);
        }
    }

    /// A two's complement number in `bits` bits.
    fn put_signed(&mut self, bits: u32, v: i32) {
        self.put(bits, v as i64 as u64);
    }

    /// `u >> k` zeros, a one, then the low `k` bits of `u`.
    fn rice(&mut self, k: u32, u: u32) {
        let mut q = u >> k;
        while q >= 32 {
            self.put(32, 0);
            q -= 32;
        }
        self.put(q + 1, 1);
        self.put(k, u as u64);
    }

    /// Pads to a whole byte with zeros.
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.put(8 - self.n, 0);
        }
        self.bytes
    }
}

/// CRC-8 of frame headers: polynomial x^8 + x^2 + x + 1, starting at 0.
fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

/// CRC-16 of frames: polynomial x^16 + x^15 + x^2 + 1, starting at 0.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

/// A FLAC decoder for the tests: STREAMINFO and frames with constant,
/// verbatim and fixed subframes (escaped partitions and wasted bits
/// included), every CRC checked.
#[cfg(test)]
pub mod decode {
    use super::{crc8, crc16};

    /// A decoded stream.
    #[derive(Debug)]
    pub struct Decoded {
        pub rate: u32,
        pub channels: usize,
        pub bits: u32,
        pub min_block: u32,
        pub max_block: u32,
        pub min_frame: u32,
        pub max_frame: u32,
        pub total: u64,
        pub md5: [u8; 16],
        /// Interleaved samples.
        pub pcm: Vec<i16>,
        /// Each frame's channel assignment.
        pub assignments: Vec<u32>,
    }

    struct Reader<'a> {
        data: &'a [u8],
        /// Position in bits.
        pos: usize,
    }

    impl Reader<'_> {
        fn bit(&mut self) -> Result<u64, String> {
            let byte = *self.data.get(self.pos / 8).ok_or("the stream ends early")?;
            let b = (byte >> (7 - self.pos % 8)) & 1;
            self.pos += 1;
            Ok(b as u64)
        }

        fn get(&mut self, bits: u32) -> Result<u64, String> {
            let mut v = 0;
            for _ in 0..bits {
                v = (v << 1) | self.bit()?;
            }
            Ok(v)
        }

        fn signed(&mut self, bits: u32) -> Result<i64, String> {
            if bits == 0 {
                return Ok(0);
            }
            let v = self.get(bits)?;
            Ok(((v << (64 - bits)) as i64) >> (64 - bits))
        }

        fn unary(&mut self) -> Result<u64, String> {
            let mut q = 0;
            while self.bit()? == 0 {
                q += 1;
            }
            Ok(q)
        }

        fn align(&mut self) {
            self.pos = self.pos.div_ceil(8) * 8;
        }
    }

    /// Decodes a whole `.flac` file.
    pub fn decode(data: &[u8]) -> Result<Decoded, String> {
        if !data.starts_with(b"fLaC") {
            return Err("no fLaC marker".into());
        }
        let mut r = Reader { data, pos: 32 };
        let mut info = None;
        loop {
            let last = r.get(1)? == 1;
            let kind = r.get(7)?;
            let len = r.get(24)? as usize;
            let start = r.pos;
            if kind == 0 {
                let block = (r.get(16)? as u32, r.get(16)? as u32);
                let frame = (r.get(24)? as u32, r.get(24)? as u32);
                let rate = r.get(20)? as u32;
                let channels = r.get(3)? as usize + 1;
                let bits = r.get(5)? as u32 + 1;
                let total = r.get(36)?;
                let mut md5 = [0; 16];
                for b in &mut md5 {
                    *b = r.get(8)? as u8;
                }
                info = Some((block, frame, rate, channels, bits, total, md5));
            }
            r.pos = start + len * 8;
            if last {
                break;
            }
        }
        let ((min_block, max_block), (min_frame, max_frame), rate, channels, bits, total, md5) =
            info.ok_or("no STREAMINFO")?;
        let mut pcm = Vec::new();
        let mut assignments = Vec::new();
        let mut number = 0;
        while r.pos / 8 < data.len() {
            let start = r.pos / 8;
            if r.get(15)? != 0x7FFC || r.get(1)? != 0 {
                return Err(format!("no frame at byte {start}"));
            }
            let size_code = r.get(4)?;
            let rate_code = r.get(4)?;
            let assignment = r.get(4)? as u32;
            let size_bits = r.get(3)?;
            if r.get(1)? != 0 || size_bits != 0b100 || rate_code != super::rate_code(rate) {
                return Err(format!("frame {number}: an unexpected header"));
            }
            // The frame number.
            let first = r.get(8)?;
            let extra = (!(first as u8)).leading_zeros().saturating_sub(1);
            let mut n = first & (0x7F >> extra);
            for _ in 0..extra {
                n = (n << 6) | (r.get(8)? & 0x3F);
            }
            if n != number {
                return Err(format!("frame {number} is numbered {n}"));
            }
            let block = match size_code {
                12 => 4096,
                6 => r.get(8)? as usize + 1,
                7 => r.get(16)? as usize + 1,
                c => return Err(format!("block size code {c}")),
            };
            let crc = crc8(&data[start..r.pos / 8]);
            if r.get(8)? as u8 != crc {
                return Err(format!("frame {number}: a bad header CRC"));
            }
            let mut chans = Vec::new();
            for c in 0..channels {
                let side = matches!((assignment, c), (8, 1) | (9, 0) | (10, 1));
                chans.push(subframe(&mut r, block, bits + side as u32)?);
            }
            r.align();
            let crc = crc16(&data[start..r.pos / 8]);
            if r.get(16)? as u16 != crc {
                return Err(format!("frame {number}: a bad CRC"));
            }
            if channels == 2 {
                let (a, b) = (chans[0].clone(), chans[1].clone());
                let (l, rr): (Vec<i64>, Vec<i64>) = match assignment {
                    1 => (a, b),
                    8 => (a.clone(), a.iter().zip(&b).map(|(l, s)| l - s).collect()),
                    9 => (a.iter().zip(&b).map(|(s, r)| s + r).collect(), b),
                    10 => a
                        .iter()
                        .zip(&b)
                        .map(|(&m, &s)| {
                            let m = (m << 1) | (s & 1);
                            ((m + s) >> 1, (m - s) >> 1)
                        })
                        .unzip(),
                    x => return Err(format!("channel assignment {x}")),
                };
                chans = vec![l, rr];
            } else if assignment as usize != channels - 1 {
                return Err(format!("channel assignment {assignment}"));
            }
            for i in 0..block {
                for ch in &chans {
                    pcm.push(i16::try_from(ch[i]).map_err(|_| "a sample out of range")?);
                }
            }
            assignments.push(assignment);
            number += 1;
        }
        Ok(Decoded { rate, channels, bits, min_block, max_block, min_frame, max_frame, total, md5, pcm, assignments })
    }

    fn subframe(r: &mut Reader<'_>, n: usize, bps: u32) -> Result<Vec<i64>, String> {
        if r.get(1)? != 0 {
            return Err("a subframe's padding bit is set".into());
        }
        let kind = r.get(6)?;
        let wasted = if r.get(1)? == 1 { r.unary()? as u32 + 1 } else { 0 };
        let bps = bps - wasted;
        let mut x: Vec<i64> = match kind {
            0 => vec![r.signed(bps)?; n],
            1 => (0..n).map(|_| r.signed(bps)).collect::<Result<_, _>>()?,
            8..=12 => {
                let order = (kind - 8) as usize;
                let mut x: Vec<i64> = (0..order).map(|_| r.signed(bps)).collect::<Result<_, _>>()?;
                let res = residual(r, n, order)?;
                for e in res {
                    let i = x.len();
                    let p = match order {
                        0 => 0,
                        1 => x[i - 1],
                        2 => 2 * x[i - 1] - x[i - 2],
                        3 => 3 * x[i - 1] - 3 * x[i - 2] + x[i - 3],
                        _ => 4 * x[i - 1] - 6 * x[i - 2] + 4 * x[i - 3] - x[i - 4],
                    };
                    x.push(p + e);
                }
                x
            }
            k => return Err(format!("subframe type {k} is not supported here")),
        };
        for v in &mut x {
            *v <<= wasted;
        }
        Ok(x)
    }

    fn residual(r: &mut Reader<'_>, n: usize, order: usize) -> Result<Vec<i64>, String> {
        let method = r.get(2)?;
        let param_bits = match method {
            0 => 4,
            1 => 5,
            m => return Err(format!("residual coding method {m}")),
        };
        let partition_order = r.get(4)?;
        let parts = 1usize << partition_order;
        if !n.is_multiple_of(parts) || n / parts < order {
            return Err("a bad partition order".into());
        }
        let mut out = Vec::with_capacity(n - order);
        for p in 0..parts {
            let count = if p == 0 { n / parts - order } else { n / parts };
            let k = r.get(param_bits)? as u32;
            if k == (1 << param_bits) - 1 {
                let raw = r.get(5)? as u32;
                for _ in 0..count {
                    out.push(r.signed(raw)?);
                }
            } else {
                for _ in 0..count {
                    let u = (r.unary()? << k) | r.get(k)?;
                    out.push(((u >> 1) as i64) ^ -((u & 1) as i64));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::decode::decode;
    use super::*;

    /// A deterministic noise source.
    fn noise(seed: u32) -> impl FnMut() -> i32 {
        let mut s = seed | 1;
        move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s as i32
        }
    }

    fn round_trip(pcm: &[i16], channels: usize, rate: u32) -> Flac {
        let flac = encode(pcm, channels, rate);
        let d = decode(&flac.file()).unwrap();
        assert_eq!((d.rate, d.channels, d.bits), (rate, channels, 16));
        assert_eq!((d.min_block, d.max_block), (4096, 4096));
        assert_eq!(d.total as usize, pcm.len() / channels);
        assert!(d.pcm == pcm, "the samples come back");
        assert_eq!(d.md5, crate::md5::digest(&pcm.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<_>>()));
        let sizes: Vec<u32> = flac.frames.iter().map(|f| f.len() as u32).collect();
        assert_eq!(d.min_frame, sizes.iter().copied().min().unwrap_or(0));
        assert_eq!(d.max_frame, sizes.iter().copied().max().unwrap_or(0));
        flac
    }

    fn size(flac: &Flac) -> usize {
        flac.frames.iter().map(|f| f.len()).sum()
    }

    #[test]
    fn crcs_match_their_check_values() {
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }

    #[test]
    fn frame_numbers_are_coded_like_utf8() {
        assert_eq!(utf8_number(0), [0]);
        assert_eq!(utf8_number(0x7F), [0x7F]);
        assert_eq!(utf8_number(0x80), [0xC2, 0x80]);
        assert_eq!(utf8_number(0x7FF), [0xDF, 0xBF]);
        assert_eq!(utf8_number(0x800), [0xE0, 0xA0, 0x80]);
        assert_eq!(utf8_number(0x10000), [0xF0, 0x90, 0x80, 0x80]);
        assert_eq!(utf8_number(0x7FFF_FFFF), [0xFD, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF]);
        assert_eq!(utf8_number(0x8000_0000), [0xFE, 0x82, 0x80, 0x80, 0x80, 0x80, 0x80]);
    }

    #[test]
    fn silence_is_tiny() {
        let pcm = vec![0i16; 2 * 48_000];
        let flac = round_trip(&pcm, 2, 48_000);
        // A constant subframe per channel: a handful of bytes per frame.
        assert!(size(&flac) < 20 * flac.frames.len(), "{}", size(&flac));
    }

    #[test]
    fn noise_comes_back() {
        let mut rnd = noise(7);
        let pcm: Vec<i16> = (0..2 * 10_000).map(|_| rnd() as i16).collect();
        let flac = round_trip(&pcm, 2, 48_000);
        // Full-scale noise cannot be compressed, nor should it grow much.
        assert!(size(&flac) < pcm.len() * 2 + 64 * flac.frames.len());
    }

    #[test]
    fn music_compresses() {
        let mut rnd = noise(3);
        let pcm: Vec<i16> = (0..30_000)
            .flat_map(|i| {
                let t = i as f32 / 48_000.0;
                let tone = |f: f32, p: f32| (std::f32::consts::TAU * f * t + p).sin();
                let l = 9000.0 * tone(220.0, 0.0) + 3000.0 * tone(330.0, 1.0) + 800.0 * tone(1760.0, 0.3);
                let r = 9000.0 * tone(220.0, 0.2) + 2500.0 * tone(440.0, 2.0) + (rnd() % 4) as f32;
                [l as i16, r as i16]
            })
            .collect();
        let flac = round_trip(&pcm, 2, 48_000);
        assert!(size(&flac) * 2 < pcm.len() * 2, "{} of {}", size(&flac), pcm.len() * 2);
    }

    #[test]
    fn extremes_and_odd_lengths() {
        for len in [1usize, 2, 5, 15, 16, 17, 255, 256, 257, 4095, 4096, 4097, 3 * 4096 + 100] {
            let pcm: Vec<i16> = (0..2 * len)
                .map(|i| match i % 7 {
                    0 => i16::MIN,
                    1 | 2 => i16::MAX,
                    3 => -1,
                    4 => 0,
                    _ => (i as i16).wrapping_mul(997),
                })
                .collect();
            round_trip(&pcm, 2, 48_000);
            let mono: Vec<i16> = pcm.iter().step_by(2).copied().collect();
            round_trip(&mono, 1, 44_100);
        }
        // Full-scale square waves: the side channel needs 17 bits.
        let square: Vec<i16> =
            (0..9000).flat_map(|i| if i / 40 % 2 == 0 { [i16::MAX, i16::MIN] } else { [i16::MIN, i16::MAX] }).collect();
        round_trip(&square, 2, 48_000);
    }

    #[test]
    fn every_stereo_coding_round_trips() {
        let mut rnd = noise(11);
        let l: Vec<i32> = (0..4096).map(|i| ((i as f32 * 0.05).sin() * 20_000.0) as i32 + rnd() % 64).collect();
        let r: Vec<i32> = l.iter().map(|v| (-v / 2 + rnd() % 300).clamp(-32_768, 32_767)).collect();
        for mode in [Stereo::LeftRight, Stereo::LeftSide, Stereo::SideRight, Stereo::MidSide] {
            let flac = Flac {
                rate: 48_000,
                channels: 2,
                streaminfo: encode(&[0, 0], 2, 48_000).streaminfo,
                frames: vec![encode_frame(0, &[l.clone(), r.clone()], 48_000, Some(mode))],
            };
            let d = decode(&flac.file()).unwrap();
            assert_eq!(d.assignments, [mode as u32]);
            let pcm: Vec<i16> = l.iter().zip(&r).flat_map(|(&a, &b)| [a as i16, b as i16]).collect();
            assert!(d.pcm == pcm, "{mode:?}");
        }
    }

    #[test]
    fn similar_channels_use_a_side_channel() {
        let pcm: Vec<i16> = (0..4096)
            .flat_map(|i| {
                let v = ((i as f32 * 0.031).sin() * 12_000.0 + (i as f32 * 0.4).sin() * 3_000.0) as i16;
                [v, v.saturating_add((i % 3) as i16)]
            })
            .collect();
        let d = decode(&encode(&pcm, 2, 48_000).file()).unwrap();
        assert_ne!(d.assignments[0], Stereo::LeftRight as u32);
    }

    #[test]
    fn damage_is_detected() {
        let pcm: Vec<i16> = (0..2 * 5000).map(|i| ((i as f32 * 0.01).sin() * 5000.0) as i16).collect();
        let file = encode(&pcm, 2, 48_000).file();
        let header = 4 + 4 + 34;
        for at in [header + 3, header + 40, file.len() - 1] {
            let mut bad = file.clone();
            bad[at] ^= 0x10;
            assert!(decode(&bad).is_err(), "a flipped bit at {at}");
        }
    }

    #[test]
    fn the_header_is_matroskas_codec_private() {
        let flac = encode(&vec![0i16; 2 * 100], 2, 48_000);
        let h = flac.header();
        assert_eq!(&h[..8], b"fLaC\x80\0\0\x22");
        assert_eq!(h.len(), 42);
        // 48 kHz, 2 channels, 16 bits: 0x0BB80 << 12 | 1 << 9 | 15 << 4 | 0.
        assert_eq!(&h[18..21], [0x0B, 0xB8, 0x02]);
        assert_eq!(h[21], 0xF0);
    }
}
