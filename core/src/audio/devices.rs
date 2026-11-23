use std::fmt::Write as _;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{DeviceId, HostId, SampleFormat, StreamConfig};
use serde::Serialize;

use super::AudioMode;
use crate::config::AudioConfig;
use crate::error::{Result, SilentError};

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub host: String,
    pub id: String,
    pub name: String,
    pub direction: String,
    pub input: bool,
    pub output: bool,
    pub default: bool,
    pub loopback_hint: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub host_id: HostId,
    pub id: DeviceId,
    pub id_string: String,
    pub name: String,
    pub input: bool,
    pub output: bool,
    pub default_input: bool,
    pub default_output: bool,
}

pub(crate) struct CapturePlan {
    pub host_id: HostId,
    pub id: DeviceId,
    pub device_name: String,
    pub device_id_string: String,
    pub config: StreamConfig,
    pub sample_format: SampleFormat,
    pub loopback: bool,
    pub mode: AudioMode,
}

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mut out = Vec::new();
    let hosts = cpal::available_hosts();
    if hosts.is_empty() {
        return Err(anyhow::Error::from(SilentError::Audio(
            "no cpal audio hosts are available".to_owned(),
        )));
    }

    for host_id in hosts {
        let host = match cpal::host_from_id(host_id) {
            Ok(host) => host,
            Err(err) => {
                tracing::warn!(?host_id, %err, "audio host unavailable");
                continue;
            }
        };
        let default_in = host.default_input_device().and_then(|d| d.id().ok());
        let default_out = host.default_output_device().and_then(|d| d.id().ok());
        let devices = match host.devices() {
            Ok(devices) => devices,
            Err(err) => {
                tracing::warn!(?host_id, %err, "failed to enumerate audio devices");
                continue;
            }
        };

        for device in devices {
            let id = match device.id() {
                Ok(id) => id,
                Err(err) => {
                    tracing::warn!(?host_id, %err, "device disappeared while listing");
                    continue;
                }
            };
            let name = device.to_string();
            let input = device.supports_input();
            let output = device.supports_output();
            let direction = match (input, output) {
                (true, true) => "duplex",
                (true, false) => "input",
                (false, true) => "output",
                (false, false) => "unknown",
            };
            let is_default = Some(&id) == default_in.as_ref() || Some(&id) == default_out.as_ref();

            out.push(DeviceInfo {
                host: host_id.to_string(),
                id: id.to_string(),
                name: name.clone(),
                direction: direction.to_owned(),
                input,
                output,
                default: is_default,
                loopback_hint: loopback_hint(&host_id, &name, input, output),
            });
        }
    }

    Ok(out)
}

