//! cpal device enumeration and selection.
//!
//! Capturing "what is playing" through cpal means opening an *output* device as an input:
//! WASAPI turns that into loopback transparently, CoreAudio builds a process tap (macOS 14.6+).
//! ALSA and PulseAudio reject an output device as capture source, so on Linux the cpal path stays
//! an input-device path (a PulseAudio/PipeWire "monitor" source is an input device already).

use cpal::traits::{DeviceTrait, HostTrait};

use crate::cli::AudioBackend;

/// Whether cpal can capture an output device on this OS (WASAPI loopback, CoreAudio tap).
pub const OS_HAS_LOOPBACK: bool = cfg!(any(target_os = "windows", target_os = "macos"));

/// How a cpal capture device is opened.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CaptureKind {
    /// An input device (microphone, line in, a Pulse/PipeWire monitor source).
    Input,
    /// An output device opened as an input: records what that device plays.
    Loopback,
}

impl std::fmt::Display for CaptureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CaptureKind::Input => "input",
            CaptureKind::Loopback => "loopback",
        })
    }
}

/// What the default cpal capture opens: `first`, then `fallback` if `first` cannot be opened.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CapturePlan {
    pub first: CaptureKind,
    pub fallback: Option<CaptureKind>,
}

/// The default cpal capture for `backend` (`Pipewire` never reaches cpal; it is treated as
/// `Cpal`). Only `Auto` chooses: loopback of the default output where the OS supports it, since
/// that is what "what you hear" means there, and the default input device if that fails.
/// `Cpal` keeps meaning "input devices" and `CpalLoopback` is loopback without a fallback.
pub fn default_capture_plan(os_has_loopback: bool, backend: AudioBackend) -> CapturePlan {
    match backend {
        AudioBackend::Auto if os_has_loopback => CapturePlan {
            first: CaptureKind::Loopback,
            fallback: Some(CaptureKind::Input),
        },
        AudioBackend::CpalLoopback => CapturePlan {
            first: CaptureKind::Loopback,
            fallback: None,
        },
        AudioBackend::Auto | AudioBackend::Cpal | AudioBackend::Pipewire => CapturePlan {
            first: CaptureKind::Input,
            fallback: None,
        },
    }
}

/// Why an output device cannot be captured as loopback, `None` if it can. A duplex device
/// (headset, audio interface) is opened by cpal as its *input* side on CoreAudio, which would
/// record the microphone instead of the output.
pub fn loopback_unavailable(os_has_loopback: bool, supports_input: bool) -> Option<&'static str> {
    if !os_has_loopback {
        Some("loopback capture needs WASAPI (Windows) or CoreAudio (macOS)")
    } else if supports_input {
        Some("the device also has inputs, so cpal would record its input instead of its output")
    } else {
        None
    }
}

/// Everything a device can be selected by.
struct DeviceNames {
    id: Option<String>,
    /// Display strings (host name, description) besides the id.
    labels: Vec<String>,
}

impl DeviceNames {
    fn of(dev: &cpal::Device) -> Self {
        let mut labels = vec![dev.to_string()];
        if let Ok(d) = dev.description() {
            labels.push(d.to_string());
        }
        Self {
            id: dev.id().ok().map(|id| id.to_string()),
            labels,
        }
    }
}

/// Index of the device `wanted` selects: an id equal to it (ignoring case) wins, else the first
/// device with an id, name or description containing it (ignoring case).
fn select(devices: &[DeviceNames], wanted: &str) -> Option<usize> {
    let w = wanted.to_ascii_lowercase();
    devices
        .iter()
        .position(|d| {
            d.id.as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(wanted))
        })
        .or_else(|| {
            devices.iter().position(|d| {
                d.id.iter()
                    .chain(&d.labels)
                    .any(|c| c.to_ascii_lowercase().contains(&w))
            })
        })
}

/// Whether `dev_id` is `default_id`. Ids are the only stable identity: the display label of
/// `default_*_device()` can differ from the enumerated one. A device without an id is never
/// reported as default.
fn is_default(dev_id: Option<&str>, default_id: Option<&str>) -> bool {
    dev_id.is_some() && dev_id == default_id
}

fn id_of(dev: &cpal::Device) -> Option<String> {
    dev.id().ok().map(|id| id.to_string())
}

