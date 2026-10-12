use acid_d1::acid::AcidInfo;
use acid_d1::audio::{decode_wav, play_stereo};
use acid_d1::stretch::{stretch_stereo, StretchParams};
use acid_d1::wav::RiffFile;
use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: acidplay_tempo <file.wav> <target_bpm>");
        return ExitCode::from(2);
    }
    let target_bpm: f32 = match args[2].parse() {
        Ok(v) if v > 0.0 => v,
        _ => {
            eprintln!("error: target_bpm must be a positive number");
            return ExitCode::from(2);
        }
    };
    match run(&args[1], target_bpm) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn run(path: &str, target_bpm: f32) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;

    let riff = RiffFile::parse(&bytes)?;
    let acid = AcidInfo::from_riff(&riff)
        .map_err(|e| format!("file has no ACID metadata: {}", e))?;

    let source_tempo = acid.tempo;
    if source_tempo <= 0.0 {
        return Err("ACID chunk reports zero source tempo".into());
    }

    let decoded = decode_wav(&bytes)?;

    // Stretch factor: to go from source_tempo to target_bpm,
    // we need to play at (target_bpm / source_tempo) speed.
    // Speed factor = target/source. Stretch factor = 1/speed = source/target.
    let stretch = (source_tempo / target_bpm) as f64;

    println!("file:            {}", path);
    println!("source tempo:    {:.3} BPM", source_tempo);
    println!("target tempo:    {:.3} BPM", target_bpm);
    println!("stretch factor:  {:.4}  ({}x length)", stretch, stretch);
    println!(
        "source:          {} frames @ {} Hz ({:.3} s)",
        decoded.num_frames(),
        decoded.sample_rate,
        decoded.duration_secs()
    );

    let params = StretchParams::default();
    let stretched = stretch_stereo(&decoded.samples, stretch, &params);

    let stretched_frames = stretched.len() / 2;
    let stretched_dur = stretched_frames as f64 / decoded.sample_rate as f64;
    println!(
        "stretched:       {} frames ({:.3} s)",
        stretched_frames, stretched_dur
    );

    play_stereo(&stretched, decoded.sample_rate)?;
    Ok(())
}
