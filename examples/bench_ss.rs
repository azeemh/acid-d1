use signalsmith_stretch::Stretch;
use std::time::Instant;

fn main() {
    let rate = 44100u32;
    let secs = 30;
    let frames = rate as usize * secs;
    let mut input = vec![0.0f32; frames * 2];
    for i in 0..frames {
        let s = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin();
        input[i * 2] = s;
        input[i * 2 + 1] = s;
    }
    let ratio = 0.68f64;
    let out_frames = (frames as f64 * ratio) as usize;

    // Benchmark 1: exact() on 30 seconds.
    {
        let mut stretch = Stretch::preset_default(2, rate);
        stretch.set_transpose_factor_semitones(0.0, None);
        let mut output = vec![0.0f32; out_frames * 2];
        let t = Instant::now();
        stretch.exact(&input, &mut output);
        println!("exact:        30s audio -> {:?}", t.elapsed());
    }

    // Benchmark 2: process() in 8192-frame chunks, feeding (n/ratio) input
    // for n output. Tests whether the streaming API can do the stretch.
    {
        let mut stretch = Stretch::preset_default(2, rate);
        stretch.set_transpose_factor_semitones(0.0, None);
        let n_out_chunk = 8192usize;
        let n_in_chunk = ((n_out_chunk as f64) / ratio).round() as usize;
        let t = Instant::now();
        let mut in_pos = 0usize;
        let mut produced = 0usize;
        let mut out_buf = vec![0.0f32; n_out_chunk * 2];
        while produced < out_frames && in_pos < frames {
            let in_end = (in_pos + n_in_chunk).min(frames);
            let in_slice = &input[in_pos * 2..in_end * 2];
            let mut padded = vec![0.0f32; n_in_chunk * 2];
            padded[..in_slice.len()].copy_from_slice(in_slice);
            stretch.process(&padded, &mut out_buf);
            in_pos = in_end;
            produced += n_out_chunk;
        }
        println!("process chunk: 30s audio -> {:?}", t.elapsed());
    }
}