/// Display label of a device (`id` is the stable identifier accepted by `--capture-device`).
pub fn label(dev: &cpal::Device) -> String {
    let name = dev
        .description()
        .map(|d| d.to_string())
        .unwrap_or_else(|_| dev.to_string());
    match dev.id() {
        Ok(id) => format!("{name} [{id}]"),
        Err(_) => name,
    }
}

fn pick(devs: Vec<cpal::Device>, wanted: &str) -> Option<cpal::Device> {
    let names: Vec<DeviceNames> = devs.iter().map(DeviceNames::of).collect();
    select(&names, wanted).and_then(|i| devs.into_iter().nth(i))
}

/// The default device of `kind`.
pub fn default_capture_device(
    host: &cpal::Host,
    kind: CaptureKind,
) -> Result<cpal::Device, String> {
    match kind {
        CaptureKind::Input => host
            .default_input_device()
            .ok_or_else(|| "no default audio input device (use --list-devices)".to_string()),
        CaptureKind::Loopback => {
            if let Some(why) = loopback_unavailable(OS_HAS_LOOPBACK, false) {
                return Err(why.to_string());
            }
            let dev = host
                .default_output_device()
                .ok_or_else(|| "no default audio output device to capture".to_string())?;
            match loopback_unavailable(OS_HAS_LOOPBACK, dev.supports_input()) {
                Some(why) => Err(format!("default output '{}': {why}", label(&dev))),
                None => Ok(dev),
            }
        }
    }
}

/// `--capture-device`: an input device, or (where the OS supports it) an output device captured
/// as loopback. Inputs are searched before outputs; an id match anywhere beats a name match.
pub fn find_capture(
    host: &cpal::Host,
    wanted: &str,
) -> Result<(cpal::Device, CaptureKind), String> {
    let mut all: Vec<(cpal::Device, CaptureKind)> = host
        .input_devices()
        .map_err(|e| format!("cannot enumerate input devices: {e}"))?
        .map(|d| (d, CaptureKind::Input))
        .collect();
    if OS_HAS_LOOPBACK {
        // A failed output enumeration only removes the loopback candidates.
        if let Ok(outs) = host.output_devices() {
            all.extend(
                outs.filter(|d| {
                    loopback_unavailable(OS_HAS_LOOPBACK, d.supports_input()).is_none()
                })
                .map(|d| (d, CaptureKind::Loopback)),
            );
        }
    }
    let names: Vec<DeviceNames> = all.iter().map(|(d, _)| DeviceNames::of(d)).collect();
    match select(&names, wanted) {
        Some(i) => Ok(all.swap_remove(i)),
        None => Err(format!(
            "no {} device matching '{wanted}' (use --list-devices)",
            if OS_HAS_LOOPBACK {
                "input or loopback-capable output"
            } else {
                "input"
            }
        )),
    }
}

/// Select an output device for playback: `None` = host default; otherwise exact id first, then a
/// case-insensitive substring of name/description/id.
pub fn find_output(host: &cpal::Host, wanted: Option<&str>) -> Result<cpal::Device, String> {
    let Some(wanted) = wanted else {
        return host
            .default_output_device()
            .ok_or_else(|| "no default audio output device".to_string());
    };
    let devs: Vec<cpal::Device> = host
        .output_devices()
        .map_err(|e| format!("cannot enumerate output devices: {e}"))?
        .collect();
    pick(devs, wanted).ok_or_else(|| format!("no output device matching '{wanted}'"))
}

fn config_text(cfg: Result<cpal::SupportedStreamConfig, cpal::Error>) -> String {
    match cfg {
        Ok(c) => format!(
            "{} ch, {} Hz, {}",
            c.channels(),
            c.sample_rate(),
            c.sample_format()
        ),
        Err(e) => format!("no default config: {e}"),
    }
}

