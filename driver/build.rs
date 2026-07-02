//! Build-time shader compilation + pipeline-descriptor codegen.
//!
//! For each Wyn root, runs `wyn compile` (emitting SPIR-V + a `.json` pipeline
//! descriptor into OUT_DIR) and emits one `generated.rs` via `quote`: the
//! embedded-SPIR-V table, the dispatch/output-size rules specialized into inlined
//! `const fn` formulas (`codegen_pipeline`), and each pipeline's binding interface
//! (`codegen_bindings`). The calculations are code, not data the driver walks.

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
    /// Pixel format for `storage_texture` bindings (e.g. "rgba32_float").
    #[serde(default)]
    format: Option<String>,
    /// `size` means different things per binding type — a std140 byte count (u32)
    /// for a `uniform` block, but a `{kind,width,height}` extent for a
    /// `storage_texture`. Keep it opaque; only the uniform path reads it (as u64).
    #[serde(default)]
    size: Option<serde_json::Value>,
    /// Flattened members of a record-typed `uniform` block (empty for scalars).
    #[serde(default)]
    members: Vec<UniformMember>,
}

/// One member of a uniform block: `name` at `offset`, `size` bytes (std140).
#[derive(serde::Deserialize)]
struct UniformMember {
    name: String,
    offset: u32,
    size: u32,
}

