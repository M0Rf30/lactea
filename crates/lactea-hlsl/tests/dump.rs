//! Debug helper: `HLSL_DUMP="substring of preset path" cargo test --test dump -- --nocapture`
mod common;
use common::*;
use lactea_hlsl::{Options, md2_wrapped_hlsl, translate};

#[test]
fn dump_selected() {
    let Ok(pat) = std::env::var("HLSL_DUMP") else {
        return;
    };
    let want_wrapped = std::env::var("HLSL_WRAPPED").is_ok();
    for c in corpus_cases().iter().filter(|c| c.label().contains(&pat)) {
        println!("===== {}", c.label());
        println!("{}", c.text);
        if want_wrapped {
            println!(
                "----- wrapped\n{}",
                md2_wrapped_hlsl(c.kind, &c.text).unwrap()
            );
        }
        match translate(c.kind, &c.text, &Options::default()) {
            Ok(t) => {
                println!(
                    "----- wgsl\n{}",
                    t.wgsl
                        .split_once("@group(1) @binding(0)")
                        .map(|x| x.1)
                        .unwrap_or(&t.wgsl)
                );
                println!("{:?}", t.interface);
            }
            Err(e) => println!("ERROR {e}"),
        }
    }
}

#[test]
fn dump_corpus_wrapped() {
    let Ok(dir) = std::env::var("HLSL_WRAP_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    for (i, c) in corpus_cases().iter().enumerate() {
        let w = md2_wrapped_hlsl(c.kind, &c.text).unwrap_or_default();
        std::fs::write(format!("{dir}/{i:04}.hlsl"), w).unwrap();
        std::fs::write(format!("{dir}/{i:04}.label"), c.label()).unwrap();
    }
}
