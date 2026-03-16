use asset_io::Asset;
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <file>", args[0]);
        return Ok(());
    }

    let asset = Asset::open(&args[1])?;
    println!("File: {}", args[1]);
    println!("Segments:");
    for (i, seg) in asset.structure().segments().iter().enumerate() {
        let label = match &seg.path {
            Some(p) if !p.is_empty() => format!("{:?} ({})", seg.kind, p),
            _ => format!("{:?}", seg.kind),
        };
        println!(
            "  {}: {} at offset={}, size={}",
            i,
            label,
            seg.location().offset,
            seg.location().size
        );
    }

    Ok(())
}
