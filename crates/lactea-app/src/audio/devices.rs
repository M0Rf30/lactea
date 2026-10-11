//! cpal device enumeration and selection.

use cpal::traits::{DeviceTrait, HostTrait};

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

fn matches(dev: &cpal::Device, wanted: &str) -> bool {
    let w = wanted.to_ascii_lowercase();
    let mut candidates = vec![dev.to_string()];
    if let Ok(d) = dev.description() {
        candidates.push(d.to_string());
    }
    if let Ok(id) = dev.id() {
        candidates.push(id.to_string());
    }
    candidates.iter().any(|c| c.eq_ignore_ascii_case(wanted))
        || candidates
            .iter()
            .any(|c| c.to_ascii_lowercase().contains(&w))
}

/// Select an input device: `None` = host default; otherwise exact id/name first, then a
/// case-insensitive substring of name/description/id.
pub fn find_input(host: &cpal::Host, wanted: Option<&str>) -> Result<cpal::Device, String> {
    let Some(wanted) = wanted else {
        return host
            .default_input_device()
            .ok_or_else(|| "no default audio input device (use --list-devices)".to_string());
    };
    let devs: Vec<cpal::Device> = host
        .input_devices()
        .map_err(|e| format!("cannot enumerate input devices: {e}"))?
        .collect();
    devs.iter()
        .find(|d| {
            d.id()
                .is_ok_and(|id| id.to_string().eq_ignore_ascii_case(wanted))
        })
        .or_else(|| devs.iter().find(|d| matches(d, wanted)))
        .cloned()
        .ok_or_else(|| format!("no input device matching '{wanted}' (use --list-devices)"))
}

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
    devs.iter()
        .find(|d| {
            d.id()
                .is_ok_and(|id| id.to_string().eq_ignore_ascii_case(wanted))
        })
        .or_else(|| devs.iter().find(|d| matches(d, wanted)))
        .cloned()
        .ok_or_else(|| format!("no output device matching '{wanted}'"))
}

/// `--list-devices`: cpal inputs/outputs and, where available, PipeWire sinks.
pub fn print_devices() {
    let host = cpal::default_host();
    println!("cpal host: {}", host.id());
    let default_in = host.default_input_device().map(|d| label(&d));
    let default_out = host.default_output_device().map(|d| label(&d));
    println!("\nInput devices (--capture-device NAME|ID):");
    match host.input_devices() {
        Ok(devs) => {
            for d in devs {
                let l = label(&d);
                let cfg = d
                    .default_input_config()
                    .map(|c| {
                        format!(
                            "{} ch, {} Hz, {}",
                            c.channels(),
                            c.sample_rate(),
                            c.sample_format()
                        )
                    })
                    .unwrap_or_else(|e| format!("no default config: {e}"));
                let star = if default_in.as_deref() == Some(l.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!(" {star} {l}  ({cfg})");
            }
        }
        Err(e) => println!("  cannot enumerate: {e}"),
    }
    println!("\nOutput devices (--output-device NAME|ID, used by --audio-file):");
    match host.output_devices() {
        Ok(devs) => {
            for d in devs {
                let l = label(&d);
                let cfg = d
                    .default_output_config()
                    .map(|c| {
                        format!(
                            "{} ch, {} Hz, {}",
                            c.channels(),
                            c.sample_rate(),
                            c.sample_format()
                        )
                    })
                    .unwrap_or_else(|e| format!("no default config: {e}"));
                let star = if default_out.as_deref() == Some(l.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!(" {star} {l}  ({cfg})");
            }
        }
        Err(e) => println!("  cannot enumerate: {e}"),
    }
    println!("\n(* = default)");
}
