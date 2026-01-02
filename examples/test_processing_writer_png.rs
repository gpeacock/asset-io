use asset_io::processing_writer::ProcessingWriter;
use asset_io::{Asset, Updates};
use std::fs::File;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = File::open("tests/fixtures/sample1.png")?;
    let mut asset = Asset::from_source(input)?;

    let jumbf_data = b"Test JUMBF with ProcessingWriter".to_vec();
    let updates = Updates::new().set_jumbf(jumbf_data.clone());

    let output = File::create("/tmp/test_processing_writer.png")?;
    let mut processing_writer = ProcessingWriter::new(output, |_data| {
        // Process nothing, just forward
    });

    asset.write(&mut processing_writer, &updates)?;
    drop(processing_writer);

    println!("✅ Written with ProcessingWriter");

    // Try to read it back
    let mut verify = Asset::open("/tmp/test_processing_writer.png")?;
    let result = verify.jumbf()?;

    if let Some(extracted) = result {
        println!("✅ JUMBF extracted: {} bytes", extracted.len());
    } else {
        println!("❌ No JUMBF found!");
    }

    Ok(())
}
