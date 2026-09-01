//! S2 spike CLI: `measure` (compression numbers on a real payload tree), `pack` (assemble the
//! D3 sandwich), `inspect` (find + verify the payload in an assembled, possibly signed exe).
//! Cross-platform on purpose — the measurement runs wherever the payload is.

use std::io::Write;
use std::time::Instant;

use crate::overlay;

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("measure") => measure(args.get(1).ok_or("measure <payload-dir>")?),
        Some("pack") => pack(
            args.get(1).ok_or("pack <base-exe> <payload-dir> <out>")?,
            args.get(2).ok_or("pack: missing payload dir")?,
            args.get(3).ok_or("pack: missing output path")?,
        ),
        Some("inspect") => inspect(args.get(1).ok_or("inspect <exe>")?),
        _ => Err("modes: measure | pack | inspect".into()),
    }
}

fn tar_dir(dir: &str) -> Result<Vec<u8>, String> {
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_dir_all("", dir)
        .map_err(|e| format!("tar {dir}: {e}"))?;
    builder.into_inner().map_err(|e| e.to_string())
}

fn mb(len: usize) -> String {
    format!("{:.1} MB", len as f64 / 1_048_576.0)
}

fn measure(dir: &str) -> Result<(), String> {
    let raw = tar_dir(dir)?;
    println!("payload tar: {}", mb(raw.len()));

    let t = Instant::now();
    let z = zstd::encode_all(raw.as_slice(), 19).map_err(|e| e.to_string())?;
    println!("zstd -19:    {}  in {:.1}s", mb(z.len()), t.elapsed().as_secs_f32());

    let t = Instant::now();
    let mut xz = xz2::write::XzEncoder::new(Vec::new(), 9);
    xz.write_all(&raw).map_err(|e| e.to_string())?;
    let x = xz.finish().map_err(|e| e.to_string())?;
    println!("xz -9:       {}  in {:.1}s", mb(x.len()), t.elapsed().as_secs_f32());
    Ok(())
}

fn pack(base: &str, dir: &str, out: &str) -> Result<(), String> {
    let exe = std::fs::read(base).map_err(|e| format!("{base}: {e}"))?;
    let raw = tar_dir(dir)?;
    let payload = zstd::encode_all(raw.as_slice(), 19).map_err(|e| e.to_string())?;
    let assembled = overlay::assemble(&exe, &payload);
    std::fs::write(out, &assembled).map_err(|e| format!("{out}: {e}"))?;
    println!(
        "packed: exe {} + payload {} (tar {}) -> {}",
        mb(exe.len()),
        mb(payload.len()),
        mb(raw.len()),
        mb(assembled.len()),
    );
    Ok(())
}

fn inspect(path: &str) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let signed = overlay::cert_table_offset(&data).unwrap_or(0) != 0;
    let payload = overlay::extract(&data)?;
    let raw = zstd::decode_all(payload).map_err(|e| format!("zstd: {e}"))?;
    let mut archive = tar::Archive::new(raw.as_slice());
    let entries = archive
        .entries()
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .count();
    println!(
        "inspect: signed={signed} payload {} -> tar {} ({entries} entries), sha256 verified",
        mb(payload.len()),
        mb(raw.len()),
    );
    Ok(())
}
