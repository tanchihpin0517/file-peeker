fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost = prost_build::Config::new();
    prost.protoc_executable(protoc);

    tonic_prost_build::configure().compile_with_config(
        prost,
        &["proto/file_peeker.proto"],
        &["proto"],
    )?;
    println!("cargo:rerun-if-changed=proto/file_peeker.proto");
    Ok(())
}
