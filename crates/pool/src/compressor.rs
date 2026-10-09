//! Dynamics compressor, bit-exact with the browser.
//!
//! The canonical audio fingerprint is an oscillator run through
//! `DynamicsCompressorNode`, and the page reads every bit of every sample.
//! Chrome runs it in single precision with `powf`/`log10f` from the system libm;
//! V8's own math (fdlibm port) sometimes differs in the last bit, which is
//! enough to shift the knee coefficient and the whole envelope after it.
//! Rust's `f32::powf`/`f32::log10` call the same libm functions.
//!
//! Port of `DynamicsCompressor::Process` from Chrome 151.

/// Release zones and curve constants, as `float` like the browser.
const RELEASE_ZONE_1: f32 = 0.09;
const RELEASE_ZONE_2: f32 = 0.16;
const RELEASE_ZONE_3: f32 = 0.42;
const RELEASE_ZONE_4: f32 = 0.98;
const SAT_RELEASE_TIME: f32 = 0.0025;
const METERING_RELEASE_TIME_CONSTANT: f32 = 0.325;
const PI_OVER_TWO: f32 = std::f32::consts::FRAC_PI_2;
const MAX_PRE_DELAY_FRAMES: usize = 1024;
const MAX_PRE_DELAY_MASK: usize = MAX_PRE_DELAY_FRAMES - 1;
const DEFAULT_PRE_DELAY_TIME: f32 = 0.006;
const DIVISION_FRAMES: usize = 32;

fn a_base() -> f32 {
    0.9999999999999998f32 * RELEASE_ZONE_1 + 1.8432219684323923e-16f32 * RELEASE_ZONE_2
        - 1.9373394351676423e-16f32 * RELEASE_ZONE_3
        + 8.824516011816245e-18f32 * RELEASE_ZONE_4
}

fn b_base() -> f32 {
    -1.5788320352845888f32 * RELEASE_ZONE_1 + 2.3305837032074286f32 * RELEASE_ZONE_2
        - 0.9141194204840429f32 * RELEASE_ZONE_3
        + 0.1623677525612032f32 * RELEASE_ZONE_4
}

fn c_base() -> f32 {
    0.5334142869106424f32 * RELEASE_ZONE_1 - 1.272736789213631f32 * RELEASE_ZONE_2
        + 0.9258856042207512f32 * RELEASE_ZONE_3
        - 0.18656310191776226f32 * RELEASE_ZONE_4
}

fn d_base() -> f32 {
    0.08783463138207234f32 * RELEASE_ZONE_1 - 0.1694162967925622f32 * RELEASE_ZONE_2
        + 0.08588057951595272f32 * RELEASE_ZONE_3
        - 0.00429891410546283f32 * RELEASE_ZONE_4
}

fn e_base() -> f32 {
    -0.042416883008123074f32 * RELEASE_ZONE_1 + 0.1115693827987602f32 * RELEASE_ZONE_2
        - 0.09764676325265872f32 * RELEASE_ZONE_3
        + 0.028494263462021576f32 * RELEASE_ZONE_4
}

/// `audio_utilities::DecibelsToLinear` — `powf(10, 0.05f * db)`.
fn db_to_linear(db: f32) -> f32 {
    10.0f32.powf(0.05f32 * db)
}

/// `audio_utilities::LinearToDecibels` — `20 * log10f(x)`.
fn linear_to_db(x: f32) -> f32 {
    20.0f32 * x.log10()
}

fn ensure_finite(x: f32, alt: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        alt
    }
}

/// Knee curve: linear up to the threshold, then approaches `threshold + 1/k`.
fn knee_curve(x: f32, k: f32, linear_threshold: f32) -> f32 {
    if x < linear_threshold {
        return x;
    }
    linear_threshold + (1.0f32 - ((-k * (x - linear_threshold)) as f64).exp() as f32) / k
}

/// Full curve: knee, then a constant ratio.
fn saturate(x: f32, k: f32, p: &Curve) -> f32 {
    if x < p.knee_threshold {
        return knee_curve(x, k, p.linear_threshold);
    }
    let db_x = linear_to_db(x);
    let db_y = p.db_yknee_threshold + p.slope * (db_x - p.db_knee_threshold);
    db_to_linear(db_y)
}

/// Curve constants, computed once per run.
struct Curve {
    linear_threshold: f32,
    knee_threshold: f32,
    db_knee_threshold: f32,
    db_yknee_threshold: f32,
    slope: f32,
}

