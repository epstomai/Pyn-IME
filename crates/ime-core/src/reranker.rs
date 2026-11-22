//! Pure-Rust causal character Transformer reranker (`IMERER01`) with prefix-trie KV sharing
//! and AVX2/FMA packed GEMM kernels.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const MAGIC: &[u8; 8] = b"IMERER01";
pub const PAD_ID: u16 = 0;
pub const BOS_ID: u16 = 1;
pub const EOS_ID: u16 = 2;
pub const PAUSE_ID: u16 = 3;
pub const UNK_ID: u16 = 4;
const MAX_CONTEXT_TOKENS: usize = 48;

#[inline]
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = (bits & 0x8000) != 0;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x03ff) as u32;
    if exp == 0 {
        if frac == 0 {
            return if sign { -0.0 } else { 0.0 };
        }
        let val = (frac as f32) * (1.0 / ((1u32 << 24) as f32));
        return if sign { -val } else { val };
    }
    if exp == 0x1f {
        let sign_bit = if sign { 0x8000_0000 } else { 0 };
        return f32::from_bits(sign_bit | 0x7f80_0000 | (frac << 13));
    }
    let sign_bit = if sign { 0x8000_0000 } else { 0 };
    f32::from_bits(sign_bit | ((exp + 112) << 23) | (frac << 13))
}

#[inline]
pub fn f32_to_f16(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let frac = bits & 0x007f_ffff;

    if exp == 255 {
        if frac == 0 {
            return sign | 0x7c00;
        }
        let nan_frac = ((frac >> 13) as u16).max(1);
        return sign | 0x7c00 | nan_frac;
    }

    let half_exp = exp - 127 + 15;
    if half_exp >= 31 {
        return sign | 0x7c00;
    }
    if half_exp <= 0 {
        if half_exp < -10 {
            return sign;
        }
        let mant = frac | 0x0080_0000;
        let shift = (14 - half_exp) as u32;
        let round_bit = (mant >> (shift - 1)) & 1;
        let sticky = (mant & ((1u32 << (shift - 1)) - 1)) != 0;
        let mut res = (mant >> shift) as u16;
        if round_bit != 0 && (sticky || (res & 1) != 0) {
            res += 1;
        }
        return sign | res;
    }

    let mut res = ((half_exp as u16) << 10) | ((frac >> 13) as u16);
    let round_bit = (frac >> 12) & 1;
    let sticky = (frac & 0x0fff) != 0;
    if round_bit != 0 && (sticky || (res & 1) != 0) {
        res += 1;
    }
    sign | res
}

#[derive(Debug, Clone)]
pub enum PackedPanels {
    I8Vnni {
        /// `[out_dim / 16, in_dim / 4, 16, 4]` packed signed `i8` weights.
        panels: Vec<i8>,
        /// Per-output-channel quantization scale `[out_dim]`.
        scales: Vec<f32>,
        /// Precomputed `scales[o] * sum_k(w_i8[o, k])` `[out_dim]` for zero-point offset.
        row_bias: Vec<f32>,
    },
    F32(Vec<f32>),
}

/// Matrix `[out_dim, in_dim]` packed for SIMD GEMM (`I8Vnni` `[out/16, in/4, 16, 4]` or `F32` `[out/16, in, 16]`).
#[derive(Debug, Clone)]
pub struct PackedMatrix {
    pub out_dim: usize,
    pub in_dim: usize,
    pub panels: PackedPanels,
}

impl PackedMatrix {
    pub fn from_row_major_f32(weights: &[f32], out_dim: usize, in_dim: usize) -> Self {
        assert_eq!(weights.len(), out_dim * in_dim);
        assert_eq!(out_dim % 16, 0, "PackedMatrix out_dim must be a multiple of 16");
        let num_blocks = out_dim / 16;
        let mut panels = vec![0.0_f32; num_blocks * in_dim * 16];
        for b in 0..num_blocks {
            let panel_base = b * in_dim * 16;
            for k in 0..in_dim {
                let dst = &mut panels[panel_base + k * 16..panel_base + k * 16 + 16];
                for lane in 0..16 {
                    dst[lane] = weights[(b * 16 + lane) * in_dim + k];
                }
            }
        }
        Self {
            out_dim,
            in_dim,
            panels: PackedPanels::F32(panels),
        }
    }

    pub fn from_row_major_f16(raw: &[u16], out_dim: usize, in_dim: usize) -> Self {
        assert_eq!(raw.len(), out_dim * in_dim);
        assert_eq!(out_dim % 16, 0, "PackedMatrix out_dim must be a multiple of 16");

        let use_vnni = in_dim % 4 == 0 && {
            #[cfg(target_arch = "x86_64")]
            {
                is_x86_feature_detected!("avx2")
                    && is_x86_feature_detected!("fma")
                    && is_x86_feature_detected!("avxvnni")
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                false
            }
        };

        if use_vnni {
            let num_blocks = out_dim / 16;
            let k_groups = in_dim / 4;
            let mut panels = vec![0i8; num_blocks * k_groups * 16 * 4];
            let mut scales = vec![0.0_f32; out_dim];
            let mut row_bias = vec![0.0_f32; out_dim];

            for o in 0..out_dim {
                let row = &raw[o * in_dim..(o + 1) * in_dim];
                let mut max_abs = 0.0_f32;
                for &bits in row {
                    let av = f16_to_f32(bits).abs();
                    if av > max_abs {
                        max_abs = av;
                    }
                }
                let scale = if max_abs < 1e-12_f32 {
                    0.0
                } else {
                    max_abs / 127.0_f32
                };
                let inv_scale = if max_abs < 1e-12_f32 {
                    0.0
                } else {
                    127.0_f32 / max_abs
                };
                scales[o] = scale;

                let blk = o / 16;
                let lane = o % 16;
                let mut sum_i32 = 0i32;
                for k4 in 0..k_groups {
                    let base_dst = (blk * k_groups + k4) * 64 + lane * 4;
                    for t in 0..4 {
                        let w = f16_to_f32(row[k4 * 4 + t]);
                        let q = if inv_scale == 0.0 {
                            0i8
                        } else {
                            (w * inv_scale).round_ties_even().clamp(-127.0, 127.0) as i8
                        };
                        panels[base_dst + t] = q;
                        sum_i32 += q as i32;
                    }
                }
                row_bias[o] = scale * (sum_i32 as f32);
            }

            return Self {
                out_dim,
                in_dim,
                panels: PackedPanels::I8Vnni {
                    panels,
                    scales,
                    row_bias,
                },
            };
        }

        Self::from_row_major_f16_as_f32(raw, out_dim, in_dim)
    }

    pub fn from_row_major_f16_as_f32(raw: &[u16], out_dim: usize, in_dim: usize) -> Self {
        assert_eq!(out_dim % 16, 0);
        assert_eq!(raw.len(), out_dim * in_dim);
        let num_blocks = out_dim / 16;
        let mut panels = vec![0.0_f32; num_blocks * in_dim * 16];
        for b in 0..num_blocks {
            let panel_base = b * in_dim * 16;
            for k in 0..in_dim {
                let dst = &mut panels[panel_base + k * 16..panel_base + k * 16 + 16];
                for lane in 0..16 {
                    dst[lane] = f16_to_f32(raw[(b * 16 + lane) * in_dim + k]);
                }
            }
        }
        Self {
            out_dim,
            in_dim,
            panels: PackedPanels::F32(panels),
        }
    }
}

#[derive(Default)]
struct VnniScratch {
    a_u8: Vec<u8>,
    a_scales: Vec<f32>,
    a_mins: Vec<f32>,
}

thread_local! {
    static VNNI_SCRATCH: RefCell<VnniScratch> = RefCell::new(VnniScratch::default());
}

