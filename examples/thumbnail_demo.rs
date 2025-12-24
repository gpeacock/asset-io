// Thumbnail Generation Interface Demo
//
// This example demonstrates the format-agnostic thumbnail generation interface.
// The core library provides efficient access to image data, and external crates
// handle the actual decoding and thumbnail generation.

use jumbf_io::{FormatHandler, JpegHandler, ThumbnailOptions};
use std::fs::File;

fn main() -> jumbf_io::Result<()> {
    #[cfg(feature = "test-utils")]
    {
        use jumbf_io::test_utils::{fixture_path, P1000708};

        println!("=== Thumbnail Generation Interface Demo ===\n");

        let path = fixture_path(P1000708);
        println!("File: {}", path.display());

        // Open the asset
        let mut file = File::open(&path)?;
        let handler = JpegHandler::new();
        let structure = handler.parse(&mut file)?;

        println!("Format: {:?}", structure.format);
        println!("Total size: {} bytes\n", structure.total_size);

        // ========================================================================
        // STEP 1: Try embedded thumbnail (fastest path!)
        // ========================================================================

        println!("1. Checking for embedded thumbnail...");
        match structure.embedded_thumbnail()? {
            Some(thumb) => {
                println!("   ✓ Found embedded thumbnail!");
                println!("     Format: {:?}", thumb.format);
                if let (Some(w), Some(h)) = (thumb.width, thumb.height) {
                    println!("     Size: {}x{}", w, h);
                }
                println!("     Data: {} bytes", thumb.data.len());

                // Check if it fits our requirements
                let options = ThumbnailOptions::default();
                if thumb.fits(options.max_width, options.max_height) {
                    println!("     ✓ Fits requirements ({}x{})", options.max_width, options.max_height);
                    println!("     → Can use directly without decoding!");
                } else {
                    println!("     ⚠ Too large, would need resizing");
                }
            }
            None => {
                println!("   ⚠ No embedded thumbnail found");
                println!("   → Would need to decode main image");
            }
        }

        println!();

        // ========================================================================
        // STEP 2: Get image data range (for external decoder)
        // ========================================================================

        println!("2. Getting image data range...");
        if let Some(range) = structure.image_data_range() {
            println!("   ✓ Image data found");
            println!("     Offset: {} bytes", range.offset);
            println!("     Size: {} bytes ({:.2} MB)", range.size, range.size as f64 / 1024.0 / 1024.0);
            println!();
            println!("   This range can be:");
            println!("   • Accessed via memory-mapping (zero-copy)");
            println!("   • Passed to external decoder (image crate, mozjpeg, etc.)");
            println!("   • Streamed in chunks for constant memory");
        } else {
            println!("   ✗ No image data found");
        }

        println!();

        // ========================================================================
        // STEP 3: Demonstrate zero-copy access (with memory-mapped)
        // ========================================================================

        #[cfg(feature = "memory-mapped")]
        {
            println!("3. Zero-copy access with memory-mapping...");

            let file = File::open(&path)?;
            let mmap = unsafe { memmap2::Mmap::map(&file)? };

            let mut file = File::open(&path)?;
            let structure = handler.parse(&mut file)?;
            let structure = structure.with_mmap(mmap);

            if let Some(range) = structure.image_data_range() {
                if let Some(slice) = structure.get_mmap_slice(range) {
                    println!("   ✓ Got zero-copy slice!");
                    println!("     Size: {} bytes", slice.len());
                    println!("     First 16 bytes: {:02X?}", &slice[..16.min(slice.len())]);
                    println!();
                    println!("   External decoder would receive:");
                    println!("   • Direct pointer to image data");
                    println!("   • No copying or allocation");
                    println!("   • Maximum decode speed");
                }
            }
        }

        #[cfg(not(feature = "memory-mapped"))]
        {
            println!("3. Zero-copy access (disabled)");
            println!("   Run with --features memory-mapped to see zero-copy demo");
        }

        println!();

        // ========================================================================
        // STEP 4: Show how external crate would use this
        // ========================================================================

        println!("4. External thumbnail generator pattern...");
        println!();
        println!("   An external crate (like 'jumbf-thumbnails') would:");
        println!();
        println!("   ```rust");
        println!("   pub fn generate_thumbnail(asset: &mut Asset) -> Result<Vec<u8>> {{");
        println!("       // Fast path: embedded thumbnail");
        println!("       if let Some(thumb) = asset.structure().embedded_thumbnail()? {{");
        println!("           if thumb.fits(256, 256) {{");
        println!("               return Ok(thumb.data);  // Done!");
        println!("           }}");
        println!("       }}");
        println!();
        println!("       // Medium path: zero-copy decode");
        println!("       if let Some(range) = asset.structure().image_data_range() {{");
        println!("           if let Some(slice) = asset.structure().get_mmap_slice(range) {{");
        println!("               return decode_and_thumbnail(slice)?;  // Zero-copy!");
        println!("           }}");
        println!("       }}");
        println!();
        println!("       // Slow path: read and decode");
        println!("       let data = asset.read_image_data()?;");
        println!("       decode_and_thumbnail(&data)");
        println!("   }}");
        println!("   ```");
        println!();

        // ========================================================================
        // SUMMARY
        // ========================================================================

        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("SUMMARY");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!();
        println!("The jumbf-io library provides THREE thumbnail paths:");
        println!();
        println!("1. Embedded Thumbnails (Instant)");
        println!("   • JPEG EXIF: ~160x120, already encoded");
        println!("   • HEIF 'thmb': Variable size");
        println!("   • WebP VP8L: Optional chunk");
        println!("   • No decoding needed!");
        println!();
        println!("2. Zero-Copy Decode (Fast)");
        println!("   • Memory-mapped access");
        println!("   • Direct pointer to compressed data");
        println!("   • External decoder at full speed");
        println!();
        println!("3. Streaming (Memory-Efficient)");
        println!("   • Constant memory usage");
        println!("   • Process in chunks");
        println!("   • Works for huge files");
        println!();
        println!("All without adding image decoding dependencies!");
        println!("External crates choose their decoder:");
        println!("  • 'image' crate (pure Rust, many formats)");
        println!("  • mozjpeg-sys (faster JPEG)");
        println!("  • libwebp-sys (faster WebP)");
        println!("  • Custom decoders");
        println!();
        println!("Keep jumbf-io lean: 435 KB");
        println!("Add thumbnails when needed: separate crate");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    }

    #[cfg(not(feature = "test-utils"))]
    {
        println!("This example requires test-utils:");
        println!("  cargo run --example thumbnail_demo");
    }

    Ok(())
}
