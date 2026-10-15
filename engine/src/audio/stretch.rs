//! Pitch-preserving time stretch (WSOLA-like).
//!
//! Port of Vanilagy's `TimeStretcher` (matches Chromium HTMLMediaElement stretcher):
//! <https://gist.github.com/Vanilagy/05f7901f4c4398356657e3a86c7aee05>
//! Recommended from mediabunny discussion (#356); mediabunny itself has no rate API.
//!
//! `factor` = output duration / input duration (= `1 / playback_rate`).
//! Faster playback → `factor < 1`; slower → `factor > 1`.
//!
//! Wired from `audio::output` on wasm; always built so native unit tests run.

#![cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]

/// Streaming multi-channel time stretcher.
pub struct TimeStretcher {
    /// Output/input duration ratio (`1 / playback_rate`). Mutable at runtime.
    pub factor: f64,
    number_of_channels: usize,
    synthesis_hop_size: usize,
    window_size: usize,
    overlap_size: usize,
    tolerance: usize,
    buffers: Vec<Vec<f32>>,
    buffer_end_index: usize,
    output_buffers: Vec<Vec<f32>>,
    next_output_buffer_shift_amount: usize,
    has_done_output: bool,
    finalized: bool,
    blend_values: Vec<f32>,
}

impl TimeStretcher {
    pub fn new(number_of_channels: usize, sample_rate: f64, factor: f64) -> Self {
        let synthesis_hop_size = ((512.0 * sample_rate) / 48000.0).floor() as usize;
        let window_size = 2 * synthesis_hop_size;
        let overlap_size = synthesis_hop_size;
        let tolerance = synthesis_hop_size;
        let mut blend_values = Vec::with_capacity(overlap_size);
        for i in 0..overlap_size {
            blend_values.push(
                0.5 * (1.0 - (std::f64::consts::PI * i as f64 / overlap_size as f64).cos()) as f32,
            );
        }
        Self {
            factor: factor.max(1e-6),
            number_of_channels,
            synthesis_hop_size,
            window_size,
            overlap_size,
            tolerance,
            buffers: (0..number_of_channels)
                .map(|_| vec![0.0; 1 << 16])
                .collect(),
            buffer_end_index: tolerance,
            output_buffers: (0..number_of_channels)
                .map(|_| vec![0.0; 1 << 16])
                .collect(),
            next_output_buffer_shift_amount: 0,
            has_done_output: false,
            finalized: false,
            blend_values,
        }
    }

    fn ensure_output_buffer_length(&mut self, required_length: usize) {
        if required_length <= self.output_buffers[0].len() {
            return;
        }
        for buf in &mut self.output_buffers {
            buf.resize(required_length, 0.0);
        }
    }

    fn clear_output_buffers(&mut self) {
        let shift = self.next_output_buffer_shift_amount;
        if shift == 0 {
            return;
        }
        for buf in &mut self.output_buffers {
            let len = buf.len();
            if shift < len {
                buf.copy_within(shift.., 0);
            }
            // Trailing region after shift is unused until overwritten.
        }
        self.next_output_buffer_shift_amount = 0;
    }

    fn cross_correlate_all_channels(&self, input_start_pos: usize, k: isize, out_i: usize) -> f64 {
        let mut dot = 0.0_f64;
        let mut norm_old = 0.0_f64;
        let mut norm_new = 0.0_f64;
        for chan in 0..self.number_of_channels {
            let input = &self.buffers[chan];
            let output = &self.output_buffers[chan];
            for j in 0..self.overlap_size {
                let old_value = output[out_i + j] as f64;
                let new_idx = (input_start_pos as isize + k + j as isize) as usize;
                let new_value = input[new_idx] as f64;
                dot += old_value * new_value;
                norm_old += old_value * old_value;
                norm_new += new_value * new_value;
            }
        }
        dot / (norm_old * norm_new).sqrt().max(1e-10)
    }

    fn synthesize_segment(
        &mut self,
        i: usize,
        window_size: usize,
        input_start_pos: usize,
        max_positive_offset: isize,
    ) {
        let best_offset = if !self.has_done_output || (self.factor - 1.0).abs() < 1e-12 {
            0
        } else {
            let mut k = -(self.tolerance as isize);
            let mut best_corr = self.cross_correlate_all_channels(input_start_pos, k, i);
            let mut best_offset = k;

            let min_step: isize = 1;
            let max_step: isize = 16;
            let mut prev_corr = best_corr;

            while k < max_positive_offset {
                let next_k = (k + 1).min(max_positive_offset);
                let corr = self.cross_correlate_all_channels(input_start_pos, next_k, i);
                let gradient = (corr - prev_corr).abs();
                let adaptive_step = max_step
                    .min(((max_step as f64) * (-gradient * 3.0).exp()).floor() as isize)
                    .max(min_step);

                if corr > best_corr {
                    best_corr = corr;
                    best_offset = next_k;
                }
                prev_corr = corr;
                k += adaptive_step;
            }

            let fine_range: isize = 8;
            for k in (best_offset - fine_range)..=(best_offset + fine_range) {
                if k >= -(self.tolerance as isize) && k <= max_positive_offset {
                    let corr = self.cross_correlate_all_channels(input_start_pos, k, i);
                    if corr > best_corr {
                        best_corr = corr;
                        best_offset = k;
                    }
                }
            }
            best_offset
        };

        for chan in 0..self.number_of_channels {
            for j in 0..window_size {
                let blend = if j < self.overlap_size {
                    self.blend_values[j]
                } else {
                    1.0
                };
                let new_idx = (input_start_pos as isize + best_offset + j as isize) as usize;
                let sample = self.buffers[chan][new_idx];
                let out = &mut self.output_buffers[chan][i + j];
                *out = *out * (1.0 - blend) + blend * sample;
            }
        }
        self.has_done_output = true;
    }