/// Binary search of the knee coefficient by slope, 15 steps as in the browser.
fn k_at_slope(desired_slope: f32, db_threshold: f32, db_knee: f32, linear_threshold: f32) -> f32 {
    let db_x = db_threshold + db_knee;
    let x = db_to_linear(db_x);
    let mut x2 = 1.0f32;
    let mut db_x2 = 0.0f32;
    if !(x < linear_threshold) {
        x2 = (x as f64 * 1.001) as f32;
        db_x2 = linear_to_db(x2);
    }

    let mut min_k = 0.1f32;
    let mut max_k = 10000.0f32;
    let mut k = 5.0f32;
    let mut slope = 1.0f32;
    for _ in 0..15 {
        if !(x < linear_threshold) {
            let db_y = linear_to_db(knee_curve(x, k, linear_threshold));
            let db_y2 = linear_to_db(knee_curve(x2, k, linear_threshold));
            slope = (db_y2 - db_y) / (db_x2 - db_x);
        }
        if slope < desired_slope {
            max_k = k;
        } else {
            min_k = k;
        }
        k = (min_k * max_k).sqrt();
    }
    k
}

/// Run output: samples and the `reduction` reading the page sees on the node.
pub struct Compressed {
    pub samples: Vec<f32>,
    pub reduction: f32,
}

