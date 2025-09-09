use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::from_filename(".env.default")?;
    dotenvy::from_filename_override(".env")?;

    Ok(())
}
