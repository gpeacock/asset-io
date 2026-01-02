use asset_io::{Asset, Updates};
use std::fs::File;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = File::open("tests/fixtures/sample1.png")?;
    let mut asset = Asset::from_source(input)?;

    let jumbf_data = b"Test JUMBF data for debugging".to_vec();
    let updates = Updates::new().set_jumbf(jumbf_data.clone());

    let mut output = File::create("/tmp/test_png_jumbf.png")?;
    asset.write(&mut output, &updates)?;
    drop(output);

    println!("✅ Written successfully");

    // Now try to read it back
    let mut verify = Asset::open("/tmp/test_png_jumbf.png")?;
    let result = verify.jumbf()?;

    if let Some(extracted) = result {
        println!("✅ JUMBF extracted: {} bytes", extracted.len());
        assert_eq!(extracted, jumbf_data);
        println!("✅ JUMBF matches!");
    } else {
        println!("❌ No JUMBF found in output!");
    }

    Ok(())
}
