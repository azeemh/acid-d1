use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    let device = host.default_output_device().expect("no output device");
    println!("device: {}", device.name().unwrap_or_default());
    println!();
    println!("default config:");
    if let Ok(c) = device.default_output_config() {
        println!("  {} Hz, {} ch, {:?}", c.sample_rate().0, c.channels(), c.sample_format());
    }
    println!();
    println!("all supported output configs:");
    if let Ok(supported) = device.supported_output_configs() {
        for c in supported {
            println!(
                "  {} ch, {} Hz..{} Hz, {:?}",
                c.channels(),
                c.min_sample_rate().0,
                c.max_sample_rate().0,
                c.sample_format()
            );
        }
    }
}
