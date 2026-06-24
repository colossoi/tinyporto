//! Build-time shader compilation + pipeline-descriptor codegen.
//!
//! For each Wyn root, runs `wyn compile` (emitting SPIR-V + a `.json` pipeline
//! descriptor into OUT_DIR), embeds the `.spv` via `include_bytes!`, and
//! *translates the descriptor into Rust* — specializing the descriptor's
//! dispatch-size and output-size rules into inlined formulas (see
//! `codegen_pipeline`). The generated `descriptor.rs` carries no rule data the
//! driver walks at runtime; it carries the calculations themselves.

use std::path::PathBuf;
use std::process::Command;

use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;

// Wyn entry roots to compile+embed. (key, path-relative-to-repo-root.)
const ROOTS: &[(&str, &str)] = &[("main", "wyn/main.wyn")];

// ---- descriptor model (the subset of the wyn `*.json` we consume) ----

#[derive(serde::Deserialize)]
struct Descriptor {
    pipelines: Vec<Pipeline>,
}

#[derive(serde::Deserialize)]
struct Pipeline {
    kind: String,
    #[serde(default)]
    bindings: Vec<Binding>,
    #[serde(default)]
    stages: Vec<Stage>,
}

#[derive(serde::Deserialize)]
struct Binding {
    #[serde(rename = "type")]
    ty: String,
    set: u32,
    binding: u32,
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    usage: Option<String>,
    name: String,
    #[serde(default)]
    length: Option<Length>,
}