#[inline]
fn quantize_row_u8_scalar(a_row: &[f32], dst_u8: &mut [u8]) -> (f32, f32) {
    let mut min_a = f32::INFINITY;
    let mut max_a = f32::NEG_INFINITY;
    for &v in a_row {
        if v < min_a {
            min_a = v;
        }
        if v > max_a {
            max_a = v;
        }
    }
    let range = max_a - min_a;
    if range < 1e-12_f32 {
        dst_u8.fill(0);
        return (min_a, 0.0);
    }
    let scale_a = range / 255.0_f32;
    let inv_scale = 255.0_f32 / range;
    for (d, &v) in dst_u8.iter_mut().zip(a_row.iter()) {
        let q = ((v - min_a) * inv_scale)
            .clamp(0.0_f32, 255.0_f32)
            .round_ties_even() as u8;
        *d = q;
    }
    (min_a, scale_a)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn quantize_rows_u8_avx2(
    a: &[f32],
    m: usize,
    k: usize,
    a_u8: &mut [u8],
    a_mins: &mut [f32],
    a_scales: &mut [f32],
) {
    use std::arch::x86_64::*;

    let ap = a.as_ptr();
    let dp = a_u8.as_mut_ptr();
    let vzero = _mm256_setzero_ps();
    let v255 = _mm256_set1_ps(255.0_f32);

    for r in 0..m {
        let row_src = ap.add(r * k);
        let row_dst = dp.add(r * k);

        let mut vmin_v = _mm256_set1_ps(f32::INFINITY);
        let mut vmax_v = _mm256_set1_ps(f32::NEG_INFINITY);
        let mut j = 0usize;
        while j + 8 <= k {
            let v = _mm256_loadu_ps(row_src.add(j));
            vmin_v = _mm256_min_ps(vmin_v, v);
            vmax_v = _mm256_max_ps(vmax_v, v);
            j += 8;
        }
        // Horizontal min/max reduction over 8 lanes.
        let min_lo = _mm256_castps256_ps128(vmin_v);
        let min_hi = _mm256_extractf128_ps(vmin_v, 1);
        let min4 = _mm_min_ps(min_lo, min_hi);
        let min2 = _mm_min_ps(min4, _mm_movehl_ps(min4, min4));
        let min1 = _mm_min_ss(min2, _mm_shuffle_ps(min2, min2, 1));
        let mut min_a = _mm_cvtss_f32(min1);

        let max_lo = _mm256_castps256_ps128(vmax_v);
        let max_hi = _mm256_extractf128_ps(vmax_v, 1);
        let max4 = _mm_max_ps(max_lo, max_hi);
        let max2 = _mm_max_ps(max4, _mm_movehl_ps(max4, max4));
        let max1 = _mm_max_ss(max2, _mm_shuffle_ps(max2, max2, 1));
        let mut max_a = _mm_cvtss_f32(max1);

        while j < k {
            let v = *row_src.add(j);
            if v < min_a {
                min_a = v;
            }
            if v > max_a {
                max_a = v;
            }
            j += 1;
        }

        let range = max_a - min_a;
        if range < 1e-12_f32 {
            a_mins[r] = min_a;
            a_scales[r] = 0.0;
            std::ptr::write_bytes(row_dst, 0, k);
            continue;
        }
        let scale_a = range / 255.0_f32;
        let inv_scale = 255.0_f32 / range;
        a_mins[r] = min_a;
        a_scales[r] = scale_a;

        let vmin = _mm256_set1_ps(min_a);
        let vinv = _mm256_set1_ps(inv_scale);
        let mut idx = 0usize;
        while idx + 16 <= k {
            let v0 = _mm256_loadu_ps(row_src.add(idx));
            let v1 = _mm256_loadu_ps(row_src.add(idx + 8));
            let s0 = _mm256_min_ps(
                _mm256_max_ps(_mm256_mul_ps(_mm256_sub_ps(v0, vmin), vinv), vzero),
                v255,
            );
            let s1 = _mm256_min_ps(
                _mm256_max_ps(_mm256_mul_ps(_mm256_sub_ps(v1, vmin), vinv), vzero),
                v255,
            );
            let q0 = _mm256_cvtps_epi32(s0);
            let q1 = _mm256_cvtps_epi32(s1);
            let p16 = _mm256_packs_epi32(q0, q1);
            let p16_fixed = _mm256_permute4x64_epi64::<0xD8>(p16);
            let lo = _mm256_castsi256_si128(p16_fixed);
            let hi = _mm256_extracti128_si256::<1>(p16_fixed);
            let bytes16 = _mm_packus_epi16(lo, hi);
            _mm_storeu_si128(row_dst.add(idx) as *mut __m128i, bytes16);
            idx += 16;
        }
        while idx < k {
            let v = *row_src.add(idx);
            *row_dst.add(idx) = ((v - min_a) * inv_scale)
                .clamp(0.0_f32, 255.0_f32)
                .round_ties_even() as u8;
            idx += 1;
        }
    }
}

/// Scalar reference matrix multiplication: `out[m, n] = a[m, k] * b^T` using packed `b`.
pub fn gemm_packed_scalar(a: &[f32], m: usize, k: usize, b: &PackedMatrix, out: &mut [f32]) {
    debug_assert_eq!(k, b.in_dim);
    debug_assert_eq!(a.len(), m * k);
    debug_assert_eq!(out.len(), m * b.out_dim);
    let num_blocks = b.out_dim / 16;
    match &b.panels {
        PackedPanels::F32(panels) => {
            for row in 0..m {
                let a_row = &a[row * k..(row + 1) * k];
                let o_row = &mut out[row * b.out_dim..(row + 1) * b.out_dim];
                for blk in 0..num_blocks {
                    let mut acc = [0.0_f32; 16];
                    let p_base = blk * k * 16;
                    for kk in 0..k {
                        let av = a_row[kk];
                        let pv = &panels[p_base + kk * 16..p_base + kk * 16 + 16];
                        for lane in 0..16 {
                            acc[lane] += av * pv[lane];
                        }
                    }
                    o_row[blk * 16..blk * 16 + 16].copy_from_slice(&acc);
                }
            }
        }
        PackedPanels::I8Vnni {
            panels,
            scales,
            row_bias,
        } => {
            let k_groups = k / 4;
            let mut q_row = vec![0u8; k];
            for row in 0..m {
                let a_row = &a[row * k..(row + 1) * k];
                let (min_a, scale_a) = quantize_row_u8_scalar(a_row, &mut q_row);
                let o_row = &mut out[row * b.out_dim..(row + 1) * b.out_dim];
                for blk in 0..num_blocks {
                    let mut acc = [0i32; 16];
                    for k4 in 0..k_groups {
                        let p_base = (blk * k_groups + k4) * 64;
                        let a0 = q_row[k4 * 4] as i32;
                        let a1 = q_row[k4 * 4 + 1] as i32;
                        let a2 = q_row[k4 * 4 + 2] as i32;
                        let a3 = q_row[k4 * 4 + 3] as i32;
                        for lane in 0..16 {
                            let w_base = p_base + lane * 4;
                            acc[lane] += a0 * (panels[w_base] as i32)
                                + a1 * (panels[w_base + 1] as i32)
                                + a2 * (panels[w_base + 2] as i32)
                                + a3 * (panels[w_base + 3] as i32);
                        }
                    }
                    for lane in 0..16 {
                        let o = blk * 16 + lane;
                        o_row[o] =
                            (acc[lane] as f32).mul_add(scale_a * scales[o], min_a * row_bias[o]);
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn gemm_packed_f32_avx2(
    a: &[f32],
    m: usize,
    k: usize,
    panels: &[f32],
    out_dim: usize,
    out: &mut [f32],
) {
    use std::arch::x86_64::*;

    let num_blocks = out_dim / 16;
    let a_ptr = a.as_ptr();
    let b_ptr = panels.as_ptr();
    let o_ptr = out.as_mut_ptr();

    let mut row = 0usize;
    while row + 4 <= m {
        let a0 = a_ptr.add(row * k);
        let a1 = a_ptr.add((row + 1) * k);
        let a2 = a_ptr.add((row + 2) * k);
        let a3 = a_ptr.add((row + 3) * k);
        let o0 = o_ptr.add(row * out_dim);
        let o1 = o_ptr.add((row + 1) * out_dim);
        let o2 = o_ptr.add((row + 2) * out_dim);
        let o3 = o_ptr.add((row + 3) * out_dim);

        for blk in 0..num_blocks {
            let mut c00 = _mm256_setzero_ps();
            let mut c01 = _mm256_setzero_ps();
            let mut c10 = _mm256_setzero_ps();
            let mut c11 = _mm256_setzero_ps();
            let mut c20 = _mm256_setzero_ps();
            let mut c21 = _mm256_setzero_ps();
            let mut c30 = _mm256_setzero_ps();
            let mut c31 = _mm256_setzero_ps();

            let mut p = b_ptr.add(blk * k * 16);
            for kk in 0..k {
                let b0 = _mm256_loadu_ps(p);
                let b1 = _mm256_loadu_ps(p.add(8));
                let av0 = _mm256_set1_ps(*a0.add(kk));
                c00 = _mm256_fmadd_ps(av0, b0, c00);
                c01 = _mm256_fmadd_ps(av0, b1, c01);
                let av1 = _mm256_set1_ps(*a1.add(kk));
                c10 = _mm256_fmadd_ps(av1, b0, c10);
                c11 = _mm256_fmadd_ps(av1, b1, c11);
                let av2 = _mm256_set1_ps(*a2.add(kk));
                c20 = _mm256_fmadd_ps(av2, b0, c20);
                c21 = _mm256_fmadd_ps(av2, b1, c21);
                let av3 = _mm256_set1_ps(*a3.add(kk));
                c30 = _mm256_fmadd_ps(av3, b0, c30);
                c31 = _mm256_fmadd_ps(av3, b1, c31);
                p = p.add(16);
            }
            let col = blk * 16;
            _mm256_storeu_ps(o0.add(col), c00);
            _mm256_storeu_ps(o0.add(col + 8), c01);
            _mm256_storeu_ps(o1.add(col), c10);
            _mm256_storeu_ps(o1.add(col + 8), c11);
            _mm256_storeu_ps(o2.add(col), c20);
            _mm256_storeu_ps(o2.add(col + 8), c21);
            _mm256_storeu_ps(o3.add(col), c30);
            _mm256_storeu_ps(o3.add(col + 8), c31);
        }
        row += 4;
    }

    if row + 2 <= m {
        let a0 = a_ptr.add(row * k);
        let a1 = a_ptr.add((row + 1) * k);
        let o0 = o_ptr.add(row * out_dim);
        let o1 = o_ptr.add((row + 1) * out_dim);
        for blk in 0..num_blocks {
            let mut c00 = _mm256_setzero_ps();
            let mut c01 = _mm256_setzero_ps();
            let mut c10 = _mm256_setzero_ps();
            let mut c11 = _mm256_setzero_ps();
            let mut p = b_ptr.add(blk * k * 16);
            for kk in 0..k {
                let b0 = _mm256_loadu_ps(p);
                let b1 = _mm256_loadu_ps(p.add(8));
                let av0 = _mm256_set1_ps(*a0.add(kk));
                c00 = _mm256_fmadd_ps(av0, b0, c00);
                c01 = _mm256_fmadd_ps(av0, b1, c01);
                let av1 = _mm256_set1_ps(*a1.add(kk));
                c10 = _mm256_fmadd_ps(av1, b0, c10);
                c11 = _mm256_fmadd_ps(av1, b1, c11);
                p = p.add(16);
            }
            let col = blk * 16;
            _mm256_storeu_ps(o0.add(col), c00);
            _mm256_storeu_ps(o0.add(col + 8), c01);
            _mm256_storeu_ps(o1.add(col), c10);
            _mm256_storeu_ps(o1.add(col + 8), c11);
        }
        row += 2;
    }

    if row < m {
        let a0 = a_ptr.add(row * k);
        let o0 = o_ptr.add(row * out_dim);
        for blk in 0..num_blocks {
            let mut c0 = _mm256_setzero_ps();
            let mut c1 = _mm256_setzero_ps();
            let mut p = b_ptr.add(blk * k * 16);
            for kk in 0..k {
                let av = _mm256_set1_ps(*a0.add(kk));
                let b0 = _mm256_loadu_ps(p);
                let b1 = _mm256_loadu_ps(p.add(8));
                c0 = _mm256_fmadd_ps(av, b0, c0);
                c1 = _mm256_fmadd_ps(av, b1, c1);
                p = p.add(16);
            }
            let col = blk * 16;
            _mm256_storeu_ps(o0.add(col), c0);
            _mm256_storeu_ps(o0.add(col + 8), c1);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma,avxvnni")]
#[allow(clippy::too_many_arguments)]
unsafe fn gemm_packed_i8_avxvnni(
    a: &[f32],
    m: usize,
    k: usize,
    panels: &[i8],
    scales: &[f32],
    row_bias: &[f32],
    out_dim: usize,
    out: &mut [f32],
) {
    use std::arch::x86_64::*;

    VNNI_SCRATCH.with(|cell| {
        let mut sc_guard = cell.borrow_mut();
        let sc = &mut *sc_guard;
        sc.a_u8.resize(m * k, 0);
        sc.a_scales.resize(m, 0.0);
        sc.a_mins.resize(m, 0.0);

        quantize_rows_u8_avx2(a, m, k, &mut sc.a_u8, &mut sc.a_mins, &mut sc.a_scales);

        let num_blocks = out_dim / 16;
        let k_groups = k / 4;
        let a_ptr = sc.a_u8.as_ptr();
        let b_ptr = panels.as_ptr();
        let sw_ptr = scales.as_ptr();
        let rb_ptr = row_bias.as_ptr();
        let o_ptr = out.as_mut_ptr();

        let mut row = 0usize;
        while row + 4 <= m {
            let a0 = a_ptr.add(row * k);
            let a1 = a_ptr.add((row + 1) * k);
            let a2 = a_ptr.add((row + 2) * k);
            let a3 = a_ptr.add((row + 3) * k);

            let vs_a0 = _mm256_set1_ps(sc.a_scales[row]);
            let vmin0 = _mm256_set1_ps(sc.a_mins[row]);
            let vs_a1 = _mm256_set1_ps(sc.a_scales[row + 1]);
            let vmin1 = _mm256_set1_ps(sc.a_mins[row + 1]);
            let vs_a2 = _mm256_set1_ps(sc.a_scales[row + 2]);
            let vmin2 = _mm256_set1_ps(sc.a_mins[row + 2]);
            let vs_a3 = _mm256_set1_ps(sc.a_scales[row + 3]);
            let vmin3 = _mm256_set1_ps(sc.a_mins[row + 3]);

            let o0 = o_ptr.add(row * out_dim);
            let o1 = o_ptr.add((row + 1) * out_dim);
            let o2 = o_ptr.add((row + 2) * out_dim);
            let o3 = o_ptr.add((row + 3) * out_dim);

            for blk in 0..num_blocks {
                let mut c00 = _mm256_setzero_si256();
                let mut c01 = _mm256_setzero_si256();
                let mut c10 = _mm256_setzero_si256();
                let mut c11 = _mm256_setzero_si256();
                let mut c20 = _mm256_setzero_si256();
                let mut c21 = _mm256_setzero_si256();
                let mut c30 = _mm256_setzero_si256();
                let mut c31 = _mm256_setzero_si256();

                let mut p = b_ptr.add(blk * k_groups * 64);
                let mut k4 = 0usize;
                while k4 + 2 <= k_groups {
                    let b0 = _mm256_loadu_si256(p as *const __m256i);
                    let b1 = _mm256_loadu_si256(p.add(32) as *const __m256i);
                    let av0 = _mm256_set1_epi32(std::ptr::read_unaligned(a0.add(k4 * 4) as *const i32));
                    let av1 = _mm256_set1_epi32(std::ptr::read_unaligned(a1.add(k4 * 4) as *const i32));
                    let av2 = _mm256_set1_epi32(std::ptr::read_unaligned(a2.add(k4 * 4) as *const i32));
                    let av3 = _mm256_set1_epi32(std::ptr::read_unaligned(a3.add(k4 * 4) as *const i32));
                    c00 = _mm256_dpbusd_avx_epi32(c00, av0, b0);
                    c01 = _mm256_dpbusd_avx_epi32(c01, av0, b1);
                    c10 = _mm256_dpbusd_avx_epi32(c10, av1, b0);
                    c11 = _mm256_dpbusd_avx_epi32(c11, av1, b1);
                    c20 = _mm256_dpbusd_avx_epi32(c20, av2, b0);
                    c21 = _mm256_dpbusd_avx_epi32(c21, av2, b1);
                    c30 = _mm256_dpbusd_avx_epi32(c30, av3, b0);
                    c31 = _mm256_dpbusd_avx_epi32(c31, av3, b1);

                    let b0n = _mm256_loadu_si256(p.add(64) as *const __m256i);
                    let b1n = _mm256_loadu_si256(p.add(96) as *const __m256i);
                    let av0n = _mm256_set1_epi32(std::ptr::read_unaligned(a0.add(k4 * 4 + 4) as *const i32));
                    let av1n = _mm256_set1_epi32(std::ptr::read_unaligned(a1.add(k4 * 4 + 4) as *const i32));
                    let av2n = _mm256_set1_epi32(std::ptr::read_unaligned(a2.add(k4 * 4 + 4) as *const i32));
                    let av3n = _mm256_set1_epi32(std::ptr::read_unaligned(a3.add(k4 * 4 + 4) as *const i32));
                    c00 = _mm256_dpbusd_avx_epi32(c00, av0n, b0n);
                    c01 = _mm256_dpbusd_avx_epi32(c01, av0n, b1n);
                    c10 = _mm256_dpbusd_avx_epi32(c10, av1n, b0n);
                    c11 = _mm256_dpbusd_avx_epi32(c11, av1n, b1n);
                    c20 = _mm256_dpbusd_avx_epi32(c20, av2n, b0n);
                    c21 = _mm256_dpbusd_avx_epi32(c21, av2n, b1n);
                    c30 = _mm256_dpbusd_avx_epi32(c30, av3n, b0n);
                    c31 = _mm256_dpbusd_avx_epi32(c31, av3n, b1n);

                    p = p.add(128);
                    k4 += 2;
                }
                if k4 < k_groups {
                    let b0 = _mm256_loadu_si256(p as *const __m256i);
                    let b1 = _mm256_loadu_si256(p.add(32) as *const __m256i);
                    let av0 = _mm256_set1_epi32(std::ptr::read_unaligned(a0.add(k4 * 4) as *const i32));
                    let av1 = _mm256_set1_epi32(std::ptr::read_unaligned(a1.add(k4 * 4) as *const i32));
                    let av2 = _mm256_set1_epi32(std::ptr::read_unaligned(a2.add(k4 * 4) as *const i32));
                    let av3 = _mm256_set1_epi32(std::ptr::read_unaligned(a3.add(k4 * 4) as *const i32));
                    c00 = _mm256_dpbusd_avx_epi32(c00, av0, b0);
                    c01 = _mm256_dpbusd_avx_epi32(c01, av0, b1);
                    c10 = _mm256_dpbusd_avx_epi32(c10, av1, b0);
                    c11 = _mm256_dpbusd_avx_epi32(c11, av1, b1);
                    c20 = _mm256_dpbusd_avx_epi32(c20, av2, b0);
                    c21 = _mm256_dpbusd_avx_epi32(c21, av2, b1);
                    c30 = _mm256_dpbusd_avx_epi32(c30, av3, b0);
                    c31 = _mm256_dpbusd_avx_epi32(c31, av3, b1);
                }

                let col = blk * 16;
                let sw0 = _mm256_loadu_ps(sw_ptr.add(col));
                let sw1 = _mm256_loadu_ps(sw_ptr.add(col + 8));
                let rb0 = _mm256_loadu_ps(rb_ptr.add(col));
                let rb1 = _mm256_loadu_ps(rb_ptr.add(col + 8));

                _mm256_storeu_ps(
                    o0.add(col),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c00), _mm256_mul_ps(vs_a0, sw0), _mm256_mul_ps(vmin0, rb0)),
                );
                _mm256_storeu_ps(
                    o0.add(col + 8),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c01), _mm256_mul_ps(vs_a0, sw1), _mm256_mul_ps(vmin0, rb1)),
                );
                _mm256_storeu_ps(
                    o1.add(col),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c10), _mm256_mul_ps(vs_a1, sw0), _mm256_mul_ps(vmin1, rb0)),
                );
                _mm256_storeu_ps(
                    o1.add(col + 8),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c11), _mm256_mul_ps(vs_a1, sw1), _mm256_mul_ps(vmin1, rb1)),
                );
                _mm256_storeu_ps(
                    o2.add(col),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c20), _mm256_mul_ps(vs_a2, sw0), _mm256_mul_ps(vmin2, rb0)),
                );
                _mm256_storeu_ps(
                    o2.add(col + 8),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c21), _mm256_mul_ps(vs_a2, sw1), _mm256_mul_ps(vmin2, rb1)),
                );
                _mm256_storeu_ps(
                    o3.add(col),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c30), _mm256_mul_ps(vs_a3, sw0), _mm256_mul_ps(vmin3, rb0)),
                );
                _mm256_storeu_ps(
                    o3.add(col + 8),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c31), _mm256_mul_ps(vs_a3, sw1), _mm256_mul_ps(vmin3, rb1)),
                );
            }
            row += 4;
        }

        if row + 2 <= m {
            let a0 = a_ptr.add(row * k);
            let a1 = a_ptr.add((row + 1) * k);
            let vs_a0 = _mm256_set1_ps(sc.a_scales[row]);
            let vmin0 = _mm256_set1_ps(sc.a_mins[row]);
            let vs_a1 = _mm256_set1_ps(sc.a_scales[row + 1]);
            let vmin1 = _mm256_set1_ps(sc.a_mins[row + 1]);
            let o0 = o_ptr.add(row * out_dim);
            let o1 = o_ptr.add((row + 1) * out_dim);

            for blk in 0..num_blocks {
                let mut c00 = _mm256_setzero_si256();
                let mut c01 = _mm256_setzero_si256();
                let mut c10 = _mm256_setzero_si256();
                let mut c11 = _mm256_setzero_si256();
                let mut p = b_ptr.add(blk * k_groups * 64);
                for k4 in 0..k_groups {
                    let b0 = _mm256_loadu_si256(p as *const __m256i);
                    let b1 = _mm256_loadu_si256(p.add(32) as *const __m256i);
                    let av0 = _mm256_set1_epi32(std::ptr::read_unaligned(a0.add(k4 * 4) as *const i32));
                    let av1 = _mm256_set1_epi32(std::ptr::read_unaligned(a1.add(k4 * 4) as *const i32));
                    c00 = _mm256_dpbusd_avx_epi32(c00, av0, b0);
                    c01 = _mm256_dpbusd_avx_epi32(c01, av0, b1);
                    c10 = _mm256_dpbusd_avx_epi32(c10, av1, b0);
                    c11 = _mm256_dpbusd_avx_epi32(c11, av1, b1);
                    p = p.add(64);
                }
                let col = blk * 16;
                let sw0 = _mm256_loadu_ps(sw_ptr.add(col));
                let sw1 = _mm256_loadu_ps(sw_ptr.add(col + 8));
                let rb0 = _mm256_loadu_ps(rb_ptr.add(col));
                let rb1 = _mm256_loadu_ps(rb_ptr.add(col + 8));
                _mm256_storeu_ps(
                    o0.add(col),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c00), _mm256_mul_ps(vs_a0, sw0), _mm256_mul_ps(vmin0, rb0)),
                );
                _mm256_storeu_ps(
                    o0.add(col + 8),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c01), _mm256_mul_ps(vs_a0, sw1), _mm256_mul_ps(vmin0, rb1)),
                );
                _mm256_storeu_ps(
                    o1.add(col),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c10), _mm256_mul_ps(vs_a1, sw0), _mm256_mul_ps(vmin1, rb0)),
                );
                _mm256_storeu_ps(
                    o1.add(col + 8),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c11), _mm256_mul_ps(vs_a1, sw1), _mm256_mul_ps(vmin1, rb1)),
                );
            }
            row += 2;
        }

        if row < m {
            let a0 = a_ptr.add(row * k);
            let vs_a0 = _mm256_set1_ps(sc.a_scales[row]);
            let vmin0 = _mm256_set1_ps(sc.a_mins[row]);
            let o0 = o_ptr.add(row * out_dim);

            for blk in 0..num_blocks {
                let mut c0 = _mm256_setzero_si256();
                let mut c1 = _mm256_setzero_si256();
                let mut p = b_ptr.add(blk * k_groups * 64);
                for k4 in 0..k_groups {
                    let b0 = _mm256_loadu_si256(p as *const __m256i);
                    let b1 = _mm256_loadu_si256(p.add(32) as *const __m256i);
                    let av0 = _mm256_set1_epi32(std::ptr::read_unaligned(a0.add(k4 * 4) as *const i32));
                    c0 = _mm256_dpbusd_avx_epi32(c0, av0, b0);
                    c1 = _mm256_dpbusd_avx_epi32(c1, av0, b1);
                    p = p.add(64);
                }
                let col = blk * 16;
                let sw0 = _mm256_loadu_ps(sw_ptr.add(col));
                let sw1 = _mm256_loadu_ps(sw_ptr.add(col + 8));
                let rb0 = _mm256_loadu_ps(rb_ptr.add(col));
                let rb1 = _mm256_loadu_ps(rb_ptr.add(col + 8));
                _mm256_storeu_ps(
                    o0.add(col),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c0), _mm256_mul_ps(vs_a0, sw0), _mm256_mul_ps(vmin0, rb0)),
                );
                _mm256_storeu_ps(
                    o0.add(col + 8),
                    _mm256_fmadd_ps(_mm256_cvtepi32_ps(c1), _mm256_mul_ps(vs_a0, sw1), _mm256_mul_ps(vmin0, rb1)),
                );
            }
        }
    });
}

