//! Audio/video support through external `ffmpeg` / `ffprobe`.
//!
//! Frames are extracted with ffmpeg and compared as images. Decoded PCM is
//! hashed so re-muxed audio files match without re-encoding.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Create a command that never flashes a console window on Windows (the UI is
/// a GUI subsystem process and ffmpeg/ffprobe are console tools).
fn new_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub has_video: bool,
    pub has_audio: bool,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub bit_rate: Option<u64>,
}

/// Run an external tool with a hard timeout and cooperative cancellation.
fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
    cancel: &AtomicBool,
) -> anyhow::Result<std::process::Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if cancel.load(Ordering::Relaxed) || started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("process cancelled or timed out");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Run `ffprobe -show_streams -show_format -of json`.
pub fn probe(ffprobe: &Path, file: &Path, cancel: &AtomicBool) -> anyhow::Result<Probe> {
    let mut cmd = new_command(ffprobe);
    cmd.args([
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_streams",
        "-show_format",
    ])
    .arg(file);
    let out = run_with_timeout(cmd, Duration::from_secs(15), cancel)?;

    if !out.status.success() {
        anyhow::bail!(
            "ffprobe failed for {}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let json: Value = serde_json::from_slice(&out.stdout)?;
    let mut p = Probe::default();

    if let Some(duration) = json
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
    {
        p.duration_ms = Some((duration * 1000.0) as u64);
    }
    if let Some(br) = json
        .get("format")
        .and_then(|f| f.get("bit_rate"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<u64>().ok())
    {
        p.bit_rate = Some(br);
    }

    if let Some(streams) = json.get("streams").and_then(|s| s.as_array()) {
        for s in streams {
            match s.get("codec_type").and_then(|t| t.as_str()) {
                Some("video") => {
                    p.has_video = true;
                    p.width = s
                        .get("width")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32)
                        .or(p.width);
                    p.height = s
                        .get("height")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32)
                        .or(p.height);
                    p.video_codec = s
                        .get("codec_name")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                Some("audio") => {
                    p.has_audio = true;
                    p.audio_codec = s
                        .get("codec_name")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                _ => {}
            }
        }
    }

    Ok(p)
}

/// Extract a single video frame to `out` (PNG) at `at_secs`.
/// Mirrors the MP4 finder's `ffmpeg -ss <pos> -i <file> -frames:v 1 <out>`.
pub fn extract_frame(
    ffmpeg: &Path,
    file: &Path,
    at_secs: f64,
    out: &Path,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let mut cmd = new_command(ffmpeg);
    cmd.arg("-v")
        .arg("error")
        .arg("-ss")
        .arg(format!("{at_secs:.3}"))
        .arg("-i")
        .arg(file)
        .arg("-frames:v")
        .arg("1")
        .arg("-y")
        .arg(out);
    let result = run_with_timeout(cmd, Duration::from_secs(20), cancel)?;
    if !result.status.success() {
        anyhow::bail!("ffmpeg could not extract a frame from {}", file.display());
    }
    Ok(())
}

/// Decode `seconds` of audio starting at `at_secs` to 8 kHz mono s16le and
/// return the SHA-256 of the PCM. Re-muxed files produce the same digest;
/// re-encoded files do not.
pub fn audio_pcm_hash(
    ffmpeg: &Path,
    file: &Path,
    at_secs: f64,
    seconds: f64,
    cancel: &AtomicBool,
) -> anyhow::Result<String> {
    let mut child = new_command(ffmpeg)
        .args(["-v", "quiet", "-ss", &format!("{at_secs:.3}")])
        .args(["-t", &format!("{seconds:.3}")])
        .arg("-i")
        .arg(file)
        .args(["-vn", "-ac", "1", "-ar", "8000", "-f", "s16le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 32 * 1024];
    let mut total = 0usize;
    let started = Instant::now();
    if let Some(stdout) = child.stdout.as_mut() {
        loop {
            if cancel.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("audio decoding cancelled or timed out");
            }
            let n = stdout.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n;
            hasher.update(&buf[..n]);
        }
    }
    let _ = child.wait();

    if total == 0 {
        anyhow::bail!("no audio decoded from {}", file.display());
    }
    Ok(crate::hash::hex::encode(hasher.finalize()))
}

/// Locate a sibling `ffmpeg`/`ffprobe`.
pub fn find_tool(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        if let Ok(p) = which(name) {
            return Some(p);
        }
    }
    None
}

fn which(name: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Ok(exe);
            }
        }
    }
    Err(())
}