impl Binding {
    /// The `BindingKind` token for this binding (buffer / texture / sampler /
    /// storage-image). Texture/sampler/storage_texture come from `#[texture]`,
    /// `#[sampler]`, and `storage_image` views.
    fn kind_tokens(&self) -> TokenStream {
        match (self.ty.as_str(), self.access.as_deref()) {
            ("uniform", _) => quote! { BindingKind::Uniform },
            ("storage_buffer", Some("write_only")) => quote! { BindingKind::StorageWrite },
            ("storage_buffer", Some("read_write")) => quote! { BindingKind::StorageReadWrite },
            ("storage_buffer", _) => quote! { BindingKind::StorageRead },
            ("texture", _) => quote! { BindingKind::Texture },
            ("sampler", _) => quote! { BindingKind::Sampler },
            ("storage_texture", acc) => {
                let format = self.format_tokens();
                let access = match acc {
                    Some("read_only") => quote! { ImgAccess::Read },
                    Some("write_only") => quote! { ImgAccess::Write },
                    Some("read_write") => quote! { ImgAccess::ReadWrite },
                    other => panic!("descriptor: storage_texture access {other:?}"),
                };
                quote! { BindingKind::StorageImage { format: #format, access: #access } }
            }
            (other, _) => panic!("descriptor: unhandled binding type {other:?}"),
        }
    }

    /// The `TexFormat` token for a `storage_texture`'s `format` field.
    fn format_tokens(&self) -> TokenStream {
        match self.format.as_deref() {
            Some("rgba8_unorm") => quote! { TexFormat::Rgba8Unorm },
            Some("rgba16_float") => quote! { TexFormat::Rgba16Float },
            Some("rgba32_float") => quote! { TexFormat::Rgba32Float },
            Some("r32_float") => quote! { TexFormat::R32Float },
            other => panic!("descriptor: storage_texture format {other:?}"),
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
#[serde(tag = "kind", rename_all = "snake_case")]
enum DispatchSize {
    /// A constant grid (the entry grid-strides internally, e.g. a multi-domain
    /// `step`): dispatch exactly `x*y*z` workgroups regardless of input size.
    Fixed { x: u32, y: u32, z: u32 },
    /// Sized from an input binding: ceil(input_len_elems / workgroup_size).
    DerivedFrom { len: Len, workgroup_size: u32 },
}

#[derive(serde::Deserialize)]
struct Len {
    binding: u32,
    elem_bytes: u64,
}

fn id(s: &str) -> Ident {
    Ident::new(s, Span::call_site())
}

/// The pipeline's canonical entry name — the stage every stage name is prefixed
/// by (the source entry). Its lowered stages are named `<entry>`, `<entry>_…`, so
/// the base is the shortest stage name; it can appear anywhere in the ordered list
/// (e.g. `step` runs first, but a fused `filter`'s primary `cull` runs last after
/// its `cull_filter_flags`/`cull_filter_scan` helpers). Names all generated items.
fn base_entry(p: &Pipeline) -> &str {
    let base = p
        .stages
        .iter()
        .map(|s| s.entry_point.as_str())
        .min_by_key(|n| n.len())
        .expect("compute pipeline has at least one stage");
    assert!(
        p.stages.iter().all(|s| s.entry_point.starts_with(base)),
        "descriptor: pipeline stages don't share the base entry {base:?}"
    );
    base
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

/// Translate one compute pipeline into `<entry>_stages` + `<entry>_out_bytes`
/// functions, with the descriptor's rules inlined as arithmetic. Non-compute
/// pipelines have nothing to compute, so they generate nothing. A compute entry
/// lowers to several ordered stages (one per output domain); the canonical entry
/// (see `base_entry`) names the whole pipeline.
fn codegen_pipeline(p: &Pipeline) -> TokenStream {
    if p.kind != "compute" {
        return quote! {};
    }
    let entry0 = base_entry(p);
    let stages_fn = id(&format!("{entry0}_stages"));
    let count_const = id(&format!("{}_STAGE_COUNT", entry0.to_uppercase()));
    let out_bytes_fn = id(&format!("{entry0}_out_bytes"));
    let n_stages = p.stages.len();

    // One `ComputeStage { entry, groups }` per descriptor stage. Each stage's
    // dispatch dims are either a constant grid (the entry indexes its whole output
    // directly) or ceil(input_len_elems / workgroup_size). The stages_fn takes the
    // byte size of every input a derived stage sizes from (sorted union), so a
    // fixed-grid stage needs no argument.
    let mut disp_params: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let stage_rows: Vec<TokenStream> = p
        .stages
        .iter()
        .map(|s| {
            let entry = s.entry_point.as_str();
            let ds = s.dispatch_size.as_ref().expect("compute stage has dispatch_size");
            let dims = match ds {
                DispatchSize::Fixed { x, y, z } => quote! { [#x, #y, #z] },
                DispatchSize::DerivedFrom { len, workgroup_size } => {
                    let param = input_param(p, len.binding);
                    disp_params.insert(param.to_string());
                    let (elem, wg) = (len.elem_bytes, workgroup_size);
                    quote! { [((#param / #elem) as u32).div_ceil(#wg), 1, 1] }
                }
            };
            quote! { crate::graph::ComputeStage { entry: #entry, groups: #dims } }
        })
        .collect();
    let disp_param_ids: Vec<Ident> = disp_params.iter().map(|s| id(s)).collect();

    // Sized writes: one match arm per binding the pass writes — entry outputs AND
    // compiler-internal `intermediate` scratch (e.g. a filter's compacted-count
    // buffer). The driver sizes both from these formulas.
    let outputs: Vec<&Binding> = p
        .bindings
        .iter()
        .filter(|b| matches!(b.usage.as_deref(), Some("output") | Some("intermediate")))
        .collect();
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
        /// Number of ordered compute stages this entry lowers to.
        pub const #count_const: usize = #n_stages;
        /// Ordered compute stages (entry point + workgroup dispatch dims) for this
        /// pipeline, each dispatch sized per the descriptor's `dispatch_size`.
        pub const fn #stages_fn(#(#disp_param_ids: u64),*) -> [crate::graph::ComputeStage; #n_stages] {
            [#(#stage_rows),*]
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
/// kind, name) tuples the driver maps to its resources — the descriptor's
/// declared binding interface.
fn codegen_bindings(p: &Pipeline) -> TokenStream {
    if p.stages.is_empty() {
        return quote! {};
    }
    let table = id(&format!("{}_BINDINGS", base_entry(p).to_uppercase()));
    // Dedup by (set, binding): the descriptor lists a storage-image resource once
    // per view kind, so the same slot can appear twice — one layout entry per slot.
    let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    let rows: Vec<TokenStream> = p
        .bindings
        .iter()
        .filter(|b| seen.insert((b.set, b.binding)))
        .map(|b| {
            let (set, binding, kind, name) = (b.set, b.binding, b.kind_tokens(), &b.name);
            quote! { (#set, #binding, #kind, #name) }
        })
        .collect();
    quote! {
        pub static #table: &[(u32, u32, BindingKind, &str)] = &[#(#rows),*];
    }
}

/// Emit the `UNIFORM_BLOCKS` table: every record-typed uniform block across all
/// pipelines (deduped by name), each with its std140 size and (field, offset,
/// size) members. The driver packs its `frame_globals` fill against this.
fn codegen_uniform_blocks(pipelines: &[Pipeline]) -> TokenStream {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut rows: Vec<TokenStream> = Vec::new();
    for p in pipelines {
        for b in &p.bindings {
            if b.ty != "uniform" || b.members.is_empty() || !seen.insert(b.name.clone()) {
                continue;
            }
            let name = &b.name;
            let size = b.size.as_ref().and_then(|v| v.as_u64()).unwrap_or(0);
            let members = b.members.iter().map(|m| {
                let (mn, off, sz) = (&m.name, m.offset, m.size);
                quote! { (#mn, #off, #sz) }
            });
            rows.push(quote! {
                crate::graph::UniformBlockLayout {
                    name: #name,
                    size: #size,
                    members: &[#(#members),*],
                }
            });
        }
    }
    quote! {
        /// std140 layouts of the record-typed uniform blocks, from the descriptor.
        pub static UNIFORM_BLOCKS: &[crate::graph::UniformBlockLayout] = &[#(#rows),*];
    }
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().expect("driver crate has a parent").to_path_buf();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", repo.join("wyn").display());

    // One codegen path (quote). Each root contributes its embedded-SPIR-V row and
    // its descriptor translation; everything is emitted into a single file.
    let mut shader_rows: Vec<TokenStream> = Vec::new();
    let mut codegen = quote! { use crate::graph::{BindingKind, ImgAccess, TexFormat}; };

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

        let spv_rel = format!("/{key}.spv");
        shader_rows.push(quote! { (#key, include_bytes!(concat!(env!("OUT_DIR"), #spv_rel))) });

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
        codegen.extend(codegen_uniform_blocks(&desc.pipelines));
    }

    let generated = quote! {
        #codegen
        /// Embedded SPIR-V modules, by source key.
        pub static SHADER_MODULES: &[(&str, &[u8])] = &[#(#shader_rows),*];
    };
    let file = syn::parse2::<syn::File>(generated).expect("generated code parses");
    let pretty = prettyplease::unparse(&file);
    std::fs::write(out_dir.join("generated.rs"), pretty).expect("write generated.rs");
}