impl Binding {
    /// The `BindingKind` token for this binding (uniform / storage read / write).
    fn kind_tokens(&self) -> TokenStream {
        match (self.ty.as_str(), self.access.as_deref()) {
            ("uniform", _) => quote! { BindingKind::Uniform },
            ("storage_buffer", Some("write_only")) => quote! { BindingKind::StorageWrite },
            ("storage_buffer", _) => quote! { BindingKind::StorageRead },
            (other, _) => panic!("descriptor: unhandled binding type {other:?}"),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Length {
    /// A fixed byte size (e.g. a small fixed-shape output array).
    Fixed { bytes: u64 },
    /// Sized from an input binding: (src_bytes / src_elem_bytes) * elem_bytes.
    LikeInput { binding: u32, elem_bytes: u64, src_elem_bytes: u64 },
}

#[derive(serde::Deserialize)]
struct Stage {
    entry_point: String,
    #[serde(default)]
    dispatch_size: Option<DispatchSize>,
}

#[derive(serde::Deserialize)]
struct DispatchSize {
    len: Len,
    workgroup_size: u32,
}

#[derive(serde::Deserialize)]
struct Len {
    binding: u32,
    elem_bytes: u64,
}

fn id(s: &str) -> Ident {
    Ident::new(s, Span::call_site())
}

/// Name (`<name>_bytes`) of the input-byte-size parameter for binding `b`.
fn input_param(p: &Pipeline, b: u32) -> Ident {
    let name = p
        .bindings
        .iter()
        .find(|x| x.usage.as_deref() == Some("input") && x.binding == b)
        .map(|x| x.name.as_str())
        .unwrap_or_else(|| panic!("descriptor: no input binding {b}"));
    id(&format!("{name}_bytes"))
}

/// Translate one compute pipeline into `<entry>_dispatch` + `<entry>_out_bytes`
/// functions, with the descriptor's rules inlined as arithmetic. Non-compute
/// pipelines have nothing to compute, so they generate nothing.
fn codegen_pipeline(p: &Pipeline) -> TokenStream {
    if p.kind != "compute" {
        return quote! {};
    }
    let stage = &p.stages[0];
    let entry = stage.entry_point.as_str();
    let dispatch_fn = id(&format!("{entry}_dispatch"));
    let out_bytes_fn = id(&format!("{entry}_out_bytes"));

    // dispatch: ceil(input_len_elems / workgroup_size).
    let ds = stage.dispatch_size.as_ref().expect("compute stage has dispatch_size");
    let d_param = input_param(p, ds.len.binding);
    let d_elem = ds.len.elem_bytes;
    let d_wg = ds.workgroup_size;

    // outputs: one match arm per output binding, formula inlined from `length`.
    let outputs: Vec<&Binding> = p.bindings.iter().filter(|b| b.usage.as_deref() == Some("output")).collect();
    let mut params: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let arms: Vec<TokenStream> = outputs
        .iter()
        .map(|o| {
            let b = o.binding;
            let expr = match o.length.as_ref().expect("output binding has length") {
                Length::Fixed { bytes } => quote! { #bytes },
                Length::LikeInput { binding, elem_bytes, src_elem_bytes } => {
                    let src = input_param(p, *binding);
                    params.insert(src.to_string());
                    quote! { (#src / #src_elem_bytes) * #elem_bytes }
                }
            };
            quote! { #b => #expr }
        })
        .collect();
    let out_params: Vec<Ident> = params.iter().map(|s| id(s)).collect();

    quote! {
        /// Workgroup dispatch dims (descriptor: ceil(input_len / workgroup_size)).
        pub const fn #dispatch_fn(#d_param: u64) -> [u32; 3] {
            [((#d_param / #d_elem) as u32).div_ceil(#d_wg), 1, 1]
        }
        /// Byte size of output binding `binding` (descriptor `length` rules).
        pub const fn #out_bytes_fn(binding: u32, #(#out_params: u64),*) -> u64 {
            match binding {
                #(#arms,)*
                _ => panic!("binding is not an output of this pipeline"),
            }
        }
    }
}

/// Generate the bind-table static for a pipeline's entry: the (set, binding,
/// kind, name) tuples the driver maps to its resources. This is the descriptor's
/// declared binding interface — the data that was hand-synced before.
fn codegen_bindings(p: &Pipeline) -> TokenStream {
    let Some(stage) = p.stages.first() else { return quote! {} };
    let table = id(&format!("{}_BINDINGS", stage.entry_point.to_uppercase()));
    let rows = p.bindings.iter().map(|b| {
        let (set, binding, kind, name) = (b.set, b.binding, b.kind_tokens(), &b.name);
        quote! { (#set, #binding, #kind, #name) }
    });
    quote! {
        pub static #table: &[(u32, u32, BindingKind, &str)] = &[#(#rows),*];
    }
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().expect("driver crate has a parent").to_path_buf();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", repo.join("wyn").display());

    let mut table = String::from(
        "// generated by build.rs — embedded SPIR-V modules\n\
         pub static SHADER_MODULES: &[(&str, &[u8])] = &[\n",
    );
    let mut codegen = quote! { use crate::graph::BindingKind; };

    for (key, rel) in ROOTS {
        let src = repo.join(rel);
        let spv = out_dir.join(format!("{key}.spv"));
        let status = Command::new("wyn")
            .args(["compile"])
            .arg(&src)
            .arg("-o")
            .arg(&spv)
            .status()
            .unwrap_or_else(|e| panic!("failed to run `wyn compile` ({e}); is `wyn` on PATH?"));
        assert!(status.success(), "`wyn compile {}` failed", src.display());

        table.push_str(&format!(
            "    ({key:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{key}.spv\"))),\n"
        ));

        // Translate the descriptor `wyn compile` wrote next to the .spv.
        let json_path = out_dir.join(format!("{key}.json"));
        let json = std::fs::read_to_string(&json_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", json_path.display()));
        let desc: Descriptor = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("parse {}: {e}", json_path.display()));
        for p in &desc.pipelines {
            codegen.extend(codegen_pipeline(p));
            codegen.extend(codegen_bindings(p));
        }
    }

    table.push_str("];\n");
    std::fs::write(out_dir.join("shaders.rs"), table).expect("write shaders.rs");

    // Pretty-print the generated descriptor code so it's readable/reviewable.
    let file = syn::parse2::<syn::File>(codegen).expect("generated descriptor code parses");
    let pretty = prettyplease::unparse(&file);
    std::fs::write(out_dir.join("descriptor.rs"), pretty).expect("write descriptor.rs");
}
