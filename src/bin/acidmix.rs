use acid_d1::acid::AcidInfo;
use acid_d1::audio::{decode_wav, play_stereo};
use acid_d1::mix::Mixer;
use acid_d1::stretch::{stretch_stereo, StretchParams};
use acid_d1::wav::{write_wav_to_file, RiffFile};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: acidmix <project_bpm> <out.wav> <in1.wav> [<in2.wav> ...]");
        return ExitCode::from(2);
    }
    let project_bpm: f32 = match args[1].parse() {
        Ok(v) if v > 0.0 => v,
        _ => {
            eprintln!("error: project_bpm must be a positive number");
            return ExitCode::from(2);
        }
    };
    let out_path = PathBuf::from(&args[2]);
    let inputs: Vec<PathBuf> = args[3..].iter().map(PathBuf::from).collect();

    match run(project_bpm, &out_path, &inputs) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn run(
    project_bpm: f32,
    out_path: &PathBuf,
    inputs: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    println!("project tempo:   {:.3} BPM", project_bpm);
    println!();

    let params = StretchParams::default();
    let mut mixer: Option<Mixer> = None;

    for path in inputs {
        let bytes = fs::read(path)?;
        let riff = RiffFile::parse(&bytes)?;
        let decoded = decode_wav(&bytes)?;

        let acid = AcidInfo::from_riff(&riff).ok();
        let (source_tempo, source_beats) = match acid {
            Some(a) => (a.tempo, a.beats),
            None => (project_bpm, 0.0),
        };

        let stretch = if source_tempo > 0.0 {
            (source_tempo / project_bpm) as f64
        } else {
            1.0
        };

        let stretched = if (stretch - 1.0).abs() < 1e-6 {
            decoded.samples.clone()
        } else {
            stretch_stereo(&decoded.samples, stretch, &params)
        };

        let src_frames = decoded.num_frames();
        let out_frames = stretched.len() / 2;
        let out_dur = out_frames as f64 / decoded.sample_rate as f64;

        println!(
            "{}: native {:.2} BPM, {} beats, {} frames -> {} frames ({:.3} s @ {:.0} BPM)",
            path.display(),
            source_tempo,
            source_beats,
            src_frames,
            out_frames,
            out_dur,
            project_bpm
        );

        match &mut mixer {
            None => {
                let mut m = Mixer::new(decoded.sample_rate, out_frames);
                m.add_at(&stretched, 0, 1.0);
                mixer = Some(m);
            }
            Some(m) => {
                m.add_at(&stretched, 0, 1.0);
            }
        }
    }

    let mut mixer = mixer.ok_or("no inputs processed")?;
    let pre_peak = mixer.peak();
    mixer.normalize_to(0.95);
    let post_peak = mixer.peak();

    println!();
    println!(
        "mixed:           {} frames ({:.3} s @ {} Hz)",
        mixer.len_frames(),
        mixer.len_frames() as f64 / mixer.sample_rate as f64,
        mixer.sample_rate
    );
    println!("peak pre:        {:.3}", pre_peak);
    println!("peak post:       {:.3}", post_peak);

    write_wav_to_file(out_path, &mixer.samples, mixer.sample_rate, 2)?;
    println!("wrote:           {}", out_path.display());

    println!();
    println!("playing mixdown...");
    play_stereo(&mixer.samples, mixer.sample_rate)?;

    Ok(())
}
