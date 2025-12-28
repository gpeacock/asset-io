use asset_io::{Asset, Updates};
use std::fs;

/// Quick reference showing all supported metadata operations
fn main() -> asset_io::Result<()> {
    let input = "image.jpg";

    // ============================================
    // BASIC OPERATIONS
    // ============================================

    // 1. Keep everything unchanged (copy)
    let mut asset = Asset::open(input)?;
    asset.write_to("output.jpg", &Updates::default())?;

    // ============================================
    // REMOVE OPERATIONS
    // ============================================

    // 2. Remove only XMP
    let mut asset = Asset::open(input)?;
    asset.write_to("output.jpg", &Updates::new().remove_xmp())?;

    // 3. Remove only JUMBF
    let mut asset = Asset::open(input)?;
    asset.write_to("output.jpg", &Updates::new().remove_jumbf())?;

    // 4. Remove both (convenience method)
    let mut asset = Asset::open(input)?;
    asset.write_to("output.jpg", &Updates::remove_all())?;

    // ============================================
    // REPLACE OPERATIONS
    // ============================================

    // 5. Replace XMP, keep JUMBF
    let new_xmp = b"<rdf:RDF>...</rdf:RDF>".to_vec();
    let mut asset = Asset::open(input)?;
    asset.write_to("output.jpg", &Updates::new().set_xmp(new_xmp.clone()))?;

    // 6. Replace JUMBF, keep XMP
    let new_jumbf = fs::read("new_c2pa.jumbf")?;
    let mut asset = Asset::open(input)?;
    asset.write_to("output.jpg", &Updates::new().set_jumbf(new_jumbf.clone()))?;

    // 7. Replace both
    let mut asset = Asset::open(input)?;
    asset.write_to(
        "output.jpg",
        &Updates::new()
            .set_xmp(new_xmp.clone())
            .set_jumbf(new_jumbf.clone()),
    )?;

    // ============================================
    // ADD OPERATIONS (when metadata doesn't exist)
    // ============================================

    // 8. Add XMP to file without XMP
    let mut asset = Asset::open("no_metadata.jpg")?;
    asset.write_to("output.jpg", &Updates::with_xmp(new_xmp.clone()))?;

    // 9. Add JUMBF to file without JUMBF
    let mut asset = Asset::open("no_metadata.jpg")?;
    asset.write_to("output.jpg", &Updates::with_jumbf(new_jumbf.clone()))?;

    // ============================================
    // MIXED OPERATIONS
    // ============================================

    // 10. Replace XMP and remove JUMBF
    let mut asset = Asset::open(input)?;
    asset.write_to(
        "output.jpg",
        &Updates::new().set_xmp(new_xmp.clone()).remove_jumbf(),
    )?;

    // 11. Remove XMP and replace JUMBF
    let mut asset = Asset::open(input)?;
    asset.write_to(
        "output.jpg",
        &Updates::new().remove_xmp().set_jumbf(new_jumbf.clone()),
    )?;

    // ============================================
    // TRANSFERRING METADATA BETWEEN FILES
    // ============================================

    // 12. Extract JUMBF from one file and add to another
    let mut source = Asset::open("source.jpg")?;
    if let Some(jumbf_data) = source.jumbf()? {
        let mut target = Asset::open("target.jpg")?;
        target.write_to("output.jpg", &Updates::new().set_jumbf(jumbf_data))?;
    }

    // ============================================
    // READING METADATA
    // ============================================

    // 13. Check what metadata exists
    let mut asset = Asset::open(input)?;

    if let Some(xmp) = asset.xmp()? {
        println!("Has XMP: {} bytes", xmp.len());
    } else {
        println!("No XMP");
    }

    if let Some(jumbf) = asset.jumbf()? {
        println!("Has JUMBF: {} bytes", jumbf.len());
    } else {
        println!("No JUMBF");
    }

    Ok(())
}
