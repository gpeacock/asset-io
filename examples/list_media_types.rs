//! Example: List all supported media types
//!
//! This example demonstrates how to query which media types are available
//! at runtime based on the features that were compiled in.

use asset_io::MediaType;

fn main() {
    println!("asset-io - Supported Media Types\n");
    println!("=================================\n");

    let supported = MediaType::all();

    if supported.is_empty() {
        println!("No media types are enabled!");
        println!("Compile with features like: --features jpeg,png");
        return;
    }

    println!("This build supports {} media type(s):\n", supported.len());

    for media_type in supported {
        println!(
            "  {} (.{})",
            media_type.to_mime(),
            media_type.to_extension()
        );
        println!("    Container: {:?}", media_type.container());
        println!();
    }

    println!("Available features:");
    println!("  --features jpeg    - JPEG/JFIF support");
    println!("  --features png     - PNG support");
    println!("\nExample: cargo run --example list_media_types --features jpeg,png");
}
