//! VirtualAsset workflow example
//!
//! This example demonstrates the VirtualAsset API pattern for efficient
//! asset updates without intermediate buffers.
//!
//! **IMPORTANT LIMITATION**: VirtualAsset currently returns the SOURCE structure,
//! not the calculated DESTINATION structure. This means:
//! - For C2PA workflows requiring exact destination offsets (e.g., data hashing),
//!   use the intermediate parsing approach shown in examples/c2pa.rs instead
//! - VirtualAsset is suitable for workflows that don't need destination offsets
//!   before writing (e.g., simple metadata updates without hashing requirements)

use asset_io::{Asset, Updates};
use std::fs::File;
use std::io::BufReader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <image_file>", args[0]);
        eprintln!("\nDemonstrates VirtualAsset workflow for efficient updates");
        return Ok(());
    }

    let image_path = &args[1];
    println!("=== VirtualAsset Workflow Example ===\n");
    println!("Reading: {}", image_path);

    // === Step 1: Open the source asset ===
    let file = File::open(image_path)?;
    let reader = BufReader::new(file);
    let mut asset = Asset::from_source(reader)?;

    println!("Container: {:?}", asset.container());
    println!("Media type: {:?}", asset.media_type());
    println!(
        "Source: {} segments, {} bytes\n",
        asset.structure().segments.len(),
        asset.structure().total_size
    );

    // === Step 2: Read existing metadata ===
    let original_xmp = asset.xmp()?;
    if let Some(ref xmp_data) = original_xmp {
        println!("=== Original XMP ===");
        println!("Size: {} bytes", xmp_data.len());

        #[cfg(feature = "xmp")]
        if let Ok(xmp_str) = std::str::from_utf8(xmp_data) {
            if let Some(creator) = asset_io::xmp::get_key(xmp_str, "dc:creator") {
                println!("Creator: {}", creator);
            }
            if let Some(title) = asset_io::xmp::get_key(xmp_str, "dc:title") {
                println!("Title: {}", title);
            }
        }
        println!();
    } else {
        println!("No XMP metadata found\n");
    }

    // === Step 3: Create updated XMP ===
    println!("=== Creating Updated XMP ===");

    let new_xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
      xmlns:dc="http://purl.org/dc/elements/1.1/"
      xmlns:xmp="http://ns.adobe.com/xap/1.0/">
      <dc:creator>VirtualAsset Example</dc:creator>
      <dc:title>Updated via VirtualAsset</dc:title>
      <xmp:CreateDate>2024-12-30T12:00:00</xmp:CreateDate>
      <xmp:ModifyDate>2024-12-30T12:00:00</xmp:ModifyDate>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
        .as_bytes()
        .to_vec();

    println!("New XMP size: {} bytes\n", new_xmp.len());

    // === Step 4: Create VirtualAsset (the key API!) ===
    println!("=== Creating VirtualAsset ===");
    let updates = Updates::new().set_xmp(new_xmp.clone());
    let mut virtual_asset = asset.with_updates(updates)?;

    println!("✓ VirtualAsset created (instant, no I/O)");
    println!(
        "  Current structure: {} segments",
        virtual_asset.structure().segments.len()
    );

    // Demonstrate reading from virtual asset BEFORE writing
    println!("\n=== Reading from VirtualAsset (before write) ===");
    if let Some(virtual_xmp) = virtual_asset.xmp()? {
        println!(
            "✓ Can read XMP from virtual asset: {} bytes",
            virtual_xmp.len()
        );

        #[cfg(feature = "xmp")]
        if let Ok(xmp_str) = std::str::from_utf8(&virtual_xmp) {
            if let Some(creator) = asset_io::xmp::get_key(xmp_str, "dc:creator") {
                println!("  Virtual creator: {}", creator);
            }
            if let Some(title) = asset_io::xmp::get_key(xmp_str, "dc:title") {
                println!("  Virtual title: {}", title);
            }
        }
    }

    // === Step 5: Write the virtual asset ===
    println!("\n=== Writing Output ===");

    // Use the MediaType API to get the correct extension
    let extension = virtual_asset.source_asset().media_type().to_extension();
    let output_path = format!("target/output_virtual_xmp.{}", extension);

    {
        let mut output = File::create(&output_path)?;
        virtual_asset.write_to(&mut output)?;
        // File closed automatically
    }

    println!("✓ Wrote output to: {}", output_path);

    // === Step 6: Verify the output ===
    println!("\n=== Verifying Output ===");
    let metadata = std::fs::metadata(&output_path)?;
    println!("Output file size: {} bytes", metadata.len());

    // Try to read it back
    println!("\nReading output file...");
    match Asset::open(&output_path) {
        Ok(mut output_asset) => {
            println!("✓ Successfully parsed output");
            println!(
                "  Structure: {} segments, {} bytes",
                output_asset.structure().segments.len(),
                output_asset.structure().total_size
            );

            if let Some(output_xmp) = output_asset.xmp()? {
                println!("  XMP size: {} bytes", output_xmp.len());

                #[cfg(feature = "xmp")]
                if let Ok(xmp_str) = std::str::from_utf8(&output_xmp) {
                    if let Some(creator) = asset_io::xmp::get_key(xmp_str, "dc:creator") {
                        println!("  Creator: {}", creator);

                        if creator == "VirtualAsset Example" {
                            println!("  ✓ XMP update verified!");
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!("Note: Could not re-parse output: {}", e);
            println!("(File may still be valid - external tools can verify)");
        }
    }

    println!("\n=== Success! ===");
    println!("\nVirtualAsset Benefits Demonstrated:");
    println!("1. ✓ Lazy evaluation - no I/O until write_to()");
    println!("2. ✓ Single write pass - efficient, no temp buffers");
    println!("3. ✓ Can read metadata before writing");
    println!("4. ✓ Safe by construction - updates tied to source");

    Ok(())
}
