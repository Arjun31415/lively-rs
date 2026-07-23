use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Instant;

pub fn start_audio_tracking(tx: Sender<f32>) -> thread::JoinHandle<()> {
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
        let mut buf = [0u8; 4096];
        let mut last_splat = Instant::now();
        const COOLDOWN_MS: u128 = 80;
        const THRESHOLD: f32 = 0.04;
        const CAPTURE_GAIN: f32 = 1.5;
        loop {
            let n = match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let samples: &[f32] = bytemuck::cast_slice(&buf[..n - (n % 4)]);
            if samples.is_empty() {
                continue;
            }
            let sum_sq: f32 = samples
                .iter()
                .map(|&s| s * s * CAPTURE_GAIN * CAPTURE_GAIN)
                .sum();
            let rms = (sum_sq / samples.len() as f32).sqrt();
            if rms > THRESHOLD && last_splat.elapsed().as_millis() > COOLDOWN_MS {
                last_splat = Instant::now();
                let _ = tx.send(rms);
            }
        }
    })
}
