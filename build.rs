fn main() {
    println!("cargo:rerun-if-changed=assets/dsh.rc");
    println!("cargo:rerun-if-changed=assets/dsh-whale.ico");
    let _ = embed_resource::compile("assets/dsh.rc", embed_resource::NONE);
}
