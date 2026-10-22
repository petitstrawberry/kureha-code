use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=app.toml");
    let path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("app.toml");
    let recipe: toml::Value = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    for (key, variable) in [("id", "APP_ID"), ("name", "APP_NAME")] {
        let value = recipe["app"][key].as_str().expect("application metadata");
        assert!(!value.is_empty() && !value.chars().any(char::is_control));
        println!("cargo:rustc-env={variable}={value}");
    }
}
