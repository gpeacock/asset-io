use asset_io::Asset;
use c2pa::Reader;
use std::env;

fn main() -> asset_io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <image_file>", args[0]);
        std::process::exit(1);
    }

    let filename = &args[1];
    println!("Parsing: {}", filename);

    // Auto-detect format and parse
    let mut asset = Asset::open(filename)?;

    println!("\nFile structure:");
    println!("  Container: {:?}", asset.container());
    println!("  Media type: {:?}", asset.media_type());
    println!("  Total size: {} bytes", asset.structure().total_size);
    println!("  Segments: {}", asset.structure().segments.len());

    // Check for XMP (loads lazily only if present)
    match asset.xmp()? {
        Some(xmp) => {
            println!("\n✓ Found XMP metadata ({} bytes)", xmp.len());
            // Parse some common XMP fields
            use asset_io::xmp::get_keys;
            let xmp_str = String::from_utf8_lossy(&xmp);
            let values = get_keys(
                &xmp_str,
                &["dcterms:provenance", "xmpMM:InstanceID", "xmpMM:DocumentID"],
            );

            println!("  dcterms:provenance: {:?}", values[0]);
            println!("  xmpMM:InstanceID: {:?}", values[1]);
            println!("  xmpMM:DocumentID: {:?}", values[2]);
        }
        None => println!("\n✗ No XMP metadata found"),
    }

    // Check for JUMBF (loads and assembles only if present)
    match asset.jumbf()? {
        Some(jumbf) => {
            println!("\n✓ Found JUMBF data ({} bytes)", jumbf.len());
            // Now format.to_string() works!
            let reader = Reader::from_manifest_data_and_stream(
                &jumbf,
                &asset.media_type().to_string(),
                asset.source_mut(),
            );
            println!("  Reader: {:?}", reader);
        }
        None => println!("\n✗ No JUMBF data found"),
    }

    Ok(())
}