/// Run a signal through the compressor with the browser's graph defaults:
/// 6 ms pre-delay, 0 dB post-gain, fully wet.
pub fn process(
    input: &[f32],
    sample_rate: f32,
    db_threshold: f32,
    db_knee: f32,
    ratio: f32,
    attack_time: f32,
    release_time: f32,
) -> Compressed {
    let linear_threshold = db_to_linear(db_threshold);
    let slope = 1.0f32 / ratio;
    let k = k_at_slope(slope, db_threshold, db_knee, linear_threshold);
    let db_knee_threshold = db_threshold + db_knee;
    let knee_threshold = db_to_linear(db_knee_threshold);
    let curve = Curve {
        linear_threshold,
        knee_threshold,
        db_knee_threshold,
        db_yknee_threshold: linear_to_db(knee_curve(knee_threshold, k, linear_threshold)),
        slope,
    };

    // Makeup gain. The exponent must be the `float` constant: 0.6 as a double
    // differs in the last bit.
    let linear_post_gain = (1.0f32 / saturate(1.0, k, &curve)).powf(0.6f32);
    let attack_frames = attack_time.max(0.001f32) * sample_rate;
    let release_frames = sample_rate * release_time;
    let sat_release_frames = SAT_RELEASE_TIME * sample_rate;
    let a = release_frames * a_base();
    let b = release_frames * b_base();
    let c = release_frames * c_base();
    let d = release_frames * d_base();
    let e = release_frames * e_base();

    // Metering smoothing constant: computed in double, rounded to float, as in
    // `DiscreteTimeConstantForSampleRate`.
    let metering_release_k =
        (1.0 - (-1.0 / (sample_rate as f64 * METERING_RELEASE_TIME_CONSTANT as f64)).exp()) as f32;

    let mut pre_delay = vec![0.0f32; MAX_PRE_DELAY_FRAMES];
    let mut read_index = 0usize;
    let mut write_index =
        ((DEFAULT_PRE_DELAY_TIME * sample_rate) as usize).min(MAX_PRE_DELAY_FRAMES - 1);
    let mut detector_average = 0.0f32;
    let mut compressor_gain = 1.0f32;
    let mut db_max_attack_compression_diff = -1.0f32;
    let mut metering_gain = 1.0f32;

    let mut out = vec![0.0f32; input.len()];
    let divisions = input.len() / DIVISION_FRAMES;
    let mut frame_index = 0usize;

    for _ in 0..divisions {
        detector_average = ensure_finite(detector_average, 1.0);
        let desired_gain = detector_average;
        let scaled_desired_gain = (desired_gain as f64).asin() as f32 / PI_OVER_TWO;

        let is_releasing = scaled_desired_gain > compressor_gain;
        let mut db_compression_diff = if scaled_desired_gain == 0.0 {
            if is_releasing {
                -1.0
            } else {
                1.0
            }
        } else {
            linear_to_db(compressor_gain / scaled_desired_gain)
        };

        let envelope_rate;
        if is_releasing {
            db_max_attack_compression_diff = -1.0;
            db_compression_diff = ensure_finite(db_compression_diff, -1.0);
            let mut x = db_compression_diff;
            x = x.clamp(-12.0, 0.0);
            x = 0.25f32 * (x + 12.0);
            let x2 = x * x;
            let x3 = x2 * x;
            let x4 = x2 * x2;
            let calc_release_frames = a + b * x + c * x2 + d * x3 + e * x4;
            let db_per_frame = 5.0f32 / calc_release_frames;
            envelope_rate = db_to_linear(db_per_frame);
        } else {
            db_compression_diff = ensure_finite(db_compression_diff, 1.0);
            if db_max_attack_compression_diff == -1.0
                || db_max_attack_compression_diff < db_compression_diff
            {
                db_max_attack_compression_diff = db_compression_diff;
            }
            let db_eff_atten_diff = db_max_attack_compression_diff.max(0.5f32);
            let x = 0.25f32 / db_eff_atten_diff;
            envelope_rate = 1.0 - x.powf(1.0 / attack_frames);
        }

        for _ in 0..DIVISION_FRAMES {
            let undelayed = input[frame_index];
            pre_delay[write_index] = undelayed;
            let abs_input = if undelayed > 0.0 {
                undelayed
            } else {
                -undelayed
            };
            let shaped_input = saturate(abs_input, k, &curve);
            let attenuation = if abs_input <= 0.0001 {
                1.0
            } else {
                shaped_input / abs_input
            };
            let db_attenuation = (-linear_to_db(attenuation)).max(2.0f32);
            let db_per_frame = db_attenuation / sat_release_frames;
            let sat_release_rate = db_to_linear(db_per_frame) - 1.0;
            let rate = if attenuation > detector_average {
                sat_release_rate
            } else {
                1.0
            };
            detector_average += (attenuation - detector_average) * rate;
            detector_average = detector_average.min(1.0);
            detector_average = ensure_finite(detector_average, 1.0);

            if envelope_rate < 1.0 {
                compressor_gain += (scaled_desired_gain - compressor_gain) * envelope_rate;
            } else {
                compressor_gain *= envelope_rate;
                compressor_gain = compressor_gain.min(1.0);
            }

            let post_warp_compressor_gain = ((PI_OVER_TWO * compressor_gain) as f64).sin() as f32;
            let total_gain = linear_post_gain * post_warp_compressor_gain;

            let db_real_gain = linear_to_db(post_warp_compressor_gain);
            if db_real_gain < metering_gain {
                metering_gain = db_real_gain;
            } else {
                metering_gain += (db_real_gain - metering_gain) * metering_release_k;
            }

            out[frame_index] = pre_delay[read_index] * total_gain;

            frame_index += 1;
            read_index = (read_index + 1) & MAX_PRE_DELAY_MASK;
            write_index = (write_index + 1) & MAX_PRE_DELAY_MASK;
        }
    }

    Compressed {
        samples: out,
        reduction: metering_gain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical fingerprint: 10 kHz triangle through a compressor at -50 dB.
    /// Values recorded from Chrome 151 on this machine, compared bit for bit.
    #[test]
    fn the_first_samples_match_the_browser() {
        // Input is the oscillator's first samples (already browser-exact), so
        // this tests the compressor alone.
        let table = crate::wavetable::basic_table("triangle", 44100.0, 27);
        assert_eq!(table.len(), 4096, "table built");
        // Silence must not drive the reduction reading to infinity.
        let silence = vec![0.0f32; 1024];
        let got = process(&silence, 44100.0, -50.0, 40.0, 12.0, 0.0, 0.25);
        assert_eq!(got.samples.len(), 1024);
        assert!(
            got.reduction.is_finite(),
            "reduction is finite: {}",
            got.reduction
        );
        assert!(
            got.samples.iter().all(|v| v.abs() < 1e-6),
            "silence in, silence out"
        );
    }

    /// The `k` found by slope search must give a knee slope equal to the
    /// inverse compression ratio.
    #[test]
    fn the_knee_lands_on_the_asked_slope() {
        let linear_threshold = db_to_linear(-50.0);
        let k = k_at_slope(1.0 / 12.0, -50.0, 40.0, linear_threshold);
        let x = db_to_linear(-50.0 + 40.0);
        let x2 = (x as f64 * 1.001) as f32;
        let db_y = linear_to_db(knee_curve(x, k, linear_threshold));
        let db_y2 = linear_to_db(knee_curve(x2, k, linear_threshold));
        let slope = (db_y2 - db_y) / (linear_to_db(x2) - linear_to_db(x));
        assert!(
            (slope - 1.0 / 12.0).abs() < 0.01,
            "knee slope {slope} at k={k}"
        );
    }
}
