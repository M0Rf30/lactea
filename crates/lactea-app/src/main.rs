//! Standalone lactea visualizer: window, audio capture/playback, preset browsing.
//!
//! The shell talks to the renderer only through [`visualizer::Visualizer`]; the MilkDrop
//! engine plugs in as `engine_vis::EngineVisualizer` (feature `engine`), the built-in
//! oscilloscope exists to develop and test everything else.

mod app;
mod audio;
mod capture;
mod cli;
#[cfg(feature = "engine")]
mod engine_vis;
mod feed;
mod gfx;
mod input;
mod presets;
mod textures;
mod visualizer;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info,wgpu_core=warn,wgpu_hal=warn,naga=warn,winit=warn,sctk=warn"),
    )
    .format_timestamp_millis()
    .init();

    if args.list_devices {
        audio::devices::print_devices();
        #[cfg(all(feature = "pipewire", target_os = "linux"))]
        {
            println!(
                "\nPipeWire sinks (the default one is what the default capture listens to; * = default):"
            );
            match audio::pw::list_sinks() {
                Ok(sinks) if sinks.is_empty() => println!("  (none)"),
                Ok(sinks) => sinks.iter().for_each(|s| println!("{s}")),
                Err(e) => println!("  PipeWire not available: {e}"),
            }
        }
        return Ok(());
    }
    app::run(args)
}
