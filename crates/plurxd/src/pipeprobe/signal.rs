//! The tone-map census's test signal and the measures taken of each output.
//!
//! Pure on purpose: everything here is arithmetic over code values and byte
//! buffers, so the PQ encoding, the patch layout and the grid statistics are
//! checked by unit tests rather than by eye on a production node.
//!
//! The signal is authored as real SMPTE ST 2084 (PQ) code values in BT.2020,
//! not an SDR pattern relabelled as PQ — `VIDEO-QUALITY-PROGRAM.md` rules that
//! out as an HDR fixture, and the boot probe's `testsrc2` fixture is exactly
//! that, which is fine for "does the graph run" and useless for judging a
//! tone map. One frame, laid out on the same 16 × 9 grid the measures use, so
//! every patch is whole grid blocks and a block mean reads one patch:
//!
//! | rows | content |
//! |---|---|
//! | 0 | grey ramp, 16 steps uniform in PQ from black to the signal's peak |
//! | 1 | grey ramp, 16 steps uniform in linear light up to the peak (highlight roll-off) |
//! | 2–3 | eight colours at 100 nits |
//! | 4–5 | the same eight at a quarter of the peak |
//! | 6–7 | the same eight at the peak |
//! | 8 | grey ramp, 16 steps uniform in PQ from black to 203 nits (BT.2408 reference white) |
//!
//! The eight colours are the BT.2020 primaries and secondaries and two skin
//! tones (the ColorChecker "light skin" and "dark skin" patches, carried from
//! sRGB into BT.2020 linear light). Each occupies two grid columns. A colour's
//! level is the light of its largest linear channel, so "at the peak" is the
//! brightest that colour can be without any channel exceeding the mastering
//! peak.

/// The measurement grid: 16 columns by 9 rows, matching a 16:9 frame.
pub const GRID_COLS: usize = 16;
pub const GRID_ROWS: usize = 9;

/// ST 2084 constants, exact as published.
const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;
/// PQ's absolute ceiling, cd/m².
const PQ_PEAK_NITS: f64 = 10_000.0;

/// BT.2020 non-constant-luminance luma weights.
const KR: f64 = 0.2627;
const KB: f64 = 0.0593;
const KG: f64 = 1.0 - KR - KB;

/// The PQ inverse EOTF: absolute luminance in cd/m² to a normalised signal
/// value in `[0, 1]`. Values outside `[0, 10000]` are clamped.
pub fn pq_encode(nits: f64) -> f64 {
    let y = (nits / PQ_PEAK_NITS).clamp(0.0, 1.0);
    let p = y.powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * p) / (1.0 + PQ_C3 * p)).powf(PQ_M2)
}

/// The PQ EOTF: a normalised signal value back to cd/m².
pub fn pq_decode(signal: f64) -> f64 {
    let e = signal.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    let y = ((e - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * e)).powf(1.0 / PQ_M1);
    y * PQ_PEAK_NITS
}

/// A normalised luma value as a 10-bit **narrow-range** (limited, "tv") code:
/// 64 is black, 940 is nominal peak. This is the range the census encodes in,
/// because it is what HDR10 titles carry and what every graph is built for.
pub fn narrow_luma_10(value: f64) -> u16 {
    (64.0 + 876.0 * value.clamp(0.0, 1.0)).round() as u16
}

/// A normalised colour-difference value in `[-0.5, 0.5]` as a 10-bit
/// narrow-range code: 512 is neutral, 64 and 960 the nominal extremes.
pub fn narrow_chroma_10(value: f64) -> u16 {
    (512.0 + 896.0 * value.clamp(-0.5, 0.5)).round() as u16
}

/// A normalised value as a 10-bit **full-range** code (0–1023). Not used by
/// the encoded signal; it exists so the tests can state the familiar
/// "100 nits is code 520" figure as well as the narrow-range one.
#[cfg(test)]
pub fn full_code_10(value: f64) -> u16 {
    (1023.0 * value.clamp(0.0, 1.0)).round() as u16
}

/// One version of the census signal: its mastering peak and MaxCLL, which
/// are the same number because the ramp reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalSpec {
    pub peak_nits: u32,
}

