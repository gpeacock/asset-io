use asset_io::Asset;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Opening PNG file...");
    let result = Asset::open("target/output_c2pa.png");
    
    match result {
        Ok(mut asset) => {
            println!("✅ File opened successfully!");
            println!("Container: {:?}", asset.container());
            println!("Total size: {}", asset.structure().total_size);
            println!("Segments: {}", asset.structure().segments().len());
            
            // Check for JUMBF
            if let Some(jumbf) = asset.jumbf()? {
                println!("JUMBF found: {} bytes", jumbf.len());
            }
        }
        Err(e) => {
            println!("❌ Error: {:?}", e);
            
            // Check actual file size
            use std::fs;
            if let Ok(metadata) = fs::metadata("target/output_c2pa.png") {
                println!("Actual file size: {} bytes", metadata.len());
            }
        }
    }
    
    Ok(())
}
