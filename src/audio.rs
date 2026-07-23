use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

/// Number of output frequency bins sent to the JS side (audioArray.length==128) 
/// ref https://github.com/rocksdanister/lively/wiki/Web-Guide-V-:-System-Data#--audio.
pub const AUDIO_BINS: usize = 128;

pub type AudioSpectrum = Arc<Mutex<Vec<f32>>>;

pub fn new_spectrum_handle() -> AudioSpectrum {
    Arc::new(Mutex::new(vec![0.0; AUDIO_BINS]))
}

const FFT_SIZE: usize = 1024;

/// Software capture gain applied before the FFT (quiet sources get boosted).
const CAPTURE_GAIN: f32 = 15.0;

pub fn start_audio_tracking(spectrum: AudioSpectrum) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut child = Command::new("parec")
            .args([
                "--device=@DEFAULT_SINK@.monitor",
                "--format=float32le",
                "--rate=44100",
                "--channels=1",
                "--latency-msec=15",
                "--process-time-msec=10",
                "--raw",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to start parec");

        let mut stdout = child.stdout.take().unwrap();
        let mut read_buf = [0u8; 4096];

        // Rolling window holding the most recent FFT_SIZE samples.
        let mut window: Vec<f32> = Vec::with_capacity(FFT_SIZE);

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        // Hann window to cut down on spectral leakage at the buffer edges.
        let hann: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos()
            })
            .collect();

        let half = FFT_SIZE / 2;
        let bins_per_output = (half as f32 / AUDIO_BINS as f32).max(1.0);

        loop {
            let n = match stdout.read(&mut read_buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let samples: &[f32] = bytemuck::cast_slice(&read_buf[..n - (n % 4)]);
            if samples.is_empty() {
                continue;
            }

            window.extend(samples.iter().map(|&s| s * CAPTURE_GAIN));
            if window.len() > FFT_SIZE {
                let excess = window.len() - FFT_SIZE;
                window.drain(0..excess);
            }
            if window.len() < FFT_SIZE {
                // Not enough audio yet for a full window.
                continue;
            }

            let mut buffer: Vec<Complex<f32>> = window
                .iter()
                .zip(hann.iter())
                .map(|(&s, &w)| Complex::new(s * w, 0.0))
                .collect();
            fft.process(&mut buffer);

            let magnitudes: Vec<f32> = buffer[..half]
                .iter()
                .map(|c| c.norm() / FFT_SIZE as f32)
                .collect();

            // Downsample the raw spectrum into AUDIO_BINS output values.
            let mut bins = vec![0.0f32; AUDIO_BINS];
            for (i, bin) in bins.iter_mut().enumerate() {
                let start = (i as f32 * bins_per_output) as usize;
                let end = (((i + 1) as f32 * bins_per_output) as usize).min(half);
                if start >= end {
                    continue;
                }
                let sum: f32 = magnitudes[start..end].iter().sum();
                *bin = sum / (end - start) as f32;
            }

            // Overwrite the shared slot — old value is simply dropped, never queued.
            if let Ok(mut guard) = spectrum.lock() {
                *guard = bins;
            }
        }
        child.wait().unwrap();
    })
}
