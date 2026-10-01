fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent-dark".into())
            // Rasterize the app's glyphs at build time for the software
            // renderer. This keeps small labels aligned to the pixel grid
            // instead of relying on unhinted runtime outline rendering.
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .expect("compile Slint UI");
}
