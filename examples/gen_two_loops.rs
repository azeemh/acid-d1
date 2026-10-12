use acid_d1::gen::build_click_loop;
use std::fs;

fn main() {
    // Loop A: 128 BPM, 8 beats, 440 Hz.
    let a = build_click_loop(8.0, 128.0, 440.0, 44100, 2);
    fs::write("/tmp/loop_a.wav", &a).unwrap();
    println!("wrote /tmp/loop_a.wav ({} bytes)", a.len());

    // Loop B: 90 BPM, 8 beats, 660 Hz.
    let b = build_click_loop(8.0, 90.0, 660.0, 44100, 2);
    fs::write("/tmp/loop_b.wav", &b).unwrap();
    println!("wrote /tmp/loop_b.wav ({} bytes)", b.len());
}
