// The material component library is vendored under material-1.0/ (MIT, from
// slint-ui/material-rust-template) and registered as a Slint library path, so
// .slint files can `import { … } from "@material"`.
fn main() {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let config = slint_build::CompilerConfiguration::new().with_library_paths(
        std::collections::HashMap::from([(
            "material".to_string(),
            std::path::Path::new(&manifest).join("material-1.0/material.slint"),
        )]),
    );
    slint_build::compile_with_config("ui/app.slint", config).expect("slint build failed");
}
