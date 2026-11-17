//! Criterion benches for the pure-Rust codec paths: H.264 I-frame decode of a generated
//! Baseline/CAVLC stream, and PCM passthrough push/poll.
//!
//! Path class: `sw` (software decode, CPU only). The H.264 stream is minted in memory by
//! `synth_i_frame` — `mediaway-test-media` has no H.264 generator and no in-workspace
//! encoder emits Baseline+CAVLC+I-only clips (see `adr/0003-cavlc-i-slice-first-decode.md`).
//! It mixes `I_PCM` (first macroblock row) with `I_16x16` DC/Plane macroblocks and no
//! residual, so it measures intra prediction + transform + bit parsing, not CAVLC level
//! decoding. No `oracle_ref`: `FFmpeg` cannot be pointed at an equivalent partial decoder.
//! Run: `cargo bench -p mediaway-sw --bench sw_codecs --features h264`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    missing_docs,
    reason = "bench harness"
)]

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use mediaway_common::{AudioFrame, Bytes, Rational, SampleFormat};
use mediaway_sw::pcm::{PcmEncoder, PcmFormat, PcmPassthroughConfig};

#[cfg(feature = "h264")]
#[allow(unreachable_pub, reason = "bench-local helper module")]
mod h264_synth {
    use mediaway_common::Bytes;
    use mediaway_sw::h264::{NalUnit, NalUnitType, Pps, Sps};

    struct BitWriter {
        bytes: Vec<u8>,
        cur: u8,
        nbits: u8,
    }

    impl BitWriter {
        fn bit(&mut self, bit: u32) {
            self.cur = (self.cur << 1) | u8::from(bit & 1 == 1);
            self.nbits += 1;
            if self.nbits == 8 {
                self.bytes.push(self.cur);
                (self.cur, self.nbits) = (0, 0);
            }
        }
        fn bits(&mut self, value: u32, count: u32) {
            for i in (0..count).rev() {
                self.bit(value >> i);
            }
        }
        fn ue(&mut self, value: u32) {
            let code = value + 1;
            let len = u32::BITS - code.leading_zeros();
            self.bits(0, len - 1);
            self.bits(code, len);
        }
        fn align(&mut self) {
            while self.nbits != 0 {
                self.bit(0);
            }
        }
    }

    /// Mint one IDR picture of `mbw x mbh` macroblocks (see the bench crate docs).
    pub fn synth_i_frame(mbw: u32, mbh: u32) -> (Sps, Pps, NalUnit) {
        let sps = Sps {
            profile_idc: 66,
            constraint_flags: 0,
            level_idc: 40,
            seq_parameter_set_id: 0,
            width: mbw * 16,
            height: mbh * 16,
            frame_mbs_only: true,
            chroma_format_idc: 1,
            log2_max_frame_num: 4,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb: 4,
            pic_width_in_mbs: mbw,
            pic_height_in_mbs: mbh,
        };
        let pps = Pps {
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            entropy_coding_mode: false,
            num_ref_idx_l0_default_active: 1,
            num_ref_idx_l1_default_active: 1,
            pic_init_qp: 28,
            chroma_qp_index_offset: 0,
            deblocking_filter_control_present: false,
            constrained_intra_pred: false,
        };
        let mut w = BitWriter {
            bytes: Vec::new(),
            cur: 0,
            nbits: 0,
        };
        // Slice header: first_mb 0, slice_type 2 (I), pps 0, frame_num, idr_pic_id,
        // poc_lsb, no_output_of_prior_pics, long_term_reference, slice_qp_delta 0.
        w.ue(0);
        w.ue(2);
        w.ue(0);
        w.bits(0, 4);
        w.ue(0);
        w.bits(0, 4);
        w.bit(0);
        w.bit(0);
        w.ue(0);
        for row in 0..mbh {
            for col in 0..mbw {
                if row == 0 {
                    // I_PCM: mb_type 25, align, 256 + 2*64 raw samples (gradient).
                    w.ue(25);
                    w.align();
                    for i in 0..384u32 {
                        w.bits((i * 7 + col * 31) & 0xFF, 8);
                    }
                    continue;
                }
                // I_16x16, cbp 0: mb_type 3 (DC) or 4 (Plane, needs left+top+corner).
                w.ue(if col > 0 { 4 } else { 3 });
                w.ue(0); // intra_chroma_pred_mode = DC
                w.ue(0); // mb_qp_delta = 0
                // DC luma coeff_token TotalCoeff=0: nC >= 8 below an I_PCM row (FLC
                // "000011"), else nC 0..2 ("1").
                if row == 1 {
                    w.bits(0b00_0011, 6);
                } else {
                    w.bit(1);
                }
            }
        }
        w.bit(1); // rbsp_stop_one_bit
        w.align();
        let nal = NalUnit {
            ref_idc: 3,
            unit_type: NalUnitType::IdrSlice,
            rbsp: Bytes::from(w.bytes),
        };
        (sps, pps, nal)
    }
}

#[cfg(feature = "h264")]
fn h264(c: &mut Criterion) {
    use mediaway_sw::h264::decode_i_frame;
    // 1280x720 = 80x45 macroblocks.
    let (sps, pps, nal) = h264_synth::synth_i_frame(80, 45);
    decode_i_frame(&sps, &pps, &nal).expect("synthetic stream decodes");
    let mut g = c.benchmark_group("sw_h264");
    g.throughput(Throughput::Elements(1));
    g.bench_function("sw_h264_decode_i_frame_720p_synth", |b| {
        b.iter(|| decode_i_frame(&sps, &pps, black_box(&nal)).unwrap());
    });
    g.finish();
}

#[cfg(not(feature = "h264"))]
fn h264(_c: &mut Criterion) {}

fn pcm(c: &mut Criterion) {
    let format = PcmFormat {
        sample_rate: 48_000,
        channels: 2,
        sample_format: SampleFormat::S16,
    };
    let config = PcmPassthroughConfig::new(format, Rational::new(1, 48_000));
    let frame = AudioFrame {
        pts: 0,
        duration: 960,
        sample_rate: 48_000,
        channels: 2,
        format: SampleFormat::S16,
        data: Bytes::from(vec![0u8; 960 * 4]),
    };
    let mut enc = PcmEncoder::new(config);
    let mut g = c.benchmark_group("pcm");
    g.throughput(Throughput::Elements(1));
    g.bench_function("zc_pcm_passthrough_push_poll_20ms", |b| {
        b.iter(|| {
            enc.push_frame(black_box(&frame)).unwrap();
            black_box(enc.poll_packet().unwrap());
        });
    });
    g.finish();
}

criterion_group!(benches, h264, pcm);
criterion_main!(benches);
