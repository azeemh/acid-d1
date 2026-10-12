use acid_d1::acid::AcidInfo;
use acid_d1::gen::{build_acid_wav, WavSpec};

fn main() {
    let info = AcidInfo {
        flags: 1,
        is_loop: true,
        root_note: 60.0,
        beats: 8.0,
        meter_den: 4,
        meter_num: 4,
        tempo: 128.0,
    };
    let bytes = build_acid_wav(&WavSpec::default(), &info);
    std::fs::write("/tmp/demo_loop.wav", bytes).unwrap();
    println!("wrote /tmp/demo_loop.wav");
}
