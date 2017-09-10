extern crate prost_build;

fn main() {
    let paths = &[
        "protobuf/domain.proto",
        "protobuf/requests.proto",
        "protobuf/responses.proto",
    ];

    prost_build::compile_protos(paths, &["protobuf/"]).unwrap();
}