pub fn gemm_packed(a: &[f32], m: usize, k: usize, b: &PackedMatrix, out: &mut [f32]) {
    assert_eq!(k, b.in_dim);
    assert_eq!(a.len(), m * k);
    assert_eq!(out.len(), m * b.out_dim);
    if m == 0 {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        match &b.panels {
            PackedPanels::I8Vnni {
                panels,
                scales,
                row_bias,
            } => {
                if is_x86_feature_detected!("avx2")
                    && is_x86_feature_detected!("fma")
                    && is_x86_feature_detected!("avxvnni")
                {
                    // SAFETY: CPU feature detection verified AVX2 + FMA + AVX-VNNI, and slice bounds were asserted above.
                    unsafe {
                        gemm_packed_i8_avxvnni(
                            a, m, k, panels, scales, row_bias, b.out_dim, out,
                        );
                    }
                    return;
                }
            }
            PackedPanels::F32(panels) => {
                if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                    // SAFETY: CPU feature detection verified AVX2 + FMA, and slice bounds were asserted above.
                    unsafe {
                        gemm_packed_f32_avx2(a, m, k, panels, b.out_dim, out);
                    }
                    return;
                }
            }
        }
    }
    gemm_packed_scalar(a, m, k, b, out);
}

#[inline]
fn dot_f32_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn dot_f32_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;

    let len = a.len();
    let ap = a.as_ptr();
    let bp = b.as_ptr();
    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();
    let mut i = 0usize;
    while i + 16 <= len {
        let a0 = _mm256_loadu_ps(ap.add(i));
        let b0 = _mm256_loadu_ps(bp.add(i));
        let a1 = _mm256_loadu_ps(ap.add(i + 8));
        let b1 = _mm256_loadu_ps(bp.add(i + 8));
        acc0 = _mm256_fmadd_ps(a0, b0, acc0);
        acc1 = _mm256_fmadd_ps(a1, b1, acc1);
        i += 16;
    }
    let sum8 = _mm256_add_ps(acc0, acc1);
    let hi = _mm256_extractf128_ps(sum8, 1);
    let lo = _mm256_castps256_ps128(sum8);
    let sum4 = _mm_add_ps(lo, hi);
    let shuf = _mm_movehdup_ps(sum4);
    let sums = _mm_add_ps(sum4, shuf);
    let shuf2 = _mm_movehl_ps(sums, sums);
    let mut total = _mm_cvtss_f32(_mm_add_ss(sums, shuf2));
    while i < len {
        total += *ap.add(i) * *bp.add(i);
        i += 1;
    }
    total
}

