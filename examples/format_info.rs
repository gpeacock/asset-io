use asset_io::{Asset, Container};
use std::env;

fn main() -> asset_io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        // Demo with known formats
        #[cfg(feature = "jpeg")]
        {
            let jpeg = Container::Jpeg;
            println!("JPEG container:");
            println!("  Primary MIME: {}", jpeg.to_mime());
            println!("  All MIME types: {:?}", jpeg.mime_types());
            println!("  Primary extension: {}", jpeg.to_extension());
            println!("  All extensions: {:?}", jpeg.extensions());
            println!("  Display: {}", jpeg); // Uses Display trait
            println!("  to_string(): {}", jpeg.to_string());
        }

        #[cfg(feature = "png")]
        {
            println!();
            let png = Container::Png;
            println!("PNG container:");
            println!("  Primary MIME: {}", png.to_mime());
            println!("  All MIME types: {:?}", png.mime_types());
            println!("  Primary extension: {}", png.to_extension());
            println!("  All extensions: {:?}", png.extensions());
            println!("  Display: {}", png);
            println!("  to_string(): {}", png.to_string());
        }

        println!("\nUsage: {} <image_file>", args[0]);
        println!("  Opens an image and displays its format information");
        return Ok(());
    }

    let filename = &args[1];
    println!("Opening: {}", filename);

    // Auto-detect format and parse
    let asset = Asset::open(filename)?;
    let container = asset.container();
    let media_type = asset.media_type();

    println!("\nDetected format:");
    println!("  Container: {:?}", container);
    println!("  Media type: {:?}", media_type);
    println!("  Container MIME: {}", container.to_mime());
    println!("  Media MIME: {}", media_type.to_mime());
    println!("  All container MIME types: {:?}", container.mime_types());
    println!("  Primary extension: {}", container.to_extension());
    println!("  All extensions: {:?}", container.extensions());
    println!("  Display string: {}", container);

    Ok(())
}
