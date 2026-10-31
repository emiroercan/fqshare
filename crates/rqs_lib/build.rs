extern crate prost_build;

fn main() {
    if std::env::var_os("PROTOC").is_none() {
        // SAFETY: build scripts are single-threaded at this point.
        unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap()) };
    }
    prost_build::compile_protos(
        &[
            "src/proto_src/device_to_device_messages.proto",
            "src/proto_src/offline_wire_formats.proto",
            "src/proto_src/securegcm.proto",
            "src/proto_src/securemessage.proto",
            "src/proto_src/ukey.proto",
            "src/proto_src/wire_format.proto",
        ],
        &["src/proto_src"],
    )
    .unwrap();
}
