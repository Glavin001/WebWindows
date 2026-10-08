//! Replays a recorded command stream on the native GPU and saves what the
//! stream read back from textures (the frames wined3d presents) as PNGs.
//!
//! Recordings come from `node runtime/node/wine.mjs --d3d-record FILE ...`:
//! "D3GR", then each batch as a little-endian u32 length and its bytes.
//!
//! cargo run --release -p d3dgpu-core --example replay -- FILE [OUT_DIR]
//! (DUMP=1 also prints the commands; PRESENTS=N also saves every Nth
//! presented frame as present-NNNNN.png, as Node has no canvas to show them,
//! and then saves the recorded readbacks only with READS=1; READ_EVERY=N
//! saves every Nth readback, BATCHES=N stops after N batches)

use std::io::{Read, Write};

use d3dgpu_core::{Core, Options};
use d3dgpu_proto::{Command, Reader};

/// The readback region the Wine bridge gives the core (runtime/d3dgpu/protocol.mjs).
const SHARED_BYTES: usize = 1 << 20;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: replay FILE [OUT_DIR]");
    let out = std::path::PathBuf::from(args.get(2).map(String::as_str).unwrap_or("target/replay"));
    std::fs::create_dir_all(&out).unwrap();
    // Streamed a batch at a time: recordings of long runs are gigabytes.
    let mut file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).unwrap();
    assert_eq!(&magic, b"D3GR", "not a d3dgpu recording");

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).expect("adapter");
    let features = adapter.features()
        & (wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::FLOAT32_FILTERABLE
            | wgpu::Features::CLIP_DISTANCES);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .expect("device");
    let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let e2 = errors.clone();
    device.on_uncaptured_error(std::sync::Arc::new(move |e: wgpu::Error| e2.lock().unwrap().push(e.to_string())));

    let mut core = Core::with_options(device.clone(), queue.clone(), Options::for_device(&device));
    // Room for presented frames too (the recorded readbacks stay in the first SHARED_BYTES).
    let mut shared = vec![0u8; SHARED_BYTES.max(32 << 20)];
    let save_reads = std::env::var("PRESENTS").is_err() || std::env::var_os("READS").is_some();
    // READ_EVERY=N saves only every Nth readback; BATCHES=N stops after N batches.
    let read_every: usize = std::env::var("READ_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    let max_batches: usize = std::env::var("BATCHES").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let mut reads_seen = 0usize;
    let (mut batches, mut frames) = (0, 0);
    let mut original = Vec::new();
    let every_present: usize = std::env::var("PRESENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let (mut sizes, mut presents) = (std::collections::HashMap::new(), 0usize);
    let mut len = [0u8; 4];
    // A recording cut off mid-batch (the program was stopped) ends at the last whole one.
    while batches < max_batches && file.read_exact(&mut len).is_ok() {
        original.resize(u32::from_le_bytes(len) as usize, 0);
        if file.read_exact(&mut original).is_err() {
            break;
        }
        let filtered = skip_commands(&original);
        let batch = &filtered[..];
        batches += 1;
        // DUMP=1 prints every command.
        if std::env::var_os("DUMP").is_some() {
            for c in Reader::new(batch).expect("batch header") {
                let text = format!("{c:?}");
                println!("  {}", &text[..text.len().min(300)]);
            }
        }
        // Readbacks in this batch, saved once it has run.
        let reads: Vec<_> = Reader::new(batch)
            .expect("batch header")
            .filter_map(|c| match c {
                Ok(Command::ReadTexture { region, dest_offset, row_pitch, .. }) => {
                    Some((region.width, region.height, dest_offset as usize, row_pitch as usize))
                }
                _ => None,
            })
            .collect();
        if let Err(e) = core.execute(batch, &shared) {
            eprintln!("batch {batches}: {e:?}");
        }
        // Finished submissions free their staging memory only when polled
        // (a browser polls by itself).
        let _ = device.poll(wgpu::PollType::Poll);
        // Presented textures, read back into the shared region after the batch.
        let mut shown = Vec::new();
        for c in Reader::new(batch).expect("batch header").flatten() {
            match c {
                Command::CreateTexture { id, desc } => {
                    sizes.insert(id.0, (desc.width, desc.height));
                }
                Command::Present { texture, .. } => {
                    presents += 1;
                    if every_present > 0 && presents % every_present == 0 {
                        if let Some(&(w, h)) = sizes.get(&texture.0) {
                            shown.push((texture, w, h, presents));
                        }
                    }
                }
                _ => {}
            }
        }
        for (texture, w, h, n) in shown {
            let pitch = w * 4;
            if (pitch * h) as usize > shared.len() {
                continue;
            }
            let mut wr = d3dgpu_proto::Writer::new();
            let region = d3dgpu_proto::TextureRegion { texture, face: 0, level: 0, x: 0, y: 0, z: 0, width: w, height: h, depth: 1 };
            wr.read_texture(&region, 0, pitch, pitch * h, 0);
            if let Err(e) = core.execute(&wr.finish(), &shared) {
                eprintln!("present {n}: {e:?}");
                continue;
            }
            core.wait(&mut shared);
            let file = out.join(format!("present-{n:05}.png"));
            write_png(&file, w, h, &shared, pitch as usize);
            println!("batch {batches}: present {n} {w}x{h} -> {}", file.display());
        }
        // Reads complete (and free their staging buffers) whether or not they are saved.
        if !reads.is_empty() {
            core.wait(&mut shared);
        }
        if save_reads && !reads.is_empty() {
            for (w, h, offset, pitch) in reads {
                reads_seen += 1;
                if (reads_seen - 1) % read_every != 0 {
                    continue;
                }
                frames += 1;
                let file = out.join(format!("read-{frames:04}.png"));
                write_png(&file, w, h, &shared[offset..], pitch);
                println!("batch {batches}: read {w}x{h} -> {}", file.display());
            }
        }
    }
    core.wait(&mut shared);
    for m in core.log() {
        println!("core: {m}");
    }
    for e in errors.lock().unwrap().iter() {
        println!("webgpu: {e}");
    }
    println!("{batches} batches, {frames} readbacks, {presents} presents; {:?}", core.stats());
}

/// SKIP=a-b,c drops those commands (0-based, in the whole recording) to
/// bisect a problem.
fn skip_commands(batch: &[u8]) -> Vec<u8> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let Ok(spec) = std::env::var("SKIP") else { return batch.to_vec() };
    let ranges: Vec<(usize, usize)> = spec
        .split(',')
        .filter_map(|r| {
            let (a, b) = r.split_once('-').unwrap_or((r, r));
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .collect();
    let mut out = batch[..12].to_vec();
    let mut q = 12;
    while q + 8 <= batch.len() {
        let size = u32::from_le_bytes(batch[q + 4..q + 8].try_into().unwrap()) as usize;
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !ranges.iter().any(|&(a, b)| n >= a && n <= b) {
            out.extend_from_slice(&batch[q..q + size]);
        }
        q += size;
    }
    let len = out.len() as u32;
    out[8..12].copy_from_slice(&len.to_le_bytes());
    out
}

/// Writes 32-bit BGRA rows (Direct3D's A8R8G8B8/X8R8G8B8) as an opaque RGBA
/// PNG, uncompressed.
fn write_png(path: &std::path::Path, w: u32, h: u32, bgra: &[u8], pitch: usize) {
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        for x in 0..w as usize {
            let o = y * pitch + x * 4;
            raw.extend_from_slice(&[bgra[o + 2], bgra[o + 1], bgra[o], 255]);
        }
    }
    // zlib with stored deflate blocks.
    let mut z = vec![0x78, 0x01];
    for (i, chunk) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        z.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], body: &[u8]| {
        png.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let start = png.len();
        png.extend_from_slice(kind);
        png.extend_from_slice(body);
        let crc = crc32(&png[start..]);
        png.extend_from_slice(&crc.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::File::create(path).unwrap().write_all(&png).unwrap();
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
