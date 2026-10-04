use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=WUAPI_VIDEO_BUNDLE");
    println!("cargo:rerun-if-env-changed=WUAPI_VIDEO_BUNDLE_REQUIRED");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    let payload = match env::var_os("WUAPI_VIDEO_BUNDLE") {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(path.is_absolute(), "WUAPI_VIDEO_BUNDLE must be absolute");
            println!("cargo:rerun-if-changed={}", path.display());
            fs::read(path).expect("read WUAPI_VIDEO_BUNDLE")
        }
        None => {
            assert!(
                env::var_os("WUAPI_VIDEO_BUNDLE_REQUIRED").is_none(),
                "release requires WUAPI_VIDEO_BUNDLE"
            );
            Vec::new()
        }
    };
    fs::write(out.join("video-bundle.tar.gz"), payload).expect("embed video runtime");
}
