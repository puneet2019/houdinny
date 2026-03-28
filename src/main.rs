use anyhow::Result;

fn main() -> Result<()> {
    println!("houdinny v{}", env!("CARGO_PKG_VERSION"));
    println!("Privacy proxy for AI agents");
    println!("Coming soon: run `houdinny --help` for usage");
    Ok(())
}