/// The two versions every census runs, in order.
pub const SIGNALS: [SignalSpec; 2] = [
    SignalSpec { peak_nits: 1_000 },
    SignalSpec { peak_nits: 4_000 },
];

/// sRGB-encoded ColorChecker skin patches (light skin, dark skin).
const SKIN_SRGB: [[f64; 3]; 2] = [[194.0, 150.0, 130.0], [115.0, 82.0, 68.0]];

/// BT.709 linear RGB → BT.2020 linear RGB (ITU-R BT.2087).
const BT709_TO_BT2020: [[f64; 3]; 3] = [
    [0.6274, 0.3293, 0.0433],
    [0.0691, 0.9195, 0.0114],
    [0.0164, 0.0880, 0.8956],
];

fn srgb_to_linear(code: f64) -> f64 {
    let v = code / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The eight patch colours as BT.2020 linear-light directions, each scaled so
/// its largest channel is 1.
pub fn patch_colours() -> [[f64; 3]; 8] {
    let skin = |srgb: [f64; 3]| {
        let lin = srgb.map(srgb_to_linear);
        let mut out = [0.0; 3];
        for (row, value) in BT709_TO_BT2020.iter().zip(out.iter_mut()) {
            *value = row[0] * lin[0] + row[1] * lin[1] + row[2] * lin[2];
        }
        let max = out.iter().copied().fold(0.0, f64::max);
        out.map(|v| v / max)
    };
    [
        [1.0, 0.0, 0.0], // red
        [0.0, 1.0, 0.0], // green
        [0.0, 0.0, 1.0], // blue
        [0.0, 1.0, 1.0], // cyan
        [1.0, 0.0, 1.0], // magenta
        [1.0, 1.0, 0.0], // yellow
        skin(SKIN_SRGB[0]),
        skin(SKIN_SRGB[1]),
    ]
}

impl SignalSpec {
    /// Stable name used in results, still file names and the UI.
    pub fn name(self) -> String {
        format!("maxcll-{}", self.peak_nits)
    }

    /// The three colour levels, in cd/m² of the brightest channel.
    pub fn levels(self) -> [f64; 3] {
        let peak = f64::from(self.peak_nits);
        [100.0, peak / 4.0, peak]
    }

    /// Linear-light BT.2020 RGB, in cd/m², of grid cell (`col`, `row`).
    pub fn cell_nits(self, col: usize, row: usize) -> [f64; 3] {
        let peak = f64::from(self.peak_nits);
        let step = col as f64 / (GRID_COLS - 1) as f64;
        let grey = |nits: f64| [nits; 3];
        match row {
            0 => grey(pq_decode(pq_encode(peak) * step)),
            1 => grey(peak * (col + 1) as f64 / GRID_COLS as f64),
            2..=7 => {
                let level = self.levels()[(row - 2) / 2];
                patch_colours()[col / 2].map(|c| c * level)
            }
            _ => grey(pq_decode(pq_encode(203.0) * step)),
        }
    }

    /// MaxFALL of the authored frame: the frame-average of each pixel's
    /// largest linear channel. Every grid cell has the same area, so the mean
    /// over cells is the mean over pixels.
    pub fn max_fall_nits(self) -> u32 {
        let mut sum = 0.0;
        for row in 0..GRID_ROWS {
            for col in 0..GRID_COLS {
                sum += self.cell_nits(col, row).into_iter().fold(0.0, f64::max);
            }
        }
        (sum / (GRID_COLS * GRID_ROWS) as f64).round() as u32
    }

    /// The narrow-range 10-bit Y′, Cb, Cr codes for grid cell (`col`, `row`).
    pub fn cell_codes(self, col: usize, row: usize) -> [u16; 3] {
        let [r, g, b] = self.cell_nits(col, row).map(pq_encode);
        let y = KR * r + KG * g + KB * b;
        let cb = (b - y) / (2.0 * (1.0 - KB));
        let cr = (r - y) / (2.0 * (1.0 - KR));
        [
            narrow_luma_10(y),
            narrow_chroma_10(cb),
            narrow_chroma_10(cr),
        ]
    }

    /// One `yuv420p10le` frame of this signal at `width` × `height`
    /// (both even), as the raw bytes ffmpeg's `rawvideo` demuxer reads.
    pub fn author_frame(self, width: usize, height: usize) -> Vec<u8> {
        assert!(
            width.is_multiple_of(2) && height.is_multiple_of(2),
            "4:2:0 needs even dimensions"
        );
        let mut codes = [[[0_u16; 3]; GRID_COLS]; GRID_ROWS];
        for (row, line) in codes.iter_mut().enumerate() {
            for (col, cell) in line.iter_mut().enumerate() {
                *cell = self.cell_codes(col, row);
            }
        }
        let cell = |x: usize, y: usize| codes[y * GRID_ROWS / height][x * GRID_COLS / width];
        let (cw, ch) = (width / 2, height / 2);
        let mut out = Vec::with_capacity((width * height + 2 * cw * ch) * 2);
        for y in 0..height {
            for x in 0..width {
                out.extend_from_slice(&cell(x, y)[0].to_le_bytes());
            }
        }
        for plane in 1..=2 {
            for y in 0..ch {
                for x in 0..cw {
                    // A chroma sample covers luma (2x, 2y)..(2x+1, 2y+1); cell
                    // edges fall on even pixels at the census geometry, so the
                    // top-left luma sample names the whole quad's cell.
                    out.extend_from_slice(&cell(2 * x, 2 * y)[plane].to_le_bytes());
                }
            }
        }
        out
    }
}

/// Per-channel block means of one 8-bit `yuv420p` frame on the census grid.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockMeans {
    pub y: Vec<f64>,
    pub u: Vec<f64>,
    pub v: Vec<f64>,
}

