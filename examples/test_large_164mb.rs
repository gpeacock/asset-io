use asset_io::{Asset, Updates};
use sha2::{Digest, Sha512};
use std::fs::OpenOptions;
use std::io::Seek;
use std::time::Instant;

fn main() -> asset_io::Result<()> {
    let input = "/Users/gpeacock/Downloads/Guest - Robert (06-24-25) (98th Birthday).mov";

    println!("🎯 Testing 164MB QuickTime MOV File");
    println!("This tests performance on a larger BMFF file\n");

    // 1. Parse
    let start = Instant::now();
    let mut asset = Asset::open(input)?;
    let parse_time = start.elapsed();

    let file_size_mb = asset.structure().total_size as f64 / 1024.0 / 1024.0;

    println!("✓ Parse: {:?}", parse_time);
    println!("  Size: {:.2} MB", file_size_mb);
    println!("  Segments: {}", asset.structure().segments().len());

    // 2. Write with JUMBF + Hash (simulating C2PA workflow)
    println!("\n⚡ Testing Write + Hash...");

    let placeholder_jumbf = vec![0u8; 20000];
    let updates = Updates::new().set_jumbf(placeholder_jumbf);

    let start = Instant::now();

    // Open output with read+write
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open("/tmp/test_large_164mb.mov")?;

    // Write the file
    // Note: Offset adjustment is not needed because UUID boxes are inserted
    // right after ftyp, before moov/mdat, so media data offsets don't change
    asset.write(&mut output, &updates)?;

    let elapsed = start.elapsed();

    // Hash the written file (excluding JUMBF)
    output.seek(std::io::SeekFrom::Start(0))?;
    let mut hasher = Sha512::new();
    let mut verify_asset = Asset::from_source(&mut output)?;

    let jumbf_index = verify_asset.structure().c2pa_jumbf_index();
    let exclude_indices = if let Some(idx) = jumbf_index {
        vec![Some(idx)]
    } else {
        vec![]
    };
    verify_asset.hash_excluding_segments(&exclude_indices, &mut hasher)?;

    let hash = hasher.finalize();

    drop(output); // Close file

    println!("✅ Complete!");
    println!("   Time: {:?}", elapsed);
    println!(
        "   Throughput: {:.1} MB/s",
        file_size_mb / elapsed.as_secs_f64()
    );
    println!(
        "   Hash: {}...",
        hash[0..16]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    );

    // Verify file is valid
    println!("\n🔍 Verifying output...");
    let mut verify = Asset::open("/tmp/test_large_164mb.mov")?;
    if verify.jumbf()?.is_some() {
        println!("   ✓ JUMBF found");
    }
    println!(
        "   ✓ File size: {:.2} MB",
        verify.structure().total_size as f64 / 1024.0 / 1024.0
    );

    // Extrapolate to 3.5GB
    let time_for_3_5gb = elapsed.as_secs_f64() * (3500.0 / file_size_mb);
    println!("\n📊 Extrapolation to 3.5GB:");
    println!("   Estimated time: {:.1}s", time_for_3_5gb);
    println!(
        "   Estimated throughput: {:.1} MB/s",
        3500.0 / time_for_3_5gb
    );

    println!("\n🎬 Output: /tmp/test_large_164mb.mov");
    println!("   Test playback: open /tmp/test_large_164mb.mov");

    Ok(())
}
