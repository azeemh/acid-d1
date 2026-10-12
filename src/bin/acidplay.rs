use acid_d1::audio::{decode_wav, play_stereo};
use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: acidplay <file.wav>");
        return ExitCode::from(2);
    }
    match run(&args[1]) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn run(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let decoded = decode_wav(&bytes)?;
    println!(
        "source: {} frames @ {} Hz ({:.2} s)",
        decoded.num_frames(),
        decoded.sample_rate,
        decoded.duration_secs()
    );
    play_stereo(&decoded.samples, decoded.sample_rate)?;
    Ok(())
}