/// Bytes in one 8-bit `yuv420p` frame.
pub fn yuv420p_frame_bytes(width: usize, height: usize) -> usize {
    width * height + 2 * (width / 2) * (height / 2)
}

fn grid_means(plane: &[u8], width: usize, height: usize) -> Vec<f64> {
    let mut sums = vec![0_u64; GRID_COLS * GRID_ROWS];
    let mut counts = vec![0_u64; GRID_COLS * GRID_ROWS];
    for y in 0..height {
        let row = y * GRID_ROWS / height;
        for x in 0..width {
            let block = row * GRID_COLS + x * GRID_COLS / width;
            sums[block] += u64::from(plane[y * width + x]);
            counts[block] += 1;
        }
    }
    sums.iter()
        .zip(&counts)
        .map(|(&s, &n)| s as f64 / n.max(1) as f64)
        .collect()
}

/// Block means of `frame`, an 8-bit `yuv420p` picture of `width` × `height`.
/// The chroma planes are gridded at their own (half) resolution, so a block
/// covers the same picture area in all three channels.
pub fn block_means(frame: &[u8], width: usize, height: usize) -> Result<BlockMeans, String> {
    let (cw, ch) = (width / 2, height / 2);
    if width < GRID_COLS * 2 || height < GRID_ROWS * 2 {
        return Err(format!(
            "a {width}x{height} frame is too small for the {GRID_COLS}x{GRID_ROWS} grid"
        ));
    }
    if frame.len() != yuv420p_frame_bytes(width, height) {
        return Err(format!(
            "expected {} bytes for a {width}x{height} yuv420p frame, got {}",
            yuv420p_frame_bytes(width, height),
            frame.len()
        ));
    }
    let (y, rest) = frame.split_at(width * height);
    let (u, v) = rest.split_at(cw * ch);
    Ok(BlockMeans {
        y: grid_means(y, width, height),
        u: grid_means(u, cw, ch),
        v: grid_means(v, cw, ch),
    })
}

/// How far one output is from another, per channel (Y, U, V), in 8-bit
/// levels of block mean.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Distance {
    /// The largest block difference.
    pub max: [f64; 3],
    /// The 95th-percentile block difference (nearest rank).
    pub p95: [f64; 3],
}

impl Distance {
    /// The largest of the three channel maxima.
    pub fn overall_max(&self) -> f64 {
        self.max.into_iter().fold(0.0, f64::max)
    }
}

/// Nearest-rank percentile of `values` (not required to be sorted).
fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    let rank = ((p * values.len() as f64).ceil() as usize).clamp(1, values.len());
    values[rank - 1]
}