/// `--list-devices`: cpal inputs/outputs and, where available, PipeWire sinks.
pub fn print_devices() {
    let host = cpal::default_host();
    println!("cpal host: {}", host.id());
    let plan = default_capture_plan(OS_HAS_LOOPBACK, AudioBackend::Auto);
    println!(
        "default cpal capture (when PipeWire is not used): {}",
        match plan.fallback {
            Some(f) => format!("{} of the default output, else the default {f}", plan.first),
            None => format!("the default {} device", plan.first),
        }
    );
    let default_in = host.default_input_device().as_ref().and_then(id_of);
    let default_out = host.default_output_device().as_ref().and_then(id_of);
    println!("\nInput devices (--capture-device NAME|ID):");
    match host.input_devices() {
        Ok(devs) => {
            for d in devs {
                let star = if is_default(id_of(&d).as_deref(), default_in.as_deref()) {
                    "*"
                } else {
                    " "
                };
                println!(
                    " {star} {}  ({})",
                    label(&d),
                    config_text(d.default_input_config())
                );
            }
        }
        Err(e) => println!("  cannot enumerate: {e}"),
    }
    if OS_HAS_LOOPBACK {
        println!(
            "\nOutput devices (--output-device NAME|ID, used by --audio-file; [loopback] = capturable \
             with --capture-device NAME|ID, records what the device plays):"
        );
    } else {
        println!("\nOutput devices (--output-device NAME|ID, used by --audio-file):");
    }
    match host.output_devices() {
        Ok(devs) => {
            for d in devs {
                let star = if is_default(id_of(&d).as_deref(), default_out.as_deref()) {
                    "*"
                } else {
                    " "
                };
                let tag = if OS_HAS_LOOPBACK
                    && loopback_unavailable(OS_HAS_LOOPBACK, d.supports_input()).is_none()
                {
                    " [loopback]"
                } else {
                    ""
                };
                println!(
                    " {star} {}{tag}  ({})",
                    label(&d),
                    config_text(d.default_output_config())
                );
            }
        }
        Err(e) => println!("  cannot enumerate: {e}"),
    }
    println!("\n(* = default)");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: Option<&str>, labels: &[&str]) -> DeviceNames {
        DeviceNames {
            id: id.map(str::to_string),
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn auto_prefers_loopback_only_where_the_os_has_it() {
        let on = default_capture_plan(true, AudioBackend::Auto);
        assert_eq!(on.first, CaptureKind::Loopback);
        assert_eq!(on.fallback, Some(CaptureKind::Input));
        let off = default_capture_plan(false, AudioBackend::Auto);
        assert_eq!((off.first, off.fallback), (CaptureKind::Input, None));
    }

    #[test]
    fn explicit_backends_do_not_change_with_the_os() {
        for os in [false, true] {
            let cpal = default_capture_plan(os, AudioBackend::Cpal);
            assert_eq!((cpal.first, cpal.fallback), (CaptureKind::Input, None));
            let lb = default_capture_plan(os, AudioBackend::CpalLoopback);
            // No fallback: an explicit loopback request must not silently record the microphone.
            assert_eq!((lb.first, lb.fallback), (CaptureKind::Loopback, None));
        }
    }

    #[test]
    fn loopback_needs_os_support_and_an_output_only_device() {
        assert!(loopback_unavailable(true, false).is_none());
        assert!(loopback_unavailable(true, true).is_some());
        assert!(loopback_unavailable(false, false).is_some());
        assert!(loopback_unavailable(false, true).is_some());
    }

    #[test]
    fn id_match_beats_name_match() {
        let devs = [
            dev(Some("alsa:other"), &["Speakers (USB)", "alsa:hw:1"]),
            dev(Some("speakers"), &["Built-in"]),
        ];
        // "speakers" is a substring of the first device's name, but it is the second one's id.
        assert_eq!(select(&devs, "Speakers"), Some(1));
    }

    #[test]
    fn name_match_is_a_case_insensitive_substring_in_order() {
        let devs = [
            dev(Some("a"), &["Realtek Microphone"]),
            dev(Some("b"), &["Realtek Speakers"]),
            dev(None, &["Realtek HD"]),
        ];
        assert_eq!(select(&devs, "realtek"), Some(0));
        assert_eq!(select(&devs, "SPEAK"), Some(1));
        assert_eq!(select(&devs, "hd"), Some(2));
        assert_eq!(select(&devs, "nothing"), None);
        assert_eq!(select(&[], "x"), None);
    }

    #[test]
    fn substring_of_the_id_selects() {
        let devs = [dev(Some("wasapi:{0.0.0.00000000}.{abc}"), &["Headphones"])];
        assert_eq!(select(&devs, "{abc}"), Some(0));
    }

    #[test]
    fn default_is_compared_by_id_not_label() {
        assert!(is_default(Some("id1"), Some("id1")));
        assert!(!is_default(Some("id1"), Some("id2")));
        assert!(!is_default(Some("id1"), None));
        // Neither side has an id: nothing is known, so nothing is marked.
        assert!(!is_default(None, None));
    }
}