#[inline]
fn axpy_f32_scalar(dst: &mut [f32], alpha: f32, src: &[f32]) {
    debug_assert_eq!(dst.len(), src.len());
    for (d, &s) in dst.iter_mut().zip(src.iter()) {
        *d += alpha * s;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
#[inline]
unsafe fn hsum256_ps(v: std::arch::x86_64::__m256) -> f32 {
    use std::arch::x86_64::*;
    let hi = _mm256_extractf128_ps(v, 1);
    let lo = _mm256_castps256_ps128(v);
    let sum4 = _mm_add_ps(lo, hi);
    let shuf = _mm_movehdup_ps(sum4);
    let sums = _mm_add_ps(sum4, shuf);
    let shuf2 = _mm_movehl_ps(sums, sums);
    _mm_cvtss_f32(_mm_add_ss(sums, shuf2))
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
#[inline]
unsafe fn fast_exp_ps_avx2(x: std::arch::x86_64::__m256) -> std::arch::x86_64::__m256 {
    use std::arch::x86_64::*;

    let lo = _mm256_set1_ps(-87.0_f32);
    let hi = _mm256_set1_ps(88.0_f32);
    let x = _mm256_min_ps(_mm256_max_ps(x, lo), hi);

    let log2e = _mm256_set1_ps(1.44269504_f32);
    let ln2_hi = _mm256_set1_ps(0.693145751953125_f32);
    let ln2_lo = _mm256_set1_ps(1.4286068203094172e-6_f32);

    let n = _mm256_round_ps::<{ _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC }>(_mm256_mul_ps(
        x, log2e,
    ));
    let r = _mm256_fnmadd_ps(n, ln2_lo, _mm256_fnmadd_ps(n, ln2_hi, x));

    let c1 = _mm256_set1_ps(1.0_f32);
    let c2 = _mm256_set1_ps(0.5_f32);
    let c3 = _mm256_set1_ps(0.16666667_f32);
    let c4 = _mm256_set1_ps(0.041666668_f32);
    let c5 = _mm256_set1_ps(0.008333334_f32);
    let c6 = _mm256_set1_ps(0.0013888889_f32);

    let mut p = _mm256_fmadd_ps(c6, r, c5);
    p = _mm256_fmadd_ps(p, r, c4);
    p = _mm256_fmadd_ps(p, r, c3);
    p = _mm256_fmadd_ps(p, r, c2);
    p = _mm256_fmadd_ps(p, r, c1);
    p = _mm256_fmadd_ps(p, r, c1);

    let n_int = _mm256_cvtps_epi32(n);
    let exp_bits = _mm256_slli_epi32::<23>(_mm256_add_epi32(n_int, _mm256_set1_epi32(127)));
    let pow2n = _mm256_castsi256_ps(exp_bits);
    _mm256_mul_ps(p, pow2n)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn add_inplace_avx2(dst: &mut [f32], src: &[f32]) {
    use std::arch::x86_64::*;
    let len = dst.len();
    let dp = dst.as_mut_ptr();
    let sp = src.as_ptr();
    let mut i = 0usize;
    while i + 16 <= len {
        let d0 = _mm256_loadu_ps(dp.add(i));
        let s0 = _mm256_loadu_ps(sp.add(i));
        let d1 = _mm256_loadu_ps(dp.add(i + 8));
        let s1 = _mm256_loadu_ps(sp.add(i + 8));
        _mm256_storeu_ps(dp.add(i), _mm256_add_ps(d0, s0));
        _mm256_storeu_ps(dp.add(i + 8), _mm256_add_ps(d1, s1));
        i += 16;
    }
    while i + 8 <= len {
        let d0 = _mm256_loadu_ps(dp.add(i));
        let s0 = _mm256_loadu_ps(sp.add(i));
        _mm256_storeu_ps(dp.add(i), _mm256_add_ps(d0, s0));
        i += 8;
    }
    while i < len {
        *dp.add(i) += *sp.add(i);
        i += 1;
    }
}

fn add_inplace(dst: &mut [f32], src: &[f32], has_avx2: bool) {
    debug_assert_eq!(dst.len(), src.len());
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2 {
            // SAFETY: caller verified AVX2 + FMA and equal slice lengths.
            unsafe {
                add_inplace_avx2(dst, src);
            }
            return;
        }
    }
    let _ = has_avx2;
    for (d, &s) in dst.iter_mut().zip(src.iter()) {
        *d += s;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn rmsnorm_batch_avx2(
    x: &[f32],
    n_rows: usize,
    dim: usize,
    weight: &[f32],
    out: &mut [f32],
) {
    use std::arch::x86_64::*;

    let inv_dim = 1.0_f32 / (dim as f32);
    let xp = x.as_ptr();
    let wp = weight.as_ptr();
    let op = out.as_mut_ptr();

    for row in 0..n_rows {
        let src = xp.add(row * dim);
        let dst = op.add(row * dim);
        let mut acc0 = _mm256_setzero_ps();
        let mut acc1 = _mm256_setzero_ps();
        let mut i = 0usize;
        while i + 16 <= dim {
            let v0 = _mm256_loadu_ps(src.add(i));
            let v1 = _mm256_loadu_ps(src.add(i + 8));
            acc0 = _mm256_fmadd_ps(v0, v0, acc0);
            acc1 = _mm256_fmadd_ps(v1, v1, acc1);
            i += 16;
        }
        let mut sq_sum = hsum256_ps(_mm256_add_ps(acc0, acc1));
        while i < dim {
            let v = *src.add(i);
            sq_sum += v * v;
            i += 1;
        }

        let scale = 1.0_f32 / (sq_sum * inv_dim + 1e-6_f32).sqrt();
        let vscale = _mm256_set1_ps(scale);
        let mut j = 0usize;
        while j + 16 <= dim {
            let s0 = _mm256_loadu_ps(src.add(j));
            let w0 = _mm256_loadu_ps(wp.add(j));
            let s1 = _mm256_loadu_ps(src.add(j + 8));
            let w1 = _mm256_loadu_ps(wp.add(j + 8));
            _mm256_storeu_ps(dst.add(j), _mm256_mul_ps(_mm256_mul_ps(s0, vscale), w0));
            _mm256_storeu_ps(
                dst.add(j + 8),
                _mm256_mul_ps(_mm256_mul_ps(s1, vscale), w1),
            );
            j += 16;
        }
        while j + 8 <= dim {
            let s0 = _mm256_loadu_ps(src.add(j));
            let w0 = _mm256_loadu_ps(wp.add(j));
            _mm256_storeu_ps(dst.add(j), _mm256_mul_ps(_mm256_mul_ps(s0, vscale), w0));
            j += 8;
        }
        while j < dim {
            *dst.add(j) = *src.add(j) * scale * *wp.add(j);
            j += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn rope_qkv_batch_avx2(
    qkv: &mut [f32],
    node_depths: &[usize],
    n_heads: usize,
    head_dim: usize,
    d_model: usize,
    max_seq_len: usize,
    rope_cos: &[f32],
    rope_sin: &[f32],
) {
    use std::arch::x86_64::*;

    let qkv_stride = 3 * d_model;
    let half = head_dim / 2;
    let qp = qkv.as_mut_ptr();
    let cos_ptr = rope_cos.as_ptr();
    let sin_ptr = rope_sin.as_ptr();

    for (u, &depth) in node_depths.iter().enumerate() {
        let clamped = depth.min(max_seq_len - 1);
        let c_base = cos_ptr.add(clamped * half);
        let s_base = sin_ptr.add(clamped * half);
        let u_base = qp.add(u * qkv_stride);

        for h in 0..n_heads {
            let hd_off = h * head_dim;
            for offset in [hd_off, d_model + hd_off] {
                let ptr = u_base.add(offset);
                let mut j = 0usize;
                while j + 8 <= half {
                    let x0 = _mm256_loadu_ps(ptr.add(j));
                    let x1 = _mm256_loadu_ps(ptr.add(j + half));
                    let cos = _mm256_loadu_ps(c_base.add(j));
                    let sin = _mm256_loadu_ps(s_base.add(j));
                    // y0 = x0 * cos - x1 * sin ; y1 = x0 * sin + x1 * cos
                    let y0 = _mm256_fmsub_ps(x0, cos, _mm256_mul_ps(x1, sin));
                    let y1 = _mm256_fmadd_ps(x0, sin, _mm256_mul_ps(x1, cos));
                    _mm256_storeu_ps(ptr.add(j), y0);
                    _mm256_storeu_ps(ptr.add(j + half), y1);
                    j += 8;
                }
                while j < half {
                    let x0 = *ptr.add(j);
                    let x1 = *ptr.add(j + half);
                    let c = *c_base.add(j);
                    let s = *s_base.add(j);
                    *ptr.add(j) = x0 * c - x1 * s;
                    *ptr.add(j + half) = x0 * s + x1 * c;
                    j += 1;
                }
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn swiglu_batch_avx2(gate_up: &[f32], n_nodes: usize, d_ff: usize, act: &mut [f32]) {
    use std::arch::x86_64::*;

    let gu_ptr = gate_up.as_ptr();
    let act_ptr = act.as_mut_ptr();
    let zero = _mm256_setzero_ps();
    let one = _mm256_set1_ps(1.0_f32);

    for u in 0..n_nodes {
        let gate = gu_ptr.add(u * 2 * d_ff);
        let up = gate.add(d_ff);
        let dst = act_ptr.add(u * d_ff);
        let mut j = 0usize;
        while j + 8 <= d_ff {
            let g = _mm256_loadu_ps(gate.add(j));
            let u_val = _mm256_loadu_ps(up.add(j));
            let e = fast_exp_ps_avx2(_mm256_sub_ps(zero, g));
            let denom = _mm256_add_ps(one, e);
            let silu = _mm256_div_ps(g, denom);
            _mm256_storeu_ps(dst.add(j), _mm256_mul_ps(silu, u_val));
            j += 8;
        }
        while j < d_ff {
            let g = *gate.add(j);
            let silu = g / (1.0_f32 + (-g).exp());
            *dst.add(j) = silu * *up.add(j);
            j += 1;
        }
    }
}

fn swiglu_batch(gate_up: &[f32], n_nodes: usize, d_ff: usize, act: &mut [f32], has_avx2: bool) {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2 {
            // SAFETY: caller verified AVX2 + FMA and slice capacities.
            unsafe {
                swiglu_batch_avx2(gate_up, n_nodes, d_ff, act);
            }
            return;
        }
    }
    let _ = has_avx2;
    for u in 0..n_nodes {
        let gu = &gate_up[u * 2 * d_ff..(u + 1) * 2 * d_ff];
        let gate = &gu[..d_ff];
        let up = &gu[d_ff..];
        let dst = &mut act[u * d_ff..(u + 1) * d_ff];
        for j in 0..d_ff {
            let g = gate[j];
            let silu = g / (1.0_f32 + (-g).exp());
            dst[j] = silu * up[j];
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
#[inline]
unsafe fn dot64_avx2(a: *const f32, b: *const f32) -> f32 {
    use std::arch::x86_64::*;

    let mut acc0 = _mm256_mul_ps(_mm256_loadu_ps(a), _mm256_loadu_ps(b));
    let mut acc1 = _mm256_mul_ps(_mm256_loadu_ps(a.add(8)), _mm256_loadu_ps(b.add(8)));
    let mut acc2 = _mm256_mul_ps(_mm256_loadu_ps(a.add(16)), _mm256_loadu_ps(b.add(16)));
    let mut acc3 = _mm256_mul_ps(_mm256_loadu_ps(a.add(24)), _mm256_loadu_ps(b.add(24)));

    acc0 = _mm256_fmadd_ps(_mm256_loadu_ps(a.add(32)), _mm256_loadu_ps(b.add(32)), acc0);
    acc1 = _mm256_fmadd_ps(_mm256_loadu_ps(a.add(40)), _mm256_loadu_ps(b.add(40)), acc1);
    acc2 = _mm256_fmadd_ps(_mm256_loadu_ps(a.add(48)), _mm256_loadu_ps(b.add(48)), acc2);
    acc3 = _mm256_fmadd_ps(_mm256_loadu_ps(a.add(56)), _mm256_loadu_ps(b.add(56)), acc3);

    let sum = _mm256_add_ps(_mm256_add_ps(acc0, acc1), _mm256_add_ps(acc2, acc3));
    hsum256_ps(sum)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
#[allow(clippy::too_many_arguments)]
unsafe fn tree_attention_avx2(
    qkv: &[f32],
    cached_len: usize,
    cached_k: &[f32],
    cached_v: &[f32],
    anc_offsets: &[usize],
    anc_nodes: &[usize],
    n_active: usize,
    n_heads: usize,
    head_dim: usize,
    d_model: usize,
    scale: f32,
    attn_scores: &mut [f32],
    attn_out: &mut [f32],
) {
    use std::arch::x86_64::*;

    let qkv_stride = 3 * d_model;
    let qkv_ptr = qkv.as_ptr();
    let ck_ptr = cached_k.as_ptr();
    let cv_ptr = cached_v.as_ptr();
    let s_ptr = attn_scores.as_mut_ptr();
    let out_ptr = attn_out.as_mut_ptr();

    if head_dim != 64 {
        attn_out.fill(0.0);
    }

    for u in 0..n_active {
        let chain = &anc_nodes[anc_offsets[u]..anc_offsets[u + 1]];
        let chain_len = chain.len();
        let total_len = cached_len + chain_len;
        let u_q_base = u * qkv_stride;
        let u_out_base = u * d_model;

        for h in 0..n_heads {
            let hd_off = h * head_dim;
            let q_ptr = qkv_ptr.add(u_q_base + hd_off);
            let q_slice = std::slice::from_raw_parts(q_ptr, head_dim);

            let mut max_s = f32::NEG_INFINITY;
            if head_dim == 64 {
                for p in 0..cached_len {
                    let s = dot64_avx2(q_ptr, ck_ptr.add(p * d_model + hd_off)) * scale;
                    *s_ptr.add(p) = s;
                    if s > max_s {
                        max_s = s;
                    }
                }
                for (idx, &anc) in chain.iter().enumerate() {
                    let s = dot64_avx2(q_ptr, qkv_ptr.add(anc * qkv_stride + d_model + hd_off)) * scale;
                    *s_ptr.add(cached_len + idx) = s;
                    if s > max_s {
                        max_s = s;
                    }
                }
            } else {
                for p in 0..cached_len {
                    let k_slice =
                        std::slice::from_raw_parts(ck_ptr.add(p * d_model + hd_off), head_dim);
                    let s = dot_f32_avx2(q_slice, k_slice) * scale;
                    *s_ptr.add(p) = s;
                    if s > max_s {
                        max_s = s;
                    }
                }
                for (idx, &anc) in chain.iter().enumerate() {
                    let k_slice = std::slice::from_raw_parts(
                        qkv_ptr.add(anc * qkv_stride + d_model + hd_off),
                        head_dim,
                    );
                    let s = dot_f32_avx2(q_slice, k_slice) * scale;
                    *s_ptr.add(cached_len + idx) = s;
                    if s > max_s {
                        max_s = s;
                    }
                }
            }

            // Softmax using vectorized exp_ps.
            let vmax = _mm256_set1_ps(max_s);
            let mut vsum = _mm256_setzero_ps();
            let mut j = 0usize;
            while j + 8 <= total_len {
                let sv = _mm256_loadu_ps(s_ptr.add(j));
                let ev = fast_exp_ps_avx2(_mm256_sub_ps(sv, vmax));
                _mm256_storeu_ps(s_ptr.add(j), ev);
                vsum = _mm256_add_ps(vsum, ev);
                j += 8;
            }
            let mut sum_exp = hsum256_ps(vsum);
            while j < total_len {
                let e = (*s_ptr.add(j) - max_s).exp();
                *s_ptr.add(j) = e;
                sum_exp += e;
                j += 1;
            }
            let inv_sum = 1.0_f32 / sum_exp.max(1e-12);

            if head_dim == 64 {
                let mut o0 = _mm256_setzero_ps();
                let mut o1 = _mm256_setzero_ps();
                let mut o2 = _mm256_setzero_ps();
                let mut o3 = _mm256_setzero_ps();
                let mut o4 = _mm256_setzero_ps();
                let mut o5 = _mm256_setzero_ps();
                let mut o6 = _mm256_setzero_ps();
                let mut o7 = _mm256_setzero_ps();

                for p in 0..cached_len {
                    let w = _mm256_set1_ps(*s_ptr.add(p) * inv_sum);
                    let vp = cv_ptr.add(p * d_model + hd_off);
                    o0 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp), o0);
                    o1 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(8)), o1);
                    o2 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(16)), o2);
                    o3 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(24)), o3);
                    o4 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(32)), o4);
                    o5 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(40)), o5);
                    o6 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(48)), o6);
                    o7 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(56)), o7);
                }
                for (idx, &anc) in chain.iter().enumerate() {
                    let w = _mm256_set1_ps(*s_ptr.add(cached_len + idx) * inv_sum);
                    let vp = qkv_ptr.add(anc * qkv_stride + 2 * d_model + hd_off);
                    o0 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp), o0);
                    o1 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(8)), o1);
                    o2 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(16)), o2);
                    o3 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(24)), o3);
                    o4 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(32)), o4);
                    o5 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(40)), o5);
                    o6 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(48)), o6);
                    o7 = _mm256_fmadd_ps(w, _mm256_loadu_ps(vp.add(56)), o7);
                }

                let dst = out_ptr.add(u_out_base + hd_off);
                _mm256_storeu_ps(dst, o0);
                _mm256_storeu_ps(dst.add(8), o1);
                _mm256_storeu_ps(dst.add(16), o2);
                _mm256_storeu_ps(dst.add(24), o3);
                _mm256_storeu_ps(dst.add(32), o4);
                _mm256_storeu_ps(dst.add(40), o5);
                _mm256_storeu_ps(dst.add(48), o6);
                _mm256_storeu_ps(dst.add(56), o7);
            } else {
                let dst = out_ptr.add(u_out_base + hd_off);
                for p in 0..cached_len {
                    let w = *s_ptr.add(p) * inv_sum;
                    let vw = _mm256_set1_ps(w);
                    let vp = cv_ptr.add(p * d_model + hd_off);
                    let mut d = 0usize;
                    while d + 8 <= head_dim {
                        let cur = _mm256_loadu_ps(dst.add(d));
                        let vv = _mm256_loadu_ps(vp.add(d));
                        _mm256_storeu_ps(dst.add(d), _mm256_fmadd_ps(vw, vv, cur));
                        d += 8;
                    }
                    while d < head_dim {
                        *dst.add(d) += w * *vp.add(d);
                        d += 1;
                    }
                }
                for (idx, &anc) in chain.iter().enumerate() {
                    let w = *s_ptr.add(cached_len + idx) * inv_sum;
                    let vw = _mm256_set1_ps(w);
                    let vp = qkv_ptr.add(anc * qkv_stride + 2 * d_model + hd_off);
                    let mut d = 0usize;
                    while d + 8 <= head_dim {
                        let cur = _mm256_loadu_ps(dst.add(d));
                        let vv = _mm256_loadu_ps(vp.add(d));
                        _mm256_storeu_ps(dst.add(d), _mm256_fmadd_ps(vw, vv, cur));
                        d += 8;
                    }
                    while d < head_dim {
                        *dst.add(d) += w * *vp.add(d);
                        d += 1;
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransformerLayer {
    pub norm1: Vec<f32>,
    pub w_qkv: PackedMatrix,
    pub w_o: PackedMatrix,
    pub norm2: Vec<f32>,
    pub w_gate_up: PackedMatrix,
    pub w_down: PackedMatrix,
}

#[derive(Debug)]
struct CachedContext {
    tokens: Vec<u16>,
    layers_k: Vec<Vec<f32>>,
    layers_v: Vec<Vec<f32>>,
    ctx_end_normed: Vec<f32>,
}

const MAX_KV_CACHE_ENTRIES: usize = 16;

#[derive(Debug, Default)]
struct ContextKvCache {
    entries: Vec<Arc<CachedContext>>,
}

impl ContextKvCache {
    fn lookup_longest(&mut self, ctx_tokens: &[u16]) -> Option<Arc<CachedContext>> {
        let mut best_idx: Option<usize> = None;
        let mut best_len = 0usize;
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.tokens.len() <= ctx_tokens.len() && ctx_tokens.starts_with(&entry.tokens) {
                if entry.tokens.len() > best_len {
                    best_len = entry.tokens.len();
                    best_idx = Some(i);
                }
            }
        }
        if let Some(idx) = best_idx {
            let item = self.entries.remove(idx);
            self.entries.insert(0, Arc::clone(&item));
            Some(item)
        } else {
            None
        }
    }

    fn insert(&mut self, entry: Arc<CachedContext>) {
        if let Some(pos) = self.entries.iter().position(|e| e.tokens == entry.tokens) {
            self.entries.remove(pos);
        }
        self.entries.insert(0, entry);
        if self.entries.len() > MAX_KV_CACHE_ENTRIES {
            self.entries.truncate(MAX_KV_CACHE_ENTRIES);
        }
    }
}

#[derive(Default)]
struct RerankerWorkspace {
    trie_tok: Vec<u16>,
    trie_parent: Vec<usize>,
    trie_depth: Vec<usize>,
    trie_children: Vec<Vec<(u16, usize)>>,
    root_children: Vec<(u16, usize)>,
    cand_paths: Vec<Vec<usize>>,
    anc_offsets: Vec<usize>,
    anc_nodes: Vec<usize>,
    scratch_chain: Vec<usize>,
    x: Vec<f32>,
    normed: Vec<f32>,
    qkv: Vec<f32>,
    attn_out: Vec<f32>,
    proj: Vec<f32>,
    ff_gate_up: Vec<f32>,
    ff_act: Vec<f32>,
    attn_scores: Vec<f32>,
    new_layers_k: Vec<Vec<f32>>,
    new_layers_v: Vec<Vec<f32>>,
}

thread_local! {
    static WORKSPACE: RefCell<RerankerWorkspace> = RefCell::new(RerankerWorkspace::default());
}

#[derive(Debug)]
pub struct Reranker {
    pub vocab_size: usize,
    pub d_model: usize,
    pub n_layers: usize,
    pub n_heads: usize,
    pub head_dim: usize,
    pub d_ff: usize,
    pub max_seq_len: usize,
    pub rope_theta: f32,
    pub unk_penalty: f32,

    bmp_to_id: Vec<u16>,
    extra_to_id: HashMap<char, u16>,
    pub tok_emb: Vec<f32>,
    pub lm_head_bias: Vec<f32>,
    pub layers: Vec<TransformerLayer>,
    pub norm_f: Vec<f32>,
    rope_cos: Vec<f32>,
    rope_sin: Vec<f32>,
    kv_cache: Mutex<ContextKvCache>,
}

impl Reranker {
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        vocab_scalars: Vec<u32>,
        d_model: usize,
        n_heads: usize,
        d_ff: usize,
        max_seq_len: usize,
        rope_theta: f32,
        unk_penalty: f32,
        tok_emb: Vec<f32>,
        lm_head_bias: Vec<f32>,
        layers: Vec<TransformerLayer>,
        norm_f: Vec<f32>,
    ) -> io::Result<Self> {
        let vocab_size = vocab_scalars.len();
        if vocab_size < 5 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "vocab_size < 5"));
        }
        if d_model == 0 || d_model % 16 != 0 || n_heads == 0 || d_model % n_heads != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid d_model/n_heads"));
        }
        let head_dim = d_model / n_heads;
        if head_dim % 2 != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "head_dim must be even"));
        }
        if d_ff == 0 || d_ff % 16 != 0 || max_seq_len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid d_ff/max_seq_len"));
        }
        if tok_emb.len() != vocab_size * d_model || lm_head_bias.len() != vocab_size || norm_f.len() != d_model {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "embedding or norm size mismatch"));
        }

        let mut bmp_to_id = vec![0u16; 65536];
        let mut extra_to_id = HashMap::new();
        for (id, &scalar) in vocab_scalars.iter().enumerate().skip(5) {
            if let Some(ch) = char::from_u32(scalar) {
                if scalar < 65536 {
                    bmp_to_id[scalar as usize] = id as u16;
                } else {
                    extra_to_id.insert(ch, id as u16);
                }
            }
        }

        let half = head_dim / 2;
        let mut rope_cos = vec![0.0_f32; max_seq_len * half];
        let mut rope_sin = vec![0.0_f32; max_seq_len * half];
        for pos in 0..max_seq_len {
            let base = pos * half;
            for j in 0..half {
                let inv_freq = 1.0_f32 / rope_theta.powf((2 * j) as f32 / (head_dim as f32));
                let angle = (pos as f32) * inv_freq;
                rope_cos[base + j] = angle.cos();
                rope_sin[base + j] = angle.sin();
            }
        }

        let n_layers = layers.len();
        Ok(Self {
            vocab_size,
            d_model,
            n_layers,
            n_heads,
            head_dim,
            d_ff,
            max_seq_len,
            rope_theta,
            unk_penalty,
            bmp_to_id,
            extra_to_id,
            tok_emb,
            lm_head_bias,
            layers,
            norm_f,
            rope_cos,
            rope_sin,
            kv_cache: Mutex::new(ContextKvCache::default()),
        })
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut r = BufReader::with_capacity(1 << 20, file);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid reranker magic header",
            ));
        }

        let vocab_size = read_u32(&mut r)? as usize;
        let d_model = read_u32(&mut r)? as usize;
        let n_layers = read_u32(&mut r)? as usize;
        let n_heads = read_u32(&mut r)? as usize;
        let d_ff = read_u32(&mut r)? as usize;
        let max_seq_len = read_u32(&mut r)? as usize;
        let rope_theta = f32::from_bits(read_u32(&mut r)?);
        let unk_penalty = f32::from_bits(read_u32(&mut r)?);

        if d_model % 16 != 0 || d_ff % 16 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "d_model and d_ff must be multiples of 16",
            ));
        }

        let vocab_scalars = read_u32_vec(&mut r, vocab_size)?;
        let tok_emb_f16 = read_u16_vec(&mut r, vocab_size * d_model)?;
        let tok_emb: Vec<f32> = tok_emb_f16.into_iter().map(f16_to_f32).collect();
        let lm_head_bias = read_f32_vec(&mut r, vocab_size)?;

        let mut layers = Vec::with_capacity(n_layers);
        for _ in 0..n_layers {
            let norm1 = read_f32_vec(&mut r, d_model)?;
            let w_qkv_raw = read_u16_vec(&mut r, 3 * d_model * d_model)?;
            let w_qkv = PackedMatrix::from_row_major_f16(&w_qkv_raw, 3 * d_model, d_model);
            let w_o_raw = read_u16_vec(&mut r, d_model * d_model)?;
            let w_o = PackedMatrix::from_row_major_f16(&w_o_raw, d_model, d_model);
            let norm2 = read_f32_vec(&mut r, d_model)?;
            let w_gate_up_raw = read_u16_vec(&mut r, 2 * d_ff * d_model)?;
            let w_gate_up = PackedMatrix::from_row_major_f16(&w_gate_up_raw, 2 * d_ff, d_model);
            let w_down_raw = read_u16_vec(&mut r, d_model * d_ff)?;
            let w_down = PackedMatrix::from_row_major_f16_as_f32(&w_down_raw, d_model, d_ff);
            layers.push(TransformerLayer {
                norm1,
                w_qkv,
                w_o,
                norm2,
                w_gate_up,
                w_down,
            });
        }
        let norm_f = read_f32_vec(&mut r, d_model)?;

        Self::from_parts(
            vocab_scalars,
            d_model,
            n_heads,
            d_ff,
            max_seq_len,
            rope_theta,
            unk_penalty,
            tok_emb,
            lm_head_bias,
            layers,
            norm_f,
        )
    }

    #[cfg(test)]
    pub fn clear_kv_cache(&self) {
        if let Ok(mut guard) = self.kv_cache.lock() {
            guard.entries.clear();
        }
    }

    #[inline]
    pub fn char_to_id(&self, ch: char) -> u16 {
        let code = ch as u32;
        if code < 65536 {
            let id = self.bmp_to_id[code as usize];
            if id != 0 {
                id
            } else {
                UNK_ID
            }
        } else {
            self.extra_to_id.get(&ch).copied().unwrap_or(UNK_ID)
        }
    }

    pub fn context_to_tokens(&self, context_text: &str) -> Vec<u16> {
        let trimmed = context_text.trim_end();
        if trimmed.is_empty() {
            return vec![BOS_ID];
        }

        // Isolate the active sentence after the last sentence-ending punctuation mark.
        let mut start_byte = 0usize;
        for (idx, ch) in trimmed.char_indices().rev() {
            if is_sentence_end(ch) {
                start_byte = idx + ch.len_utf8();
                break;
            }
        }
        let active = &trimmed[start_byte..];

        let mut raw = Vec::new();
        for ch in active.chars() {
            if is_hanzi(ch) {
                raw.push(self.char_to_id(ch));
            } else if is_pause_punct(ch) {
                if !raw.is_empty() && raw.last().copied() != Some(PAUSE_ID) {
                    raw.push(PAUSE_ID);
                }
            } else if is_inline_wrapper(ch) || ch.is_whitespace() {
                continue;
            } else {
                // Any non-Hanzi symbol resets local sentence prefix inside the chunk.
                raw.clear();
            }
        }

        if raw.is_empty() {
            return vec![BOS_ID];
        }

        if raw.len() < MAX_CONTEXT_TOKENS {
            let mut out = Vec::with_capacity(raw.len() + 1);
            out.push(BOS_ID);
            out.extend_from_slice(&raw);
            return out;
        }

        let tail = &raw[raw.len() - MAX_CONTEXT_TOKENS..];
        if let Some(pause_idx) = tail.iter().position(|&t| t == PAUSE_ID) {
            if tail.len() - (pause_idx + 1) >= 8 {
                return tail[pause_idx + 1..].to_vec();
            }
        }
        tail.to_vec()
    }

    #[inline]
    pub fn score_token(&self, hidden: &[f32], token_id: u16) -> f32 {
        let has_avx2 = {
            #[cfg(target_arch = "x86_64")]
            {
                is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                false
            }
        };
        self.score_token_with_isa(hidden, token_id, has_avx2)
    }

    #[inline]
    fn score_token_with_isa(&self, hidden: &[f32], token_id: u16, has_avx2: bool) -> f32 {
        let tid = token_id as usize;
        let emb = &self.tok_emb[tid * self.d_model..(tid + 1) * self.d_model];
        let dot = {
            #[cfg(target_arch = "x86_64")]
            {
                if has_avx2 {
                    // SAFETY: caller verified AVX2 + FMA and equal lengths.
                    unsafe { dot_f32_avx2(hidden, emb) }
                } else {
                    dot_f32_scalar(hidden, emb)
                }
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                let _ = has_avx2;
                dot_f32_scalar(hidden, emb)
            }
        };
        let mut s = dot + self.lm_head_bias[tid];
        if token_id == UNK_ID {
            s -= self.unk_penalty;
        }
        s
    }

    fn apply_rope_qkv_batch(&self, qkv: &mut [f32], node_depths: &[usize], has_avx2: bool) {
        #[cfg(target_arch = "x86_64")]
        {
            if has_avx2 {
                // SAFETY: caller verified AVX2 + FMA and valid buffer lengths.
                unsafe {
                    rope_qkv_batch_avx2(
                        qkv,
                        node_depths,
                        self.n_heads,
                        self.head_dim,
                        self.d_model,
                        self.max_seq_len,
                        &self.rope_cos,
                        &self.rope_sin,
                    );
                }
                return;
            }
        }
        let _ = has_avx2;
        let qkv_stride = 3 * self.d_model;
        let half = self.head_dim / 2;
        for (u, &pos) in node_depths.iter().enumerate() {
            let clamped = pos.min(self.max_seq_len - 1);
            let cos = &self.rope_cos[clamped * half..(clamped + 1) * half];
            let sin = &self.rope_sin[clamped * half..(clamped + 1) * half];
            let base = u * qkv_stride;
            for h in 0..self.n_heads {
                let hd_off = h * self.head_dim;
                for off in [base + hd_off, base + self.d_model + hd_off] {
                    let vec = &mut qkv[off..off + self.head_dim];
                    for j in 0..half {
                        let x0 = vec[j];
                        let x1 = vec[j + half];
                        vec[j] = x0 * cos[j] - x1 * sin[j];
                        vec[j + half] = x0 * sin[j] + x1 * cos[j];
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_tree_attention(
        &self,
        qkv: &[f32],
        cached_len: usize,
        cached_k: &[f32],
        cached_v: &[f32],
        anc_offsets: &[usize],
        anc_nodes: &[usize],
        n_active: usize,
        scale: f32,
        attn_scores: &mut [f32],
        attn_out: &mut [f32],
        has_avx2: bool,
    ) {
        #[cfg(target_arch = "x86_64")]
        {
            if has_avx2 {
                // SAFETY: caller verified AVX2 + FMA and valid buffer sizes.
                unsafe {
                    tree_attention_avx2(
                        qkv,
                        cached_len,
                        cached_k,
                        cached_v,
                        anc_offsets,
                        anc_nodes,
                        n_active,
                        self.n_heads,
                        self.head_dim,
                        self.d_model,
                        scale,
                        attn_scores,
                        attn_out,
                    );
                }
                return;
            }
        }
        let _ = has_avx2;
        attn_out.fill(0.0);
        let d = self.d_model;
        let qkv_stride = 3 * d;
        for u in 0..n_active {
            let chain = &anc_nodes[anc_offsets[u]..anc_offsets[u + 1]];
            let chain_len = chain.len();
            let total_len = cached_len + chain_len;
            let u_q_base = u * qkv_stride;
            let u_out_base = u * d;

            for h in 0..self.n_heads {
                let hd_off = h * self.head_dim;
                let q_vec = &qkv[u_q_base + hd_off..u_q_base + hd_off + self.head_dim];

                let mut max_s = f32::NEG_INFINITY;
                for p in 0..cached_len {
                    let k_off = p * d + hd_off;
                    let s = dot_f32_scalar(q_vec, &cached_k[k_off..k_off + self.head_dim]) * scale;
                    attn_scores[p] = s;
                    if s > max_s {
                        max_s = s;
                    }
                }
                for (idx, &anc) in chain.iter().enumerate() {
                    let k_off = anc * qkv_stride + d + hd_off;
                    let s = dot_f32_scalar(q_vec, &qkv[k_off..k_off + self.head_dim]) * scale;
                    attn_scores[cached_len + idx] = s;
                    if s > max_s {
                        max_s = s;
                    }
                }

                let mut sum_exp = 0.0_f32;
                for s in &mut attn_scores[..total_len] {
                    let e = (*s - max_s).exp();
                    *s = e;
                    sum_exp += e;
                }
                let inv_sum = 1.0_f32 / sum_exp.max(1e-12);
                let out_head = &mut attn_out[u_out_base + hd_off..u_out_base + hd_off + self.head_dim];
                for p in 0..cached_len {
                    let w = attn_scores[p] * inv_sum;
                    let v_off = p * d + hd_off;
                    axpy_f32_scalar(out_head, w, &cached_v[v_off..v_off + self.head_dim]);
                }
                for (idx, &anc) in chain.iter().enumerate() {
                    let w = attn_scores[cached_len + idx] * inv_sum;
                    let v_off = anc * qkv_stride + 2 * d + hd_off;
                    axpy_f32_scalar(out_head, w, &qkv[v_off..v_off + self.head_dim]);
                }
            }
        }
    }

    /// Scores all `candidates` given `context_text` in a single shared prefix-trie forward pass.
    pub fn score_candidates(
        &self,
        context_text: &str,
        candidates: &[&str],
        neural_eos_weight: f32,
    ) -> Vec<f32> {
        if candidates.is_empty() {
            return Vec::new();
        }

        let has_avx2 = {
            #[cfg(target_arch = "x86_64")]
            {
                is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                false
            }
        };

        let ctx_tokens = self.context_to_tokens(context_text);
        let d = self.d_model;

        let cached_opt = self
            .kv_cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.lookup_longest(&ctx_tokens));
        let cached_len = cached_opt.as_ref().map_or(0, |c| c.tokens.len());
        let uncached_ctx_len = ctx_tokens.len() - cached_len;

        WORKSPACE.with(|cell| {
            let mut ws_guard = cell.borrow_mut();
            let ws = &mut *ws_guard;
            ws.trie_tok.clear();
            ws.trie_parent.clear();
            ws.trie_depth.clear();
            ws.trie_children.clear();
            ws.root_children.clear();
            ws.cand_paths.clear();

            // 1. Add any uncached context suffix nodes sequentially.
            let mut cand_root_parent = usize::MAX;
            for (k, &tok) in ctx_tokens[cached_len..].iter().enumerate() {
                let id = ws.trie_tok.len();
                let parent = if k == 0 { usize::MAX } else { id - 1 };
                if parent != usize::MAX {
                    ws.trie_children[parent].push((tok, id));
                }
                ws.trie_tok.push(tok);
                ws.trie_parent.push(parent);
                ws.trie_depth.push(cached_len + k);
                ws.trie_children.push(Vec::new());
                cand_root_parent = id;
            }

            // 2. Insert candidate sequences into active prefix trie.
            for &cand in candidates {
                let mut cur = cand_root_parent;
                let mut path = Vec::with_capacity(cand.chars().count());
                for ch in cand.chars() {
                    if !is_hanzi(ch) && ch != '，' {
                        continue;
                    }
                    let tok = if ch == '，' { PAUSE_ID } else { self.char_to_id(ch) };
                    let existing = if cur == usize::MAX {
                        ws.root_children.iter().find(|&&(t, _)| t == tok).map(|&(_, id)| id)
                    } else {
                        ws.trie_children[cur].iter().find(|&&(t, _)| t == tok).map(|&(_, id)| id)
                    };
                    if let Some(child_id) = existing {
                        cur = child_id;
                    } else {
                        let child_id = ws.trie_tok.len();
                        let depth = if cur == usize::MAX {
                            cached_len
                        } else {
                            ws.trie_depth[cur] + 1
                        };
                        if cur == usize::MAX {
                            ws.root_children.push((tok, child_id));
                        } else {
                            ws.trie_children[cur].push((tok, child_id));
                        }
                        ws.trie_tok.push(tok);
                        ws.trie_parent.push(cur);
                        ws.trie_depth.push(depth);
                        ws.trie_children.push(Vec::new());
                        cur = child_id;
                    }
                    path.push(cur);
                }
                ws.cand_paths.push(path);
            }

            let n_active = ws.trie_tok.len();
            if n_active == 0 {
                // Fast exit when context was completely cached and candidates produced zero tokens.
                if let Some(ref cached) = cached_opt {
                    let eos_score =
                        self.score_token_with_isa(&cached.ctx_end_normed, EOS_ID, has_avx2);
                    return vec![neural_eos_weight * eos_score; candidates.len()];
                }
                return vec![0.0; candidates.len()];
            }

            // 3. Precompute local causal ancestor chains `[..., u]` inside `[0..n_active]`.
            ws.anc_offsets.clear();
            ws.anc_nodes.clear();
            ws.anc_offsets.push(0usize);
            for u in 0..n_active {
                ws.scratch_chain.clear();
                let mut cur = u;
                while cur != usize::MAX {
                    ws.scratch_chain.push(cur);
                    cur = ws.trie_parent[cur];
                }
                ws.scratch_chain.reverse();
                ws.anc_nodes.extend_from_slice(&ws.scratch_chain);
                let end_len = ws.anc_nodes.len();
                ws.anc_offsets.push(end_len);
            }

            // 4. Populate initial token embeddings `[n_active, d_model]`.
            ws.x.resize(n_active * d, 0.0);
            for u in 0..n_active {
                let tid = ws.trie_tok[u] as usize;
                ws.x[u * d..(u + 1) * d].copy_from_slice(&self.tok_emb[tid * d..(tid + 1) * d]);
            }

            ws.normed.resize(n_active * d, 0.0);
            ws.qkv.resize(n_active * 3 * d, 0.0);
            ws.attn_out.resize(n_active * d, 0.0);
            ws.proj.resize(n_active * d, 0.0);
            ws.ff_gate_up.resize(n_active * 2 * self.d_ff, 0.0);
            ws.ff_act.resize(n_active * self.d_ff, 0.0);
            ws.attn_scores.resize(cached_len + n_active, 0.0);

            let capture_ctx = uncached_ctx_len > 0;
            if capture_ctx {
                ws.new_layers_k.resize_with(self.n_layers, Vec::new);
                ws.new_layers_v.resize_with(self.n_layers, Vec::new);
            }

            let scale = 1.0_f32 / (self.head_dim as f32).sqrt();
            let qkv_stride = 3 * d;
            let total_ctx_len = ctx_tokens.len();

            for (layer_idx, layer) in self.layers.iter().enumerate() {
                rmsnorm_batch(&ws.x, n_active, d, &layer.norm1, &mut ws.normed, has_avx2);
                gemm_packed(&ws.normed, n_active, d, &layer.w_qkv, &mut ws.qkv);
                self.apply_rope_qkv_batch(&mut ws.qkv, &ws.trie_depth, has_avx2);

                if capture_ctx {
                    let lk = &mut ws.new_layers_k[layer_idx];
                    let lv = &mut ws.new_layers_v[layer_idx];
                    lk.clear();
                    lk.reserve(total_ctx_len * d);
                    lv.clear();
                    lv.reserve(total_ctx_len * d);
                    if let Some(ref cached) = cached_opt {
                        lk.extend_from_slice(&cached.layers_k[layer_idx]);
                        lv.extend_from_slice(&cached.layers_v[layer_idx]);
                    }
                    for u in 0..uncached_ctx_len {
                        let base = u * qkv_stride;
                        lk.extend_from_slice(&ws.qkv[base + d..base + 2 * d]);
                        lv.extend_from_slice(&ws.qkv[base + 2 * d..base + 3 * d]);
                    }
                }

                let (cached_k, cached_v): (&[f32], &[f32]) = match cached_opt {
                    Some(ref cached) => (
                        &cached.layers_k[layer_idx],
                        &cached.layers_v[layer_idx],
                    ),
                    None => (&[], &[]),
                };

                self.run_tree_attention(
                    &ws.qkv,
                    cached_len,
                    cached_k,
                    cached_v,
                    &ws.anc_offsets,
                    &ws.anc_nodes,
                    n_active,
                    scale,
                    &mut ws.attn_scores,
                    &mut ws.attn_out,
                    has_avx2,
                );
                gemm_packed(&ws.attn_out, n_active, d, &layer.w_o, &mut ws.proj);
                add_inplace(&mut ws.x, &ws.proj, has_avx2);

                rmsnorm_batch(&ws.x, n_active, d, &layer.norm2, &mut ws.normed, has_avx2);
                gemm_packed(&ws.normed, n_active, d, &layer.w_gate_up, &mut ws.ff_gate_up);
                swiglu_batch(
                    &ws.ff_gate_up,
                    n_active,
                    self.d_ff,
                    &mut ws.ff_act,
                    has_avx2,
                );
                gemm_packed(&ws.ff_act, n_active, self.d_ff, &layer.w_down, &mut ws.proj);
                add_inplace(&mut ws.x, &ws.proj, has_avx2);
            }

            // 5. Final RMSNorm over active nodes.
            rmsnorm_batch(&ws.x, n_active, d, &self.norm_f, &mut ws.normed, has_avx2);

            if capture_ctx {
                let end_idx = uncached_ctx_len - 1;
                let ctx_end_normed = ws.normed[end_idx * d..(end_idx + 1) * d].to_vec();
                let entry = Arc::new(CachedContext {
                    tokens: ctx_tokens,
                    layers_k: std::mem::take(&mut ws.new_layers_k),
                    layers_v: std::mem::take(&mut ws.new_layers_v),
                    ctx_end_normed,
                });
                if let Ok(mut cache) = self.kv_cache.lock() {
                    cache.insert(entry);
                }
            }

            // 6. Score each candidate sequence from its active/cached context end state.
            let mut scores = Vec::with_capacity(candidates.len());
            for path in &ws.cand_paths {
                let mut total = 0.0_f32;
                let mut prev = cand_root_parent;
                for &node in path {
                    let tok = ws.trie_tok[node];
                    let hidden = if prev == usize::MAX {
                        cached_opt
                            .as_ref()
                            .map(|c| c.ctx_end_normed.as_slice())
                            .unwrap_or(&ws.normed[0..d])
                    } else {
                        &ws.normed[prev * d..(prev + 1) * d]
                    };
                    total += self.score_token_with_isa(hidden, tok, has_avx2);
                    prev = node;
                }
                let final_hidden = if prev == usize::MAX {
                    cached_opt
                        .as_ref()
                        .map(|c| c.ctx_end_normed.as_slice())
                        .unwrap_or(&ws.normed[0..d])
                } else {
                    &ws.normed[prev * d..(prev + 1) * d]
                };
                total += neural_eos_weight
                    * self.score_token_with_isa(final_hidden, EOS_ID, has_avx2);
                scores.push(total);
            }
            scores
        })
    }
}

fn rmsnorm_batch(
    x: &[f32],
    n_rows: usize,
    dim: usize,
    weight: &[f32],
    out: &mut [f32],
    has_avx2: bool,
) {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2 {
            // SAFETY: caller verified AVX2 + FMA and valid buffer lengths.
            unsafe {
                rmsnorm_batch_avx2(x, n_rows, dim, weight, out);
            }
            return;
        }
    }
    let _ = has_avx2;
    let inv_dim = 1.0_f32 / (dim as f32);
    for row in 0..n_rows {
        let src = &x[row * dim..(row + 1) * dim];
        let dst = &mut out[row * dim..(row + 1) * dim];
        let sq_sum = dot_f32_scalar(src, src);
        let scale = 1.0_f32 / (sq_sum * inv_dim + 1e-6_f32).sqrt();
        for i in 0..dim {
            dst[i] = src[i] * scale * weight[i];
        }
    }
}

fn is_hanzi(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
}

fn is_sentence_end(ch: char) -> bool {
    matches!(ch, '。' | '？' | '！' | '?' | '!')
}

fn is_pause_punct(ch: char) -> bool {
    matches!(ch, '，' | '、' | '；' | '：' | ',' | ';' | '—' | '…')
}

fn is_inline_wrapper(ch: char) -> bool {
    matches!(
        ch,
        '“' | '”'
            | '‘'
            | '’'
            | '「'
            | '」'
            | '『'
            | '』'
            | '《'
            | '》'
            | '（'
            | '）'
            | '【'
            | '】'
            | '('
            | ')'
            | '['
            | ']'
            | '"'
            | '\''
    )
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u16_vec<R: Read>(r: &mut R, len: usize) -> io::Result<Vec<u16>> {
    let mut raw = vec![0u8; len * 2];
    r.read_exact(&mut raw)?;
    Ok(raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect())
}

fn read_u32_vec<R: Read>(r: &mut R, len: usize) -> io::Result<Vec<u32>> {
    let mut raw = vec![0u8; len * 4];
    r.read_exact(&mut raw)?;
    Ok(raw
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn read_f32_vec<R: Read>(r: &mut R, len: usize) -> io::Result<Vec<f32>> {
    let mut raw = vec![0u8; len * 4];
    r.read_exact(&mut raw)?;
    Ok(raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Instant;

    fn pseudo_rand(seed: &mut u64) -> f32 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let u = ((*seed >> 40) as f32) / 16777216.0_f32;
        (u - 0.5) * 0.4
    }

    fn make_random_reranker(
        vocab_size: usize,
        d_model: usize,
        n_layers: usize,
        n_heads: usize,
        d_ff: usize,
    ) -> Reranker {
        let mut seed = 0x1234_5678_9abc_def0_u64;
        let mut vocab_scalars: Vec<u32> = (0..5).collect();
        for i in 5..vocab_size {
            vocab_scalars.push(0x4e00 + (i as u32 - 5));
        }
        let tok_emb: Vec<f32> = (0..vocab_size * d_model)
            .map(|_| pseudo_rand(&mut seed))
            .collect();
        let lm_head_bias: Vec<f32> = (0..vocab_size).map(|_| pseudo_rand(&mut seed)).collect();

        let mut layers = Vec::with_capacity(n_layers);
        for _ in 0..n_layers {
            let norm1 = vec![1.0_f32; d_model];
            let qkv: Vec<f32> = (0..3 * d_model * d_model)
                .map(|_| pseudo_rand(&mut seed))
                .collect();
            let wo: Vec<f32> = (0..d_model * d_model)
                .map(|_| pseudo_rand(&mut seed))
                .collect();
            let norm2 = vec![1.0_f32; d_model];
            let gate_up: Vec<f32> = (0..2 * d_ff * d_model)
                .map(|_| pseudo_rand(&mut seed))
                .collect();
            let down: Vec<f32> = (0..d_model * d_ff)
                .map(|_| pseudo_rand(&mut seed))
                .collect();
            layers.push(TransformerLayer {
                norm1,
                w_qkv: PackedMatrix::from_row_major_f32(&qkv, 3 * d_model, d_model),
                w_o: PackedMatrix::from_row_major_f32(&wo, d_model, d_model),
                norm2,
                w_gate_up: PackedMatrix::from_row_major_f32(&gate_up, 2 * d_ff, d_model),
                w_down: PackedMatrix::from_row_major_f32(&down, d_model, d_ff),
            });
        }
        let norm_f = vec![1.0_f32; d_model];
        Reranker::from_parts(
            vocab_scalars,
            d_model,
            n_heads,
            d_ff,
            128,
            10000.0,
            5.0,
            tok_emb,
            lm_head_bias,
            layers,
            norm_f,
        )
        .unwrap()
    }

    #[test]
    fn f16_roundtrip_accuracy() {
        for &v in &[0.0_f32, -0.0, 1.0, -1.0, 0.12345, -3.75, 42.25, 1e-4, -1e-4] {
            let h = f32_to_f16(v);
            let back = f16_to_f32(h);
            let err = (v - back).abs();
            assert!(err <= v.abs() * 2e-3 + 1e-6, "v={v} back={back} err={err}");
        }
    }

    #[test]
    fn gemm_packed_matches_scalar() {
        let mut seed = 999_u64;
        let k = 192;
        let out_dim = 384;
        let b_raw: Vec<f32> = (0..out_dim * k).map(|_| pseudo_rand(&mut seed)).collect();
        let b = PackedMatrix::from_row_major_f32(&b_raw, out_dim, k);

        for m in [1usize, 7, 13, 28, 37] {
            let a: Vec<f32> = (0..m * k).map(|_| pseudo_rand(&mut seed)).collect();
            let mut out_fast = vec![0.0_f32; m * out_dim];
            let mut out_scalar = vec![0.0_f32; m * out_dim];
            gemm_packed(&a, m, k, &b, &mut out_fast);
            gemm_packed_scalar(&a, m, k, &b, &mut out_scalar);
            let max_diff = out_fast
                .iter()
                .zip(out_scalar.iter())
                .map(|(x, y)| (x - y).abs())
                .fold(0.0_f32, f32::max);
            assert!(max_diff < 1e-4, "m={m} max_diff={max_diff}");
        }
    }

    #[test]
    fn prefix_trie_matches_independent_candidate_scoring() {
        let rr = make_random_reranker(128, 64, 2, 2, 128);
        let ctx = "今天天气，";
        let cands = ["一丁", "一七", "七上", "上下", "万一"];
        let batched = rr.score_candidates(ctx, &cands, 0.85);
        for (i, &cand) in cands.iter().enumerate() {
            let single = rr.score_candidates(ctx, &[cand], 0.85);
            let diff = (batched[i] - single[0]).abs();
            assert!(diff < 1e-4, "cand={cand} batched={} single={} diff={diff}", batched[i], single[0]);
        }
    }

    #[test]
    fn imerer01_binary_roundtrip_and_context_tokens() {
        let mut seed = 42_u64;
        let vocab_size = 64usize;
        let d_model = 32usize;
        let n_layers = 2usize;
        let n_heads = 2usize;
        let d_ff = 64usize;
        let max_seq_len = 64usize;
        let rope_theta = 10000.0_f32;
        let unk_penalty = 5.0_f32;

        let mut raw_bytes = Vec::new();
        raw_bytes.extend_from_slice(MAGIC);
        for val in [
            vocab_size as u32,
            d_model as u32,
            n_layers as u32,
            n_heads as u32,
            d_ff as u32,
            max_seq_len as u32,
            rope_theta.to_bits(),
            unk_penalty.to_bits(),
        ] {
            raw_bytes.extend_from_slice(&val.to_le_bytes());
        }
        let mut vocab_scalars: Vec<u32> = (0..5).collect();
        for i in 5..vocab_size {
            vocab_scalars.push(0x4e00 + (i as u32 - 5));
        }
        for &sc in &vocab_scalars {
            raw_bytes.extend_from_slice(&sc.to_le_bytes());
        }
        let write_f16s = |buf: &mut Vec<u8>, count: usize, s: &mut u64| -> (Vec<f32>, Vec<u16>) {
            let mut vals = Vec::with_capacity(count);
            let mut raw = Vec::with_capacity(count);
            for _ in 0..count {
                let bits = f32_to_f16(pseudo_rand(s));
                buf.extend_from_slice(&bits.to_le_bytes());
                raw.push(bits);
                vals.push(f16_to_f32(bits));
            }
            (vals, raw)
        };
        let write_f32s = |buf: &mut Vec<u8>, val: f32, count: usize| {
            let mut vals = Vec::with_capacity(count);
            for _ in 0..count {
                buf.extend_from_slice(&val.to_le_bytes());
                vals.push(val);
            }
            vals
        };

        let (tok_emb, _) = write_f16s(&mut raw_bytes, vocab_size * d_model, &mut seed);
        let lm_head_bias = write_f32s(&mut raw_bytes, 0.01, vocab_size);
        let mut layers = Vec::new();
        for _ in 0..n_layers {
            let norm1 = write_f32s(&mut raw_bytes, 1.0, d_model);
            let (_, qkv) = write_f16s(&mut raw_bytes, 3 * d_model * d_model, &mut seed);
            let (_, wo) = write_f16s(&mut raw_bytes, d_model * d_model, &mut seed);
            let norm2 = write_f32s(&mut raw_bytes, 1.0, d_model);
            let (_, gate_up) = write_f16s(&mut raw_bytes, 2 * d_ff * d_model, &mut seed);
            let (_, down) = write_f16s(&mut raw_bytes, d_model * d_ff, &mut seed);
            layers.push(TransformerLayer {
                norm1,
                w_qkv: PackedMatrix::from_row_major_f16(&qkv, 3 * d_model, d_model),
                w_o: PackedMatrix::from_row_major_f16(&wo, d_model, d_model),
                norm2,
                w_gate_up: PackedMatrix::from_row_major_f16(&gate_up, 2 * d_ff, d_model),
                w_down: PackedMatrix::from_row_major_f16_as_f32(&down, d_model, d_ff),
            });
        }
        let norm_f = write_f32s(&mut raw_bytes, 1.0, d_model);

        let tmp = std::env::temp_dir().join("imerer01_roundtrip_test.bin");
        File::create(&tmp).unwrap().write_all(&raw_bytes).unwrap();
        let loaded = Reranker::load(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let expected_rr = Reranker::from_parts(
            vocab_scalars,
            d_model,
            n_heads,
            d_ff,
            max_seq_len,
            rope_theta,
            unk_penalty,
            tok_emb,
            lm_head_bias,
            layers,
            norm_f,
        )
        .unwrap();

        let s_loaded = loaded.score_candidates("一。丁", &["七", "万"], 0.85);
        let s_expected = expected_rr.score_candidates("一。丁", &["七", "万"], 0.85);
        for (a, b) in s_loaded.iter().zip(s_expected.iter()) {
            assert!((a - b).abs() < 1e-5, "loaded={a} expected={b}");
        }
        assert_eq!(loaded.context_to_tokens("一。"), vec![BOS_ID]);
        assert_eq!(loaded.context_to_tokens("一，"), vec![BOS_ID, 5, PAUSE_ID]);
    }

    #[test]
    fn gemm_packed_f16_matches_scalar() {
        let mut seed = 1337_u64;
        let k = 192;
        let out_dim = 384;
        let b_raw: Vec<u16> = (0..out_dim * k)
            .map(|_| f32_to_f16(pseudo_rand(&mut seed)))
            .collect();
        let b_vnni = PackedMatrix::from_row_major_f16(&b_raw, out_dim, k);
        let b_f32_vals: Vec<f32> = b_raw.iter().copied().map(f16_to_f32).collect();
        let b_f32 = PackedMatrix::from_row_major_f32(&b_f32_vals, out_dim, k);

        for m in [1usize, 2, 3, 4, 5, 7, 13, 28, 37] {
            let a: Vec<f32> = (0..m * k).map(|_| pseudo_rand(&mut seed)).collect();
            let mut out_fast = vec![0.0_f32; m * out_dim];
            let mut out_scalar = vec![0.0_f32; m * out_dim];
            let mut out_ref = vec![0.0_f32; m * out_dim];
            gemm_packed(&a, m, k, &b_vnni, &mut out_fast);
            gemm_packed_scalar(&a, m, k, &b_vnni, &mut out_scalar);
            gemm_packed_scalar(&a, m, k, &b_f32, &mut out_ref);
            let max_diff = out_fast
                .iter()
                .zip(out_scalar.iter())
                .map(|(x, y)| (x - y).abs())
                .fold(0.0_f32, f32::max);
            assert!(max_diff < 1e-4, "f16/vnni m={m} max_diff={max_diff}");

            let dot: f64 = out_fast
                .iter()
                .zip(out_ref.iter())
                .map(|(&x, &y)| (x as f64) * (y as f64))
                .sum();
            let nf: f64 = out_fast.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>().sqrt();
            let nr: f64 = out_ref.iter().map(|&y| (y as f64) * (y as f64)).sum::<f64>().sqrt();
            let cos_sim = dot / (nf * nr).max(1e-12);
            assert!(cos_sim > 0.999, "f16/vnni m={m} cos_sim={cos_sim}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn fast_exp_ps_avx2_accuracy() {
        if !(is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")) {
            return;
        }
        use std::arch::x86_64::*;
        let mut max_rel = 0.0_f32;
        for step in 0..=400 {
            let base = -20.0_f32 + (step as f32) * 0.1_f32;
            let inp = [base; 8];
            let mut out = [0.0_f32; 8];
            unsafe {
                let v = _mm256_loadu_ps(inp.as_ptr());
                let ev = fast_exp_ps_avx2(v);
                _mm256_storeu_ps(out.as_mut_ptr(), ev);
            }
            let exact = base.exp();
            let rel = (out[0] - exact).abs() / exact.max(1e-30);
            if rel > max_rel {
                max_rel = rel;
            }
            assert!(rel < 2e-6, "x={base} out={} exact={exact} rel={rel}", out[0]);
        }
        assert!(max_rel < 2e-6);
    }

    #[test]
    fn prefix_kv_cache_matches_cold_scoring() {
        let rr = make_random_reranker(128, 64, 2, 2, 128);
        let cands = ["一丁", "一七", "七上", "上下", "万一"];

        rr.clear_kv_cache();
        let cold = rr.score_candidates("一丁", &cands, 0.85);
        // Same context hits full KV cache.
        let warm_exact = rr.score_candidates("一丁", &cands, 0.85);
        for (a, b) in cold.iter().zip(warm_exact.iter()) {
            assert!((a - b).abs() < 1e-4, "cold={a} warm_exact={b}");
        }

        // Compute cold scores for extended context vs extending cached "一丁" -> "一丁七".
        rr.clear_kv_cache();
        let cold_ext = rr.score_candidates("一丁七", &cands, 0.85);
        rr.clear_kv_cache();
        let _ = rr.score_candidates("一丁", &cands, 0.85);
        let warm_ext = rr.score_candidates("一丁七", &cands, 0.85);
        for (a, b) in cold_ext.iter().zip(warm_ext.iter()) {
            assert!((a - b).abs() < 1e-4, "cold_ext={a} warm_ext={b}");
        }
    }

    fn make_random_reranker_f16(
        vocab_size: usize,
        d_model: usize,
        n_layers: usize,
        n_heads: usize,
        d_ff: usize,
    ) -> Reranker {
        let mut seed = 0x1234_5678_9abc_def0_u64;
        let mut vocab_scalars: Vec<u32> = (0..5).collect();
        for i in 5..vocab_size {
            vocab_scalars.push(0x4e00 + (i as u32 - 5));
        }
        let tok_emb: Vec<f32> = (0..vocab_size * d_model)
            .map(|_| pseudo_rand(&mut seed))
            .collect();
        let lm_head_bias: Vec<f32> = (0..vocab_size).map(|_| pseudo_rand(&mut seed)).collect();

        let mut layers = Vec::with_capacity(n_layers);
        for _ in 0..n_layers {
            let norm1 = vec![1.0_f32; d_model];
            let qkv: Vec<u16> = (0..3 * d_model * d_model)
                .map(|_| f32_to_f16(pseudo_rand(&mut seed)))
                .collect();
            let wo: Vec<u16> = (0..d_model * d_model)
                .map(|_| f32_to_f16(pseudo_rand(&mut seed)))
                .collect();
            let norm2 = vec![1.0_f32; d_model];
            let gate_up: Vec<u16> = (0..2 * d_ff * d_model)
                .map(|_| f32_to_f16(pseudo_rand(&mut seed)))
                .collect();
            let down: Vec<u16> = (0..d_model * d_ff)
                .map(|_| f32_to_f16(pseudo_rand(&mut seed)))
                .collect();
            layers.push(TransformerLayer {
                norm1,
                w_qkv: PackedMatrix::from_row_major_f16(&qkv, 3 * d_model, d_model),
                w_o: PackedMatrix::from_row_major_f16(&wo, d_model, d_model),
                norm2,
                w_gate_up: PackedMatrix::from_row_major_f16(&gate_up, 2 * d_ff, d_model),
                w_down: PackedMatrix::from_row_major_f16_as_f32(&down, d_model, d_ff),
            });
        }
        let norm_f = vec![1.0_f32; d_model];
        Reranker::from_parts(
            vocab_scalars,
            d_model,
            n_heads,
            d_ff,
            128,
            10000.0,
            5.0,
            tok_emb,
            lm_head_bias,
            layers,
            norm_f,
        )
        .unwrap()
    }

    #[test]
    #[ignore]
    fn bench_cpu_reranker_latency() {
        for &(n_layers, d_model, n_heads, d_ff) in &[
            (6usize, 192usize, 3usize, 384usize),
            (4, 192, 3, 384),
            (4, 192, 3, 512),
        ] {
            let rr = make_random_reranker(6144, d_model, n_layers, n_heads, d_ff);
            let rr_prod = make_random_reranker_f16(6144, d_model, n_layers, n_heads, d_ff);
            // Typical context (~12 chars) + 8 overlapping 8-char candidates yielding N~28 nodes, and 16-char context -> N~47 nodes
            let ctx_28 = "今天早上我在厨房里，";
            let cands_28 = [
                "喝了一杯牛奶吗",
                "喝了已杯牛奶吗",
                "和了一杯牛奶吗",
                "合了一杯牛奶吗",
                "喝了一倍牛奶吗",
                "喝了一辈牛奶吗",
                "喝了一被牛奶吗",
                "喝了一杯牛耐吗",
            ];
            let ctx_47 = "我想知道昨天晚上他在教室里说的那句话";
            let cands_47 = [
                "是不是已经改变了原计划",
                "是不是已经改遍了原计划",
                "时不是已经改变了原计划",
                "是不少已经改变了原计划",
                "是不是一直改变了原计划",
                "是不是一件改变了原计划",
                "是不是依据改变了原计划",
                "是不是意境改变了原计划",
            ];
            for (tag, model_rr) in [("F32", &rr), ("VNNI-I8", &rr_prod)] {
                for _ in 0..5 {
                    model_rr.clear_kv_cache();
                    let _ = model_rr.score_candidates(ctx_28, &cands_28, 0.85);
                }
                let t0 = Instant::now();
                for _ in 0..100 {
                    model_rr.clear_kv_cache();
                    let _ = model_rr.score_candidates(ctx_28, &cands_28, 0.85);
                }
                let cold28 = t0.elapsed().as_secs_f64() * 1000.0 / 100.0;

                let _ = model_rr.score_candidates(ctx_28, &cands_28, 0.85);
                let t1 = Instant::now();
                for _ in 0..100 {
                    let _ = model_rr.score_candidates(ctx_28, &cands_28, 0.85);
                }
                let warm28 = t1.elapsed().as_secs_f64() * 1000.0 / 100.0;

                let t2 = Instant::now();
                for _ in 0..100 {
                    model_rr.clear_kv_cache();
                    let _ = model_rr.score_candidates(ctx_47, &cands_47, 0.85);
                }
                let cold47 = t2.elapsed().as_secs_f64() * 1000.0 / 100.0;

                let _ = model_rr.score_candidates(ctx_47, &cands_47, 0.85);
                let t3 = Instant::now();
                for _ in 0..100 {
                    let _ = model_rr.score_candidates(ctx_47, &cands_47, 0.85);
                }
                let warm47 = t3.elapsed().as_secs_f64() * 1000.0 / 100.0;
                println!(
                    "BENCH {tag:7} {n_layers}L d={d_model} h={n_heads} ff={d_ff}: N~28 cold={cold28:.3}ms warm={warm28:.3}ms | N~47 cold={cold47:.3}ms warm={warm47:.3}ms"
                );
            }
        }
    }
}