/// Per-channel distance between two sets of block means.
pub fn distance(a: &BlockMeans, b: &BlockMeans) -> Distance {
    let channel = |x: &[f64], y: &[f64]| {
        let mut deltas: Vec<f64> = x.iter().zip(y).map(|(p, q)| (p - q).abs()).collect();
        let max = deltas.iter().copied().fold(0.0, f64::max);
        (max, percentile(&mut deltas, 0.95))
    };
    let (ym, yp) = channel(&a.y, &b.y);
    let (um, up) = channel(&a.u, &b.u);
    let (vm, vp) = channel(&a.v, &b.v);
    Distance {
        max: [ym, um, vm],
        p95: [yp, up, vp],
    }
}

/// The luma of the brightest block — where a curve that clips, or one that
/// leaves highlights dim, shows first.
pub fn brightest_block_luma(means: &BlockMeans) -> f64 {
    means.y.iter().copied().fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat 8-bit yuv420p frame with every block at (y, u, v).
    fn flat(width: usize, height: usize, y: u8, u: u8, v: u8) -> Vec<u8> {
        let mut frame = vec![y; width * height];
        frame.extend(std::iter::repeat_n(u, (width / 2) * (height / 2)));
        frame.extend(std::iter::repeat_n(v, (width / 2) * (height / 2)));
        frame
    }

    /// Paint grid block (`col`, `row`) of the luma plane with `value`.
    fn paint_luma(
        frame: &mut [u8],
        width: usize,
        height: usize,
        col: usize,
        row: usize,
        value: u8,
    ) {
        for y in 0..height {
            for x in 0..width {
                if y * GRID_ROWS / height == row && x * GRID_COLS / width == col {
                    frame[y * width + x] = value;
                }
            }
        }
    }

    #[test]
    fn pq_encodes_known_luminances_exactly() {
        // ST 2084 reference points. 100 cd/m² is the figure everybody quotes:
        // 0.508 of full scale, code 520 of 1023 full range — and, in the
        // narrow range this census actually encodes, code 509.
        let e100 = pq_encode(100.0);
        assert!((e100 - 0.508_078).abs() < 1e-5, "{e100}");
        assert_eq!(full_code_10(e100), 520);
        assert_eq!(narrow_luma_10(e100), 509);
        // 203 cd/m² (BT.2408 reference white) is 58% PQ; 1000 is 75.2%.
        assert!((pq_encode(203.0) - 0.580_688).abs() < 1e-5);
        assert!((pq_encode(1000.0) - 0.751_827).abs() < 1e-5);
        assert_eq!(narrow_luma_10(pq_encode(1000.0)), 723);
        // The ends of the scale.
        assert_eq!(narrow_luma_10(pq_encode(10_000.0)), 940);
        assert_eq!(narrow_luma_10(pq_encode(0.0)), 64);
        assert_eq!(narrow_chroma_10(0.0), 512);
        // And the EOTF undoes it.
        for nits in [0.1, 1.0, 100.0, 1000.0, 4000.0] {
            assert!(
                (pq_decode(pq_encode(nits)) - nits).abs() / nits < 1e-9,
                "{nits}"
            );
        }
    }

    #[test]
    fn the_signal_reaches_its_peak_and_carries_colour() {
        for spec in SIGNALS {
            let peak = f64::from(spec.peak_nits);
            // Both ramps end at the stated MaxCLL, so the metadata is true.
            assert!((spec.cell_nits(15, 0)[0] - peak).abs() < 1e-6);
            assert!((spec.cell_nits(15, 1)[0] - peak).abs() < 1e-6);
            assert_eq!(spec.cell_nits(0, 0), [0.0; 3]);
            let brightest = (0..GRID_ROWS)
                .flat_map(|r| (0..GRID_COLS).map(move |c| (c, r)))
                .map(|(c, r)| spec.cell_nits(c, r).into_iter().fold(0.0, f64::max))
                .fold(0.0, f64::max);
            assert!((brightest - peak).abs() < 1e-6, "nothing exceeds MaxCLL");
            // Neutral cells are neutral; primaries are not.
            assert_eq!(spec.cell_codes(8, 0)[1], 512);
            assert_eq!(spec.cell_codes(8, 0)[2], 512);
            let red = spec.cell_codes(0, 6);
            assert!(red[2] > 800, "BT.2020 red at peak drives Cr high: {red:?}");
            let blue = spec.cell_codes(4, 6);
            assert!(
                blue[1] > 800,
                "BT.2020 blue at peak drives Cb high: {blue:?}"
            );
            // Skin tones are warm: R > G > B in linear light.
            for col in [12, 14] {
                let [r, g, b] = spec.cell_nits(col, 4);
                assert!(r > g && g > b, "{col}: {r} {g} {b}");
            }
            let fall = spec.max_fall_nits();
            assert!(fall > 0 && f64::from(fall) < peak, "{fall}");
        }
    }

    #[test]
    fn an_authored_frame_has_the_layout_the_grid_reads() {
        let spec = SIGNALS[0];
        let (w, h) = (64, 36);
        let frame = spec.author_frame(w, h);
        assert_eq!(frame.len(), (w * h + 2 * (w / 2) * (h / 2)) * 2);
        let luma = |x: usize, y: usize| {
            u16::from_le_bytes([frame[(y * w + x) * 2], frame[(y * w + x) * 2 + 1]])
        };
        // Top-left is black, the top ramp's last block is the peak's code,
        // and every pixel of a block carries that block's code.
        assert_eq!(luma(0, 0), 64);
        assert_eq!(luma(w - 1, 0), narrow_luma_10(pq_encode(1000.0)));
        assert_eq!(luma(w - 4, 3), luma(w - 1, 0));
        let chroma_base = w * h * 2;
        let cb = |x: usize, y: usize| {
            let at = chroma_base + (y * (w / 2) + x) * 2;
            u16::from_le_bytes([frame[at], frame[at + 1]])
        };
        // Row 6 (peak colours) begins at luma y = 24, chroma y = 12; column 4
        // (blue) begins at luma x = 16, chroma x = 8.
        assert_eq!(cb(8, 12), spec.cell_codes(4, 6)[1]);
    }

    #[test]
    fn block_means_read_each_grid_block_on_its_own() {
        let (w, h) = (64, 36);
        let mut frame = flat(w, h, 100, 128, 128);
        paint_luma(&mut frame, w, h, 3, 2, 200);
        let means = block_means(&frame, w, h).expect("frame measures");
        assert_eq!(means.y.len(), GRID_COLS * GRID_ROWS);
        assert_eq!(means.y[2 * GRID_COLS + 3], 200.0);
        assert_eq!(means.y[2 * GRID_COLS + 4], 100.0);
        assert!(means.u.iter().all(|&u| u == 128.0));
        assert_eq!(brightest_block_luma(&means), 200.0);
        // A frame of the wrong size is refused, not measured.
        assert!(block_means(&frame[1..], w, h).is_err());
    }

    #[test]
    fn distance_reports_the_worst_block_and_the_95th_percentile() {
        let (w, h) = (64, 36);
        let base = block_means(&flat(w, h, 100, 128, 128), w, h).expect("test fixture");
        // One block off by 30: the maximum sees it; the 95th percentile of 144
        // blocks (rank 137) does not.
        let mut one = flat(w, h, 100, 128, 128);
        paint_luma(&mut one, w, h, 0, 0, 130);
        let d = distance(&block_means(&one, w, h).expect("test fixture"), &base);
        assert_eq!(d.max, [30.0, 0.0, 0.0]);
        assert_eq!(d.p95, [0.0, 0.0, 0.0]);
        // Eight blocks off by 30: 8 > 144 - 137, so the 95th percentile does.
        let mut eight = flat(w, h, 100, 128, 128);
        for col in 0..8 {
            paint_luma(&mut eight, w, h, col, 4, 70);
        }
        let d = distance(&block_means(&eight, w, h).expect("test fixture"), &base);
        assert_eq!(d.max[0], 30.0);
        assert_eq!(d.p95[0], 30.0);
        // A chroma shift lands in its own channel.
        let tinted = block_means(&flat(w, h, 100, 140, 120), w, h).expect("test fixture");
        let d = distance(&tinted, &base);
        assert_eq!(d.max, [0.0, 12.0, 8.0]);
        assert_eq!(d.overall_max(), 12.0);
        // Identical outputs are exactly zero apart.
        assert_eq!(distance(&base, &base).overall_max(), 0.0);
    }
}