pub fn format_device_list(devices: &[DeviceInfo]) -> String {
    let mut out = String::new();
    if devices.is_empty() {
        out.push_str("No audio devices found.\n");
        return out;
    }

    let _ = writeln!(out, "Audio devices ({}):", devices.len());
    for device in devices {
        let _ = writeln!(out, "{}", device.id);
        let default = if device.default { " · default" } else { "" };
        let _ = writeln!(
            out,
            "  {} · {}{} · {}",
            device.host, device.direction, default, device.name
        );
        if !device.loopback_hint.is_empty() {
            let _ = writeln!(out, "  → {}", device.loopback_hint);
        }
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn loopback_hint(host: &HostId, name: &str, input: bool, output: bool) -> String {
    let lower = name.to_lowercase();
    #[cfg(target_os = "windows")]
    {
        if output {
            return "WASAPI loopback: use this output device as the capture source".to_owned();
        }
    }
    #[cfg(target_os = "macos")]
    {
        if lower.contains("blackhole") {
            return "BlackHole virtual input: select as system-audio source on macOS 13–14.5"
                .to_owned();
        }
        if output {
            return "Core Audio tap on macOS 14.6+; on 13–14.5 use BlackHole or a Multi-Output Device"
                .to_owned();
        }
    }
    #[cfg(target_os = "linux")]
    {
        let host_name = host.to_string();
        if host_name == "pipewire" && (name == "default_sink" || lower.contains("monitor")) {
            return "PipeWire capture sink (system audio)".to_owned();
        }
        if lower.contains("monitor") {
            return "PulseAudio monitor source (system audio)".to_owned();
        }
        if host_name == "pipewire" && output {
            return "PipeWire sink; prefer the `default_sink` device for loopback".to_owned();
        }
        if host_name == "alsa" && !input && output {
            return "ALSA output-only; select a PulseAudio monitor source or build with --features native-pipewire".to_owned();
        }
    }
    String::new()
}

pub(crate) fn resolve_plan(config: &AudioConfig) -> Result<CapturePlan> {
    let hosts = cpal::available_hosts();
    if hosts.is_empty() {
        return Err(anyhow::Error::from(SilentError::Audio(
            "no cpal audio hosts are available".to_owned(),
        )));
    }

    if config.device != "auto"
        && let Some((host_id, device_id)) = parse_device_id(&config.device)
        && hosts.contains(&host_id)
    {
        let host = cpal::host_from_id(host_id).map_err(|e| {
            anyhow::Error::from(SilentError::Audio(format!(
                "failed to open host {host_id}: {e}"
            )))
        })?;
        if let Some(device) = host.device_by_id(&device_id) {
            return plan_for_candidate(config, host_id, &device);
        }
        return Err(anyhow::Error::from(SilentError::Audio(format!(
            "device {} was not found on host {host_id}. Run --list-devices",
            config.device
        ))));
    }

    let candidates = enumerate_candidates()?;
    if candidates.is_empty() {
        return Err(anyhow::Error::from(SilentError::Audio(
            "no capture devices found. Run --list-devices".to_owned(),
        )));
    }

    let selected = if config.device != "auto" {
        match_candidate(&candidates, &config.device)?
    } else {
        default_candidate(config, &candidates)?
    };

    let host = cpal::host_from_id(selected.host_id).map_err(|e| {
        anyhow::Error::from(SilentError::Audio(format!(
            "failed to open audio host {}: {e}",
            selected.host_id
        )))
    })?;
    let device = host.device_by_id(&selected.id).ok_or_else(|| {
        anyhow::Error::from(SilentError::Audio(format!(
            "device {} ({}) disappeared",
            selected.name, selected.id_string
        )))
    })?;
    plan_for_candidate(config, selected.host_id, &device)
}

fn enumerate_candidates() -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for host_id in cpal::available_hosts() {
        let host = match cpal::host_from_id(host_id) {
            Ok(host) => host,
            Err(err) => {
                tracing::warn!(?host_id, %err, "audio host unavailable");
                continue;
            }
        };
        let default_in = host.default_input_device().and_then(|d| d.id().ok());
        let default_out = host.default_output_device().and_then(|d| d.id().ok());
        let devices = match host.devices() {
            Ok(devices) => devices,
            Err(err) => {
                tracing::warn!(?host_id, %err, "failed to enumerate devices");
                continue;
            }
        };

        for device in devices {
            let id = match device.id() {
                Ok(id) => id,
                Err(_) => continue,
            };
            let input = device.supports_input();
            let output = device.supports_output();
            let default_input = Some(&id) == default_in.as_ref();
            let default_output = Some(&id) == default_out.as_ref();
            out.push(Candidate {
                host_id,
                id_string: id.to_string(),
                id,
                name: device.to_string(),
                input,
                output,
                default_input,
                default_output,
            });
        }
    }
    Ok(out)
}

fn parse_device_id(value: &str) -> Option<(HostId, DeviceId)> {
    let (host, _) = value.split_once(':')?;
    let host_id: HostId = host.parse().ok()?;
    let device_id: DeviceId = value.parse().ok()?;
    Some((host_id, device_id))
}

fn match_candidate<'a>(candidates: &'a [Candidate], wanted: &str) -> Result<&'a Candidate> {
    let wanted_lower = wanted.to_lowercase();
    let exact: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.id_string.eq_ignore_ascii_case(wanted) || c.name.eq_ignore_ascii_case(wanted))
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0]);
    }
    if exact.len() > 1 {
        return Err(anyhow::Error::from(SilentError::Audio(format!(
            "device `{wanted}` is ambiguous; use its full id from --list-devices"
        ))));
    }

    let fuzzy: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            c.id_string.to_lowercase().contains(&wanted_lower)
                || c.name.to_lowercase().contains(&wanted_lower)
        })
        .collect();
    match fuzzy.len() {
        1 => Ok(fuzzy[0]),
        0 => Err(anyhow::Error::from(SilentError::Audio(format!(
            "no audio device matches `{wanted}`. Run --list-devices"
        )))),
        _ => {
            let names = fuzzy
                .iter()
                .map(|c| c.id_string.as_str())
                .take(6)
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow::Error::from(SilentError::Audio(format!(
                "device `{wanted}` is ambiguous; candidates: {names}"
            ))))
        }
    }
}

