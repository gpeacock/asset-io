use asset_io::Asset;
use sha2::{Digest, Sha512};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = "tests/fixtures/massive_test.png";
    let mut asset = Asset::open(path)?;
    
    let start = Instant::now();
    
    let jumbf_idx = asset.structure().c2pa_jumbf_index();
    let mut hasher = Sha512::new();
    asset.hash_excluding_segments(&[jumbf_idx], &mut hasher)?;
    let _hash = hasher.finalize();
    
    let elapsed = start.elapsed();
    let file_size = std::fs::metadata(path)?.len();
    let mb_per_sec = (file_size as f64 / 1024.0 / 1024.0) / elapsed.as_secs_f64();
    
    println!("Hashed {} MB in {:.2}s = {:.0} MB/s", 
        file_size / 1024 / 1024, 
        elapsed.as_secs_f64(),
        mb_per_sec);
    
    Ok(())
}
