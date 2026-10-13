//! Builds the C++ preflate library (third_party/preflate) plus a small C wrapper.

fn main() {
    let dir = std::path::Path::new("../third_party/preflate");
    let lib = [
        "block_decoder",
        "block_reencoder",
        "block_trees",
        "complevel_estimator",
        "constants",
        "decoder",
        "hash_chain",
        "info",
        "parameter_estimator",
        "parser_config",
        "predictor_state",
        "reencoder",
        "seq_chain",
        "statistical_codec",
        "statistical_model",
        "token",
        "token_predictor",
        "tree_predictor",
    ];
    let support = [
        "arithmetic_coder",
        "array_helper",
        "bit_helper",
        "bitstream",
        "const_division",
        "huffman_decoder",
        "huffman_encoder",
        "huffman_helper",
        "memstream",
        "outputcachestream",
        "task_pool",
    ];
    let mut b = cc::Build::new();
    b.cpp(true)
        .std("c++14")
        .opt_level(3)
        .warnings(false)
        .include(dir)
        .file("csrc/wrap.cpp");
    for s in lib {
        b.file(dir.join(format!("preflate_{s}.cpp")));
    }
    for s in support {
        b.file(dir.join(format!("support/{s}.cpp")));
    }
    b.compile("preflate");
    println!("cargo:rerun-if-changed=csrc/wrap.cpp");
    println!("cargo:rerun-if-changed=../third_party/preflate");
}
