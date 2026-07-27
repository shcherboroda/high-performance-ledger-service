use std::{env, fs};

use anyhow::{Context, Result, bail};

const OPENAPI_PATH: &str = "openapi.json";

fn main() -> Result<()> {
    let check = match env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [argument] if argument == "--check" => true,
        _ => bail!("usage: cargo run --bin generate-openapi [-- --check]"),
    };
    if check {
        let tracked_document = fs::read_to_string(OPENAPI_PATH)
            .with_context(|| format!("failed to read {OPENAPI_PATH}"))?;
        if !rust_backend_technical_assessment::openapi::matches_generated_document(
            &tracked_document,
        )
        .context("failed to serialize generated OpenAPI document")?
        {
            bail!(
                "{OPENAPI_PATH} is out of date; run `cargo run --bin generate-openapi` to regenerate it"
            );
        }
        return Ok(());
    }

    let generated_document = rust_backend_technical_assessment::openapi::generated_document()
        .context("failed to serialize generated OpenAPI document")?;
    fs::write(OPENAPI_PATH, generated_document)
        .with_context(|| format!("failed to write {OPENAPI_PATH}"))?;
    Ok(())
}
