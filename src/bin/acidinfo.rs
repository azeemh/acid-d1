use acid_d1::acid::AcidInfo;
use acid_d1::wav::{PcmFormat, RiffFile};
use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: acidinfo <file.wav>");
        return ExitCode::from(2);
    }

    let path = &args[1];
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error reading {}: {}", path, e);
            return ExitCode::from(1);
        }
    };

    let riff = match RiffFile::parse(&data) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error parsing {}: {}", path, e);
            return ExitCode::from(1);
        }
    };

    println!("file: {}", path);
    println!("size: {} bytes", data.len());
    println!();

    println!("chunks:");
    for c in &riff.chunks {
        println!("  {} ({} bytes)", c.id_str(), c.data.len());
    }
    println!();

    if let Some(fmt_chunk) = riff.find(b"fmt ") {
        if let Some(fmt) = PcmFormat::parse(&fmt_chunk.data) {
            println!("format:");
            println!("  channels:        {}", fmt.channels);
            println!("  sample rate:     {} Hz", fmt.sample_rate);
            println!("  bits/sample:     {}", fmt.bits_per_sample);
            println!();
        }
    }

    match AcidInfo::from_riff(&riff) {
        Ok(info) => {
            println!("acid metadata:");
            println!("  flags:           0x{:08x}", info.flags);
            println!("  loop:            {}", info.is_loop);
            println!("  root note:       {:.2} ({} / {:.2} Hz)", info.root_note, info.note_name(), info.root_hz());
            println!("  beats:           {:.3}", info.beats);
            println!("  meter:           {}/{}", info.meter_num, info.meter_den);
            println!("  tempo:           {:.3} BPM", info.tempo);
        }
        Err(e) => {
            println!("acid metadata:     not present ({})", e);
        }
    }

    ExitCode::SUCCESS
}