fn default_candidate<'a>(
    config: &AudioConfig,
    candidates: &'a [Candidate],
) -> Result<&'a Candidate> {
    #[cfg(target_os = "linux")]
    {
        if config.mode == AudioMode::System {
            // Native PipeWire host (optional feature) exposes a capture sink.
            if config.linux_prefer_pipewire {
                if let Some(pw) = candidates
                    .iter()
                    .find(|c| c.host_id.to_string() == "pipewire" && c.name == "default_sink")
                {
                    tracing::info!("using native PipeWire default_sink for system audio");
                    return Ok(pw);
                }
                if let Some(monitor) = candidates.iter().find(|c| {
                    c.host_id.to_string() == "pipewire" && c.name.to_lowercase().contains("monitor")
                }) {
                    return Ok(monitor);
                }
            }

            // Default build: PipeWire's PulseAudio server exposes monitor
            // sources. Pick the monitor belonging to the current default sink,
            // not just the first monitor (HDMI vs analog matters).
            if let Some(sink) = candidates
                .iter()
                .find(|c| c.host_id.to_string() == "pulseaudio" && c.default_output && c.output)
            {
                let sink_name = sink.name.to_lowercase();
                if let Some(monitor) = candidates.iter().find(|c| {
                    c.host_id.to_string() == "pulseaudio"
                        && c.input
                        && c.name.to_lowercase().contains(&sink_name)
                }) {
                    tracing::info!(sink = %sink.name, "using PulseAudio monitor of the default sink");
                    return Ok(monitor);
                }
            }
            if let Some(source) = candidates.iter().find(|c| {
                c.host_id.to_string() == "pulseaudio"
                    && c.input
                    && c.name.to_lowercase().contains("monitor")
            }) {
                return Ok(source);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if config.mode == AudioMode::System && !macos_taps_supported() {
            if let Some(blackhole) = candidates
                .iter()
                .find(|c| c.name.to_lowercase().contains("blackhole") && c.input)
            {
                tracing::info!("macOS < 14.6: using BlackHole virtual input for system audio");
                return Ok(blackhole);
            }
            return Err(anyhow::Error::from(SilentError::Audio(
                "system audio on macOS 13–14.5 needs BlackHole 2ch. Install BlackHole, create a \
                 Multi-Output Device (speakers + BlackHole), then run --list-devices and set \
                 [audio] device to the BlackHole input. macOS 14.6+ uses native Core Audio taps."
                    .to_owned(),
            )));
        }
    }

    let wanted_default_output = config.mode == AudioMode::System;
    let selected = candidates
        .iter()
        .find(|c| {
            if wanted_default_output {
                c.default_output && (c.output || c.input)
            } else {
                c.default_input && c.input
            }
        })
        .or_else(|| {
            candidates.iter().find(|c| {
                if wanted_default_output {
                    c.output
                } else {
                    c.input
                }
            })
        });

    selected
        .ok_or_else(|| anyhow::Error::from(SilentError::Audio(default_device_error(config.mode))))
}

fn plan_for_candidate(
    config: &AudioConfig,
    host_id: HostId,
    device: &cpal::Device,
) -> Result<CapturePlan> {
    #[cfg(target_os = "macos")]
    if config.mode == AudioMode::System && !macos_taps_supported() && !device.supports_input() {
        return Err(anyhow::Error::from(SilentError::Audio(
            "this output device is not a valid system-audio source on macOS 13–14.5. Install \
             BlackHole 2ch, create a Multi-Output Device, and select the BlackHole input; native \
             Core Audio taps require macOS 14.6+."
                .to_owned(),
        )));
    }

    let supports_input = device.supports_input();
    let use_input_config = config.mode == AudioMode::Microphone || supports_input;

    let mut ranges: Vec<cpal::SupportedStreamConfigRange> = if use_input_config {
        device
            .supported_input_configs()
            .map(|iter| iter.collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if ranges.is_empty() {
        ranges = device
            .supported_output_configs()
            .map(|iter| iter.collect())
            .unwrap_or_default();
    }

    let chosen = choose_supported_config(ranges, device, use_input_config, config.sample_rate)?;

    let stream_config = StreamConfig {
        channels: chosen.channels(),
        sample_rate: chosen.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    Ok(CapturePlan {
        host_id,
        id: device.id().map_err(|e| {
            anyhow::Error::from(SilentError::Audio(format!("device disappeared: {e}")))
        })?,
        device_name: device.to_string(),
        device_id_string: device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| format!("{host_id}:unknown")),
        config: stream_config,
        sample_format: chosen.sample_format(),
        loopback: config.mode == AudioMode::System,
        mode: config.mode,
    })
}

fn choose_supported_config(
    ranges: Vec<cpal::SupportedStreamConfigRange>,
    device: &cpal::Device,
    prefer_input: bool,
    desired_rate: Option<u32>,
) -> Result<cpal::SupportedStreamConfig> {
    if !ranges.is_empty() {
        {
            let mut best: Option<(i32, cpal::SupportedStreamConfig)> = None;
            for range in ranges {
                let format_score = match range.sample_format() {
                    SampleFormat::F32 => 0,
                    SampleFormat::I16 => 1,
                    SampleFormat::I32 => 2,
                    SampleFormat::U16 => 3,
                    SampleFormat::I8 => 4,
                    SampleFormat::U8 => 5,
                    SampleFormat::F64 => 6,
                    _ => 20,
                };
                if format_score >= 20 {
                    continue;
                }

                let rate = desired_rate
                    .filter(|rate| range.contains_rate(*rate))
                    .or_else(|| {
                        if range.contains_rate(48_000) {
                            Some(48_000)
                        } else if range.contains_rate(16_000) {
                            Some(16_000)
                        } else if range.min_sample_rate() > 48_000 {
                            Some(range.min_sample_rate())
                        } else {
                            Some(range.max_sample_rate().min(48_000))
                        }
                    })
                    .unwrap_or(range.max_sample_rate());

                let Some(config) = range.try_with_sample_rate(rate) else {
                    continue;
                };
                let channel_score = if config.channels() == 2 {
                    0
                } else if config.channels() == 1 {
                    1
                } else {
                    2
                };
                let score = format_score * 10 + channel_score;
                if best
                    .as_ref()
                    .is_none_or(|(best_score, _)| score < *best_score)
                {
                    best = Some((score, config));
                }
            }
            if let Some((_, config)) = best {
                return Ok(config);
            }
        }
    }

    let fallback = if prefer_input {
        device
            .default_input_config()
            .or_else(|_| device.default_output_config())
    } else {
        device
            .default_output_config()
            .or_else(|_| device.default_input_config())
    };
    fallback.map_err(|e| {
        anyhow::Error::from(SilentError::Audio(format!(
            "device has no usable stream configuration: {e}"
        )))
    })
}

#[cfg(target_os = "macos")]
fn macos_taps_supported() -> bool {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output();
    let Ok(output) = output else {
        return false;
    };
    let version = String::from_utf8_lossy(&output.stdout);
    let mut parts = version.trim().split('.');
    let major = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    major > 14 || (major == 14 && minor >= 6)
}

fn default_device_error(mode: AudioMode) -> String {
    match mode {
        AudioMode::System => {
            #[cfg(target_os = "windows")]
            {
                "no playback device found for WASAPI loopback. Run --list-devices and set \
                 [audio] device to your speakers/headphones."
                    .to_owned()
            }
            #[cfg(target_os = "linux")]
            {
                "no system-audio monitor source found. Install PipeWire with pipewire-pulse, \
                 run --list-devices, and select a `Monitor of ...` source (host pulseaudio). \
                 Alternatively build with --features native-pipewire to use `default_sink`."
                    .to_owned()
            }
            #[cfg(target_os = "macos")]
            {
                "no loopback source found. Use macOS 14.6+ for native taps, or install BlackHole \
                 and select its input in [audio] device."
                    .to_owned()
            }
            #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
            {
                "no system-audio source available on this platform".to_owned()
            }
        }
        AudioMode::Microphone => "no microphone input device found".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_round_trip() {
        let parsed = parse_device_id("pipewire:sink_default");
        if let Some((host, id)) = parsed {
            assert_eq!(host.to_string(), "pipewire");
            assert_eq!(id.to_string(), "pipewire:sink_default");
        }
    }

    #[test]
    fn format_listing_is_nonempty() {
        let devices = list_devices().unwrap_or_default();
        let text = format_device_list(&devices);
        assert!(!text.is_empty());
    }
}
