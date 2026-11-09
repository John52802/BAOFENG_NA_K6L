// Asks Windows for modern-looking controls; without this they use the classic style.
// Also embeds the firmware and voices when they exist locally: they are not in the repository.
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(k6_embedded)");
    let files = ["../../out/bf_uvk6-rx-spectrum.bin", "../../out/voces_es_gonzalo/voces_es.bin"];
    for f in files {
        println!("cargo:rerun-if-changed={f}");
    }
    if files.iter().all(|f| Path::new(f).is_file()) {
        println!("cargo:rustc-cfg=k6_embedded");
    }
    embed_manifest::embed_manifest(embed_manifest::new_manifest("Baofeng.K6.Actualizar"))
        .expect("unable to embed manifest");
}