    /// Append planar channel buffers. Returns stretched output frames (planar) when ready.
    pub fn append(&mut self, new_buffers: &[Vec<f32>]) -> Option<Vec<Vec<f32>>> {
        assert_eq!(new_buffers.len(), self.number_of_channels);
        if self.finalized {
            panic!("Cannot append more buffers after finalize");
        }
        let new_len = new_buffers[0].len();
        for (chan, new_buf) in new_buffers.iter().enumerate() {
            assert_eq!(new_buf.len(), new_len);
            let required = self.buffer_end_index + new_len;
            if required > self.buffers[chan].len() {
                let mut cap = self.buffers[chan].len();
                while cap < required {
                    cap *= 2;
                }
                self.buffers[chan].resize(cap, 0.0);
            }
            self.buffers[chan][self.buffer_end_index..self.buffer_end_index + new_len]
                .copy_from_slice(new_buf);
        }
        self.buffer_end_index += new_len;
        self.process()
    }

    fn process(&mut self) -> Option<Vec<Vec<f32>>> {
        let mut synthesis_length = 0usize;
        loop {
            let input_shift = (synthesis_length as f64 / self.factor).floor() as usize;
            if input_shift + self.window_size + 2 * self.tolerance > self.buffer_end_index {
                if synthesis_length == 0 {
                    return None;
                }
                synthesis_length = synthesis_length.saturating_sub(self.synthesis_hop_size);
                break;
            }
            synthesis_length += self.synthesis_hop_size;
        }
        if synthesis_length == 0 {
            return None;
        }

        let input_shift_amount = (synthesis_length as f64 / self.factor).floor() as usize;
        let output_length = synthesis_length - self.synthesis_hop_size + self.window_size;

        self.clear_output_buffers();
        self.ensure_output_buffer_length(output_length);

        let mut i = 0usize;
        while i < synthesis_length {
            let input_start_pos = self.tolerance + (i as f64 / self.factor).floor() as usize;
            self.synthesize_segment(
                i,
                self.window_size,
                input_start_pos,
                self.tolerance as isize,
            );
            i += self.synthesis_hop_size;
        }

        for chan in 0..self.number_of_channels {
            let end = self.buffer_end_index;
            self.buffers[chan].copy_within(input_shift_amount..end, 0);
        }
        self.buffer_end_index -= input_shift_amount;
        self.next_output_buffer_shift_amount = synthesis_length;

        Some(
            self.output_buffers
                .iter()
                .map(|b| b[..synthesis_length].to_vec())
                .collect(),
        )
    }

    /// Flush remaining input after the last append.
    pub fn finalize(&mut self) -> Option<Vec<Vec<f32>>> {
        self.finalized = true;
        if self.buffer_end_index <= self.tolerance {
            return None;
        }
        self.clear_output_buffers();
        let synthesis_length = self.buffer_end_index - self.tolerance;
        self.ensure_output_buffer_length(synthesis_length);
        self.synthesize_segment(0, synthesis_length, self.tolerance, 0);
        Some(
            self.output_buffers
                .iter()
                .map(|b| b[..synthesis_length].to_vec())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, freq: f64, sr: f64) -> Vec<f32> {
        (0..frames)
            .map(|i| (2.0 * std::f64::consts::PI * freq * (i as f64) / sr).sin() as f32)
            .collect()
    }

    #[test]
    fn faster_factor_shortens_output() {
        let sr = 48_000.0;
        let input = sine(sr as usize, 440.0, sr); // 1s
        let mut s = TimeStretcher::new(1, sr, 0.5); // 2× playback
        let mut out_len = 0usize;
        // Feed in chunks.
        for chunk in input.chunks(4096) {
            if let Some(out) = s.append(&[chunk.to_vec()]) {
                out_len += out[0].len();
            }
        }
        if let Some(out) = s.finalize() {
            out_len += out[0].len();
        }
        // ~0.5s wall for 1s media (±20% for WSOLA edges).
        assert!((out_len as f64) > sr * 0.35, "out_len={out_len}");
        assert!((out_len as f64) < sr * 0.65, "out_len={out_len}");
    }

    #[test]
    fn slower_factor_lengthens_output() {
        let sr = 48_000.0;
        let input = sine((sr * 0.5) as usize, 440.0, sr); // 0.5s
        let mut s = TimeStretcher::new(1, sr, 2.0); // 0.5× playback
        let mut out_len = 0usize;
        for chunk in input.chunks(4096) {
            if let Some(out) = s.append(&[chunk.to_vec()]) {
                out_len += out[0].len();
            }
        }
        if let Some(out) = s.finalize() {
            out_len += out[0].len();
        }
        // ~1s wall for 0.5s media.
        assert!((out_len as f64) > sr * 0.7, "out_len={out_len}");
        assert!((out_len as f64) < sr * 1.3, "out_len={out_len}");
    }
}
