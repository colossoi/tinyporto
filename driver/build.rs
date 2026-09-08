//! Build-time shader compilation + pipeline-descriptor codegen.
//!
//! For each Wyn root, runs `wyn build --graphics` (emitting SPIR-V + a `.json` pipeline
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

// Local package dependencies declared in the repository-root wyn.toml. Cargo
// cannot discover Wyn's import graph itself, so track their sources explicitly.
const WYN_PACKAGE_PATHS: &[&str] = &["../wyn/pkg/gtao", "../wyn/pkg/noise", "../wyn/pkg/rng"];

// ---- descriptor model (the subset of the wyn `*.json` we consume) ----

#[derive(serde::Deserialize)]
struct Descriptor {
    pipelines: Vec<Pipeline>,
    #[serde(default)]
    frame_graph: Option<FrameGraph>,
}

#[derive(serde::Deserialize)]
struct FrameGraph {
    #[serde(default)]
    passes: Vec<FrameGraphPass>,
    #[serde(default)]
    resources: Vec<FrameGraphResource>,
}

#[derive(serde::Deserialize)]
struct FrameGraphPass {
    name: String,
    kind: String,
    pipeline_index: usize,
    #[serde(default)]
    depends_on: Vec<usize>,
}

#[derive(serde::Deserialize)]
struct FrameGraphResource {
    name: String,
    #[serde(default)]
    bindings: Vec<FrameGraphBinding>,
}

#[derive(serde::Deserialize)]
struct FrameGraphBinding {
    name: String,
}

#[derive(serde::Deserialize)]
struct Pipeline {
    kind: String,
    #[serde(default)]
    bindings: Vec<Binding>,
    #[serde(default)]
    stages: Vec<Stage>,
    #[serde(default)]
    invocation: Option<Invocation>,
    #[serde(default)]
    fragment_outputs: Vec<FragmentOutput>,
}

#[derive(serde::Deserialize)]
struct FragmentOutput {
    location: u32,
    name: String,
}

#[derive(serde::Deserialize)]
struct Invocation {
    topology: String,
    draw: DrawInvocation,
    fragment_state: FragmentState,
}

#[derive(serde::Deserialize)]
struct FragmentState {
    depth_test: String,
    depth_write: bool,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DrawInvocation {
    Direct {
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    },
    Indirect {
        commands: IndirectCommands,
        offset: u64,
        draw_count: DrawCount,
    },
}

#[derive(serde::Deserialize)]
struct IndirectCommands {
    name: String,
}

#[derive(serde::Deserialize)]
struct DrawCount {
    kind: String,
    count: u32,
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

    /// Logical access for a physical stage. Storage-buffer layout permissions
    /// are subsequently taken from the compiled shader's declarations.
    fn stage_kind_tokens(&self, reads: bool, writes: bool) -> TokenStream {
        match self.ty.as_str() {
            "storage_buffer" => self.kind_tokens(),
            "storage_texture" => {
                let format = self.format_tokens();
                let access = match (reads, writes) {
                    (true, false) => quote! { ImgAccess::Read },
                    (false, true) => quote! { ImgAccess::Write },
                    (true, true) => quote! { ImgAccess::ReadWrite },
                    (false, false) => panic!("descriptor: unused stage storage texture"),
                };
                quote! { BindingKind::StorageImage { format: #format, access: #access } }
            }
            _ => self.kind_tokens(),
        }
    }

    fn usage_tokens(&self) -> TokenStream {
        match self.usage.as_deref() {
            Some("input") => quote! { BindingUsage::Input },
            Some("output") => quote! { BindingUsage::Output },
            Some("intermediate") => quote! { BindingUsage::Intermediate },
            _ => quote! { BindingUsage::Other },
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
    LikeInput {
        binding: u32,
        elem_bytes: u64,
        src_elem_bytes: u64,
    },
    /// One element per dispatched invocation (compiler scratch whose length tracks
    /// the pass's grid): (dispatch_elems) * elem_bytes, where dispatch_elems is the
    /// element count of the input binding this pass's dispatch derives from.
    SameAsDispatch { elem_bytes: u64 },
}

#[derive(serde::Deserialize)]
struct Stage {
    entry_point: String,
    owner: String,
    #[serde(default)]
    workgroup_size: [u32; 3],
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    dispatch_size: Option<DispatchSize>,
    /// Indices into the parent pipeline's `bindings` array read by this physical
    /// stage. Together with `writes`, this defines its exact bind interface.
    #[serde(default)]
    reads: Vec<usize>,
    /// Indices into the parent pipeline's `bindings` array that this stage writes;
    /// associates a `same_as_dispatch` output with the domain-derived stage that
    /// produces it. These are descriptor-array indices, not Vulkan binding numbers.
    #[serde(default)]
    writes: Vec<usize>,
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

/// The domain a `DerivedFrom` dispatch is sized from. A storage-buffer input
/// (`ceil(len_elems / wg)`), a storage image (`ceil(width*height / wg)`), or a
/// compile-time element count (`ceil(count / wg)`, an `iota` domain).
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Len {
    InputBinding { binding: u32, elem_bytes: u64 },
    StorageImage { set: u32, binding: u32 },
    Fixed { count: u64 },
}

fn id(s: &str) -> Ident {
    Ident::new(s, Span::call_site())
}

// Resolve `wyn` the way the shell will: scan PATH, honouring PATHEXT on Windows so
// `wyn` matches `wyn.exe`. Returns the first hit, or None if PATH has no match.
fn which_wyn() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    // On Windows a bare `wyn` resolves against PATHEXT; elsewhere the name is literal.
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE".into())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("wyn{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn rerun_if_wyn_changed(dir: &std::path::Path) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("read_dir entry").path();
        if path.is_dir() {
            rerun_if_wyn_changed(&path);
        } else if path.extension().is_some_and(|ext| ext == "wyn") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

/// The input binding a compute pipeline's dispatch grid derives from, as
/// (binding, src_elem_bytes) — the domain a `same_as_dispatch` output tracks. All
/// buffer-derived stages of a fused pipeline share one domain (e.g. `cull` runs
/// every filter/scan/gather stage over `bidx`); panics if they disagree.
fn dispatch_input(p: &Pipeline) -> (u32, u64) {
    let mut found: Option<(u32, u64)> = None;
    for s in &p.stages {
        if let Some(DispatchSize::DerivedFrom {
            len:
                Len::InputBinding {
                    binding,
                    elem_bytes,
                },
            ..
        }) = s.dispatch_size.as_ref()
        {
            match found {
                Some((b, _)) => assert_eq!(
                    b, *binding,
                    "pipeline dispatch derives from >1 input binding"
                ),
                None => found = Some((*binding, *elem_bytes)),
            }
        }
    }
    found.expect("same_as_dispatch needs a buffer-derived dispatch")
}

/// The dispatch domain of the stage that writes output `binding` — the domain a
/// `same_as_dispatch` output is sized by. With mixed domains in one pipeline
/// (several iota maps), each output must be sized from ITS producing stage, not
/// a pipeline-wide domain. Fixed{1,1,1} prelude stages list every output as a
/// write, so only domain-derived stages count; None falls back to the legacy
/// pipeline-wide `dispatch_input`.
fn output_domain<'a>(p: &'a Pipeline, output: &Binding) -> Option<&'a Len> {
    let mut found: Option<&Len> = None;
    for s in &p.stages {
        if let Some(DispatchSize::DerivedFrom { len, .. }) = s.dispatch_size.as_ref() {
            let writes_output = s.writes.iter().any(|&index| {
                let binding = p.bindings.get(index).unwrap_or_else(|| {
                    panic!(
                        "descriptor: stage {} write index {} is outside {} bindings",
                        s.entry_point,
                        index,
                        p.bindings.len()
                    )
                });
                binding.set == output.set && binding.binding == output.binding
            });
            if writes_output {
                assert!(
                    found.is_none(),
                    "output binding {}:{} written by more than one domain-derived stage",
                    output.set,
                    output.binding
                );
                found = Some(len);
            }
        }
    }
    found
}

/// The compiler-provided logical owner shared by every physical stage in one
/// compute pipeline. Generated helper names use this identity; they must not
/// infer ownership from lowered entry-point spelling.
fn pipeline_owner(p: &Pipeline) -> &str {
    let owner = p
        .stages
        .first()
        .map(|stage| stage.owner.as_str())
        .expect("pipeline has at least one stage");
    assert!(
        p.stages.iter().all(|stage| stage.owner == owner),
        "descriptor: pipeline stages don't share owner {owner:?}"
    );
    owner
}

/// Rust identifier for a pipeline's generated binding table. Compute owners are
/// unique descriptor entry identities. Graphics stages all belong to the root
/// frame entry, so their descriptor pipeline index disambiguates them.
fn binding_table_id(p: &Pipeline, pipeline_index: usize) -> Ident {
    if p.kind == "compute" {
        id(&format!("{}_BINDINGS", pipeline_owner(p).to_uppercase()))
    } else {
        id(&format!("PIPELINE_{pipeline_index}_BINDINGS"))
    }
}

fn stage_binding_table_id(p: &Pipeline, stage_index: usize) -> Ident {
    id(&format!(
        "{}_STAGE_{stage_index}_BINDINGS",
        pipeline_owner(p).to_uppercase()
    ))
}

/// Generate the exact interface used by one physical compute stage. `reads` and
/// `writes` are descriptor-array indices; duplicate descriptor rows for one
/// Vulkan slot are folded after their access has been combined.
fn codegen_stage_bindings(
    p: &Pipeline,
    stage: &Stage,
    stage_index: usize,
    interfaces: &BufferInterfaces,
) -> TokenStream {
    let table = stage_binding_table_id(p, stage_index);
    let mut used = vec![false; p.bindings.len()];
    for &binding_index in stage.reads.iter().chain(&stage.writes) {
        *used.get_mut(binding_index).unwrap_or_else(|| {
            panic!(
                "descriptor: stage {} binding index {} is outside {} bindings",
                stage.entry_point,
                binding_index,
                p.bindings.len()
            )
        }) = true;
    }

    let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    let rows: Vec<TokenStream> = p
        .bindings
        .iter()
        .enumerate()
        .filter(|(binding_index, b)| used[*binding_index] && seen.insert((b.set, b.binding)))
        .map(|(_, b)| {
            let same_slot = |binding_index: usize| {
                let other = &p.bindings[binding_index];
                other.set == b.set && other.binding == b.binding
            };
            let reads = stage.reads.iter().copied().any(same_slot);
            let writes = stage.writes.iter().copied().any(same_slot);
            let (set, binding, name) = (b.set, b.binding, &b.name);
            let kind = if let Some(writable) =
                interfaces.get(&(stage.entry_point.clone(), b.set, b.binding))
            {
                if *writable {
                    quote! { BindingKind::StorageReadWrite }
                } else {
                    quote! { BindingKind::StorageRead }
                }
            } else {
                // Unused/optimized-out buffers have no shader requirement;
                // retain their descriptor declaration and non-buffer handling.
                b.stage_kind_tokens(reads, writes)
            };
            let usage = b.usage_tokens();
            quote! { (#set, #binding, #kind, #usage, #name) }
        })
        .collect();
    quote! {
        pub static #table: &[(u32, u32, BindingKind, BindingUsage, &str)] = &[#(#rows),*];
    }
}

/// Temporary compatibility bridge for Tinyporto's two runtime `iota` domains.
/// The compiler currently emits these expression-derived domains as implicit
/// fixed dispatches. The host already owns the window extent, so use its two
/// products until the descriptor preserves parameter expressions directly.
fn temporary_host_count_param(p: &Pipeline, stage_index: usize) -> Option<&'static str> {
    if pipeline_owner(p) == "tinyporto_frame__compute_2" {
        match stage_index {
            0 => Some("window_pixels"),
            1 => Some("occ_pixels"),
            _ => None,
        }
    } else {
        None
    }
}

fn temporary_output_count_param(p: &Pipeline, output: &Binding) -> Option<&'static str> {
    let binding_index = p.bindings.iter().position(|binding| {
        binding.set == output.set
            && binding.binding == output.binding
            && binding.name == output.name
    })?;
    p.stages
        .iter()
        .enumerate()
        .find(|(_, stage)| stage.writes.contains(&binding_index))
        .and_then(|(stage_index, _)| temporary_host_count_param(p, stage_index))
}

/// Fixed byte capacity of `intermediate` binding `b`, if the pipeline has one.
/// A dispatch (or `same_as_dispatch` output) sized from an intermediate — e.g. a
/// `filter`'s gather buffer feeding the map over its survivors — covers the full
/// capacity; the kernel bounds the live prefix itself via the count buffer.
fn intermediate_fixed_bytes(p: &Pipeline, b: u32) -> Option<u64> {
    p.bindings
        .iter()
        .find(|x| x.usage.as_deref() == Some("intermediate") && x.binding == b)
        .map(|x| match x.length.as_ref() {
            Some(Length::Fixed { bytes }) => *bytes,
            _ => panic!("descriptor: intermediate binding {b} has no fixed length"),
        })
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

/// Name (`<name>_pixels`) of the pixel-count parameter for the storage-image
/// binding at (`set`, `binding`) — the domain an image-derived dispatch sizes from.
fn image_pixels_param(p: &Pipeline, set: u32, b: u32) -> Ident {
    let name = p
        .bindings
        .iter()
        .find(|x| x.ty == "storage_texture" && x.set == set && x.binding == b)
        .map(|x| x.name.as_str())
        .unwrap_or_else(|| panic!("descriptor: no storage_texture at set {set} binding {b}"));
    id(&format!("{name}_pixels"))
}

// These two leading arguments form the ComputePass ABI. A BTreeSet sorts
// occ_pixels before window_pixels, silently reversing both dispatch and capacity.
fn ordered_runtime_params(params: &std::collections::BTreeSet<String>) -> Vec<Ident> {
    ["window_pixels", "occ_pixels"]
        .into_iter()
        .chain(
            params.iter().map(String::as_str)
                .filter(|name| *name != "window_pixels" && *name != "occ_pixels"),
        )
        .map(id)
        .collect()
}

/// Translate one compute pipeline into `<entry>_stages` + `<entry>_out_bytes`
/// functions, with the descriptor's rules inlined as arithmetic. Non-compute
/// pipelines have nothing to compute, so they generate nothing. A compute entry
/// lowers to several ordered stages (one per output domain); the canonical entry
/// (see `pipeline_owner`) names the whole pipeline.
fn codegen_pipeline(p: &Pipeline, interfaces: &BufferInterfaces) -> TokenStream {
    if p.kind != "compute" {
        return quote! {};
    }
    let entry0 = pipeline_owner(p);
    let stages_fn = id(&format!("{entry0}_stages"));
    let count_const = id(&format!("{}_STAGE_COUNT", entry0.to_uppercase()));
    let out_bytes_fn = id(&format!("{entry0}_out_bytes"));
    let n_stages = p.stages.len();
    let stage_binding_defs: Vec<TokenStream> = p
        .stages
        .iter()
        .enumerate()
        .map(|(stage_index, stage)| codegen_stage_bindings(p, stage, stage_index, interfaces))
        .collect();

    // One `ComputeStage { entry, groups }` per descriptor stage. Each stage's
    // dispatch dims are either a constant grid (the entry indexes its whole output
    // directly) or ceil(input_len_elems / workgroup_size). The stages_fn takes the
    // byte size of every input a derived stage sizes from (sorted union), so a
    // fixed-grid stage needs no argument.
    let mut disp_params: std::collections::BTreeSet<String> =
        ["window_pixels".to_string(), "occ_pixels".to_string()]
            .into_iter()
            .collect();
    let stage_rows: Vec<TokenStream> = p
        .stages
        .iter()
        .enumerate()
        .map(|(stage_index, s)| {
            let entry = s.entry_point.as_str();
            let bindings = stage_binding_table_id(p, stage_index);
            let ds = s
                .dispatch_size
                .as_ref()
                .expect("compute stage has dispatch_size");
            let dims = if let Some(param) = temporary_host_count_param(p, stage_index) {
                assert_eq!(s.workgroup_size[1..], [1, 1]);
                let count = id(param);
                let workgroup_size = s.workgroup_size[0];
                quote! { [(#count as u32).div_ceil(#workgroup_size), 1, 1] }
            } else {
                match ds {
                    DispatchSize::Fixed { x, y, z } => quote! { [#x, #y, #z] },
                    DispatchSize::DerivedFrom {
                        len,
                        workgroup_size,
                    } => {
                        let wg = *workgroup_size;
                        match len {
                            // Buffer-sized: a fixed-capacity intermediate is a constant
                            // grid; an entry input arrives as a `<name>_bytes` arg.
                            Len::InputBinding {
                                binding,
                                elem_bytes,
                            } => {
                                if let Some(bytes) = intermediate_fixed_bytes(p, *binding) {
                                    let count = bytes / elem_bytes;
                                    quote! { [(#count as u32).div_ceil(#wg), 1, 1] }
                                } else {
                                    let param = input_param(p, *binding);
                                    disp_params.insert(param.to_string());
                                    quote! { [((#param / #elem_bytes) as u32).div_ceil(#wg), 1, 1] }
                                }
                            }
                            // Storage image: dispatch ceil(width*height / wg). The pixel
                            // count arrives as a `<name>_pixels` arg (see image_pixels_param).
                            Len::StorageImage { set, binding } => {
                                let param = image_pixels_param(p, *set, *binding);
                                disp_params.insert(param.to_string());
                                quote! { [(#param as u32).div_ceil(#wg), 1, 1] }
                            }
                            // Compile-time count (an iota domain): a constant grid,
                            // no argument needed.
                            Len::Fixed { count } => {
                                quote! { [(#count as u32).div_ceil(#wg), 1, 1] }
                            }
                        }
                    }
                }
            };
            quote! {
                crate::graph::ComputeStage {
                    entry: #entry,
                    groups: #dims,
                    bindings: #bindings,
                }
            }
        })
        .collect();
    let disp_param_ids = ordered_runtime_params(&disp_params);

    // Sized writes: one match arm per binding the pass writes — entry outputs AND
    // compiler-internal `intermediate` scratch (e.g. a filter's compacted-count
    // buffer). The driver sizes both from these formulas.
    let outputs: Vec<&Binding> = p
        .bindings
        .iter()
        .filter(|b| matches!(b.usage.as_deref(), Some("output") | Some("intermediate")))
        .collect();
    let mut params: std::collections::BTreeSet<String> =
        ["window_pixels".to_string(), "occ_pixels".to_string()]
            .into_iter()
            .collect();
    let arms: Vec<TokenStream> = outputs
        .iter()
        .map(|o| {
            let b = o.binding;
            let expr = match o.length.as_ref().expect("output binding has length") {
                Length::Fixed { bytes } => quote! { #bytes },
                Length::LikeInput {
                    binding,
                    elem_bytes,
                    src_elem_bytes,
                } => {
                    let src = input_param(p, *binding);
                    params.insert(src.to_string());
                    quote! { (#src / #src_elem_bytes) * #elem_bytes }
                }
                Length::SameAsDispatch { elem_bytes } => {
                    if let Some(param) = temporary_output_count_param(p, o) {
                        let count = id(param);
                        quote! { #count * #elem_bytes }
                    } else {
                        match output_domain(p, o) {
                            Some(Len::Fixed { count }) => quote! { #count * #elem_bytes },
                            Some(Len::InputBinding {
                                binding,
                                elem_bytes: src_elem_bytes,
                            }) => {
                                if let Some(bytes) = intermediate_fixed_bytes(p, *binding) {
                                    let count = bytes / src_elem_bytes;
                                    quote! { #count * #elem_bytes }
                                } else {
                                    let src = input_param(p, *binding);
                                    params.insert(src.to_string());
                                    quote! { (#src / #src_elem_bytes) * #elem_bytes }
                                }
                            }
                            Some(Len::StorageImage { set, binding }) => {
                                let src = image_pixels_param(p, *set, *binding);
                                params.insert(src.to_string());
                                quote! { #src * #elem_bytes }
                            }
                            None => {
                                let (binding, src_elem_bytes) = dispatch_input(p);
                                let src = input_param(p, binding);
                                params.insert(src.to_string());
                                quote! { (#src / #src_elem_bytes) * #elem_bytes }
                            }
                        }
                    }
                }
            };
            quote! { #b => #expr }
        })
        .collect();
    let out_params = ordered_runtime_params(&params);

    quote! {
        #(#stage_binding_defs)*
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

/// Generate the bind-table static for a pipeline's entry: the descriptor's
/// pipeline-wide interface shared by its ordered physical stages.
fn codegen_bindings(p: &Pipeline, pipeline_index: usize) -> TokenStream {
    if p.stages.is_empty() {
        return quote! {};
    }
    let table = binding_table_id(p, pipeline_index);
    // Dedup by (set, binding): the descriptor lists a storage-image resource once
    // per view kind, so the same slot can appear twice — one layout entry per slot.
    let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    let rows: Vec<TokenStream> = p
        .bindings
        .iter()
        .filter(|b| seen.insert((b.set, b.binding)))
        .map(|b| {
            let (set, binding, kind, name) = (b.set, b.binding, b.kind_tokens(), &b.name);
            let usage = b.usage_tokens();
            quote! { (#set, #binding, #kind, #usage, #name) }
        })
        .collect();
    quote! {
        pub static #table: &[(u32, u32, BindingKind, BindingUsage, &str)] = &[#(#rows),*];
    }
}

/// Emit one complete graphics item from the unified pipeline descriptor. Stage
/// entry points, binding interface, draw mode/counts, and depth behavior are all
/// compiler-owned; the application graph only groups attachments into passes.
fn codegen_graphics_item(key: &str, p: &Pipeline, pipeline_index: usize) -> TokenStream {
    if p.kind != "graphics" {
        return quote! {};
    }
    let item = id(&format!("PIPELINE_{pipeline_index}_ITEM"));
    let bindings = binding_table_id(p, pipeline_index);
    let vertex = p
        .stages
        .iter()
        .find(|stage| stage.stage.as_deref() == Some("vertex"))
        .map(|stage| stage.entry_point.as_str())
        .expect("graphics pipeline has a vertex stage");
    let fragment = p
        .stages
        .iter()
        .find(|stage| stage.stage.as_deref() == Some("fragment"))
        .map(|stage| stage.entry_point.as_str())
        .expect("graphics pipeline has a fragment stage");
    let invocation = p.invocation.as_ref().expect("graphics invocation");
    assert_eq!(invocation.topology, "triangle_list");
    let draw = match &invocation.draw {
        DrawInvocation::Direct {
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        } => quote! {
            crate::graph::Draw::Direct {
                vertex_count: #vertex_count,
                instance_count: #instance_count,
                first_vertex: #first_vertex,
                first_instance: #first_instance,
            }
        },
        DrawInvocation::Indirect {
            commands,
            offset,
            draw_count,
        } => {
            assert_eq!(draw_count.kind, "fixed");
            assert_eq!(draw_count.count, 1, "multi-draw is not supported yet");
            let name = &commands.name;
            quote! { crate::graph::Draw::Indirect { commands: #name, offset: #offset } }
        }
    };
    let depth_write = invocation.fragment_state.depth_write;
    let depth_test = match invocation.fragment_state.depth_test.as_str() {
        "disabled" => quote! { crate::graph::DepthTest::Disabled },
        "less_equal" => quote! { crate::graph::DepthTest::LessEqual },
        other => panic!("descriptor: unsupported depth test {other:?}"),
    };
    let label = format!("{key}:pipeline_{pipeline_index}");
    quote! {
        pub static #item: crate::graph::RenderItem = crate::graph::RenderItem {
            label: #label,
            module: #key,
            vs: #vertex,
            fs: #fragment,
            bindings: #bindings,
            draw: #draw,
            depth_test: #depth_test,
            depth_write: #depth_write,
        };
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

fn prerequisite_pipelines(fg: &FrameGraph, target: usize) -> Vec<usize> {
    fn visit(
        fg: &FrameGraph,
        pass_index: usize,
        seen_passes: &mut std::collections::HashSet<usize>,
        seen_pipelines: &mut std::collections::HashSet<usize>,
        pipelines: &mut Vec<usize>,
    ) {
        if !seen_passes.insert(pass_index) {
            return;
        }
        let pass = fg
            .passes
            .get(pass_index)
            .unwrap_or_else(|| panic!("descriptor: dependency pass {pass_index} is out of range"));
        for &dependency in &pass.depends_on {
            visit(fg, dependency, seen_passes, seen_pipelines, pipelines);
            let dependency_pass = &fg.passes[dependency];
            if dependency_pass.kind == "compute"
                && seen_pipelines.insert(dependency_pass.pipeline_index)
            {
                pipelines.push(dependency_pass.pipeline_index);
            }
        }
    }

    let mut pipelines = Vec::new();
    visit(
        fg,
        target,
        &mut std::collections::HashSet::new(),
        &mut std::collections::HashSet::new(),
        &mut pipelines,
    );
    pipelines
}

/// Compiler-inserted prerequisites currently have closed, fixed-size domains.
/// Those can be materialized as an opaque `ComputePass` without any app-provided
/// dispatch or sizing arguments.
fn has_fixed_compute_factory(p: &Pipeline) -> bool {
    p.kind == "compute"
        && p.stages
            .iter()
            .all(|stage| matches!(stage.dispatch_size, Some(DispatchSize::Fixed { .. })))
        && p.bindings
            .iter()
            .filter(|binding| {
                matches!(
                    binding.usage.as_deref(),
                    Some("output") | Some("intermediate")
                )
            })
            .all(|binding| matches!(binding.length, Some(Length::Fixed { .. })))
}

fn compute_stage_args(p: &Pipeline) -> Option<Vec<TokenStream>> {
    let mut params = std::collections::BTreeSet::new();
    for stage in &p.stages {
        let Some(DispatchSize::DerivedFrom { len, .. }) = stage.dispatch_size.as_ref() else {
            continue;
        };
        match len {
            Len::Fixed { .. } => {}
            Len::InputBinding { binding, .. } => {
                intermediate_fixed_bytes(p, *binding)?;
            }
            Len::StorageImage { .. } => {
                params.insert("window_pixels".to_string());
            }
        }
    }
    Some(
        params
            .into_iter()
            .map(|param| {
                let ident = id(&param);
                quote! { #ident }
            })
            .collect(),
    )
}

fn codegen_frame_graph(key: &str, desc: &Descriptor) -> TokenStream {
    let Some(fg) = desc.frame_graph.as_ref() else {
        return quote! {};
    };

    let rows = fg.passes.iter().map(|p| {
        let name = &p.name;
        let kind = &p.kind;
        quote! { crate::graph::DescriptorPassInfo { module: #key, name: #name, kind: #kind } }
    });
    let pipeline_index_arms = fg.passes.iter().map(|p| {
        let name = &p.name;
        let pipeline_index = p.pipeline_index;
        quote! { (#key, #name) => Some(#pipeline_index) }
    });
    let prerequisite_arms = fg.passes.iter().enumerate().map(|(pass_index, p)| {
        let name = &p.name;
        let pipelines = prerequisite_pipelines(fg, pass_index);
        quote! { (#key, #name) => &[#(#pipelines),*] }
    });

    let compute_factory_arms = desc
        .pipelines
        .iter()
        .enumerate()
        .filter(|(_, pipeline)| has_fixed_compute_factory(pipeline))
        .map(|(pipeline_index, pipeline)| {
            let entry = pipeline_owner(pipeline);
            let bindings = binding_table_id(pipeline, pipeline_index);
            let stages = id(&format!("{entry}_stages"));
            let out_bytes = id(&format!("{entry}_out_bytes"));
            quote! {
                (#key, #pipeline_index) => Some(crate::graph::ComputePass {
                    label: #entry,
                    module: #key,
                    bindings: #bindings,
                    stages: #stages(0, 0).to_vec(),
                    out_bytes: #out_bytes,
                    runtime_counts: [0, 0],
                })
            }
        });

    let compute_entry_arms = desc
        .pipelines
        .iter()
        .enumerate()
        .filter(|(_, pipeline)| pipeline.kind == "compute")
        .filter_map(|(pipeline_index, pipeline)| {
            let entry = pipeline_owner(pipeline);
            let args = compute_stage_args(pipeline)?;
            let bindings = binding_table_id(pipeline, pipeline_index);
            let stages = id(&format!("{entry}_stages"));
            let out_bytes = id(&format!("{entry}_out_bytes"));
            Some(quote! {
                (#key, #entry) => Some(crate::graph::ComputePass {
                    label: #entry,
                    module: #key,
                    bindings: #bindings,
                    stages: #stages(window_pixels, occ_pixels, #(#args),*).to_vec(),
                    out_bytes: #out_bytes,
                    runtime_counts: [window_pixels, occ_pixels],
                })
            })
        });

    let mut resource_arms = Vec::new();
    let mut emitted_binding_names = std::collections::BTreeSet::new();
    for resource in &fg.resources {
        let resource_name = &resource.name;
        let binding_names: Vec<&str> = resource
            .bindings
            .iter()
            .map(|binding| binding.name.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for binding_name in &binding_names {
            assert!(
                emitted_binding_names.insert(*binding_name),
                "descriptor: binding name {binding_name:?} belongs to multiple frame resources"
            );
            resource_arms.push(quote! {
                (#key, #binding_name) => Some(crate::graph::DescriptorResourceInfo {
                    name: #resource_name,
                    binding_names: &[#(#binding_names),*],
                })
            });
        }
    }

    quote! {
        /// Physical stages from the descriptor frame_graph, preserving descriptor
        /// stage names and kinds for graph validation/diagnostics.
        pub static DESCRIPTOR_PASSES: &[crate::graph::DescriptorPassInfo] = &[#(#rows),*];

        fn descriptor_pipeline_index(module: &str, entry: &str) -> Option<usize> {
            match (module, entry) {
                #(#pipeline_index_arms,)*
                _ => None,
            }
        }

        fn descriptor_prerequisite_pipelines(module: &str, entry: &str) -> &'static [usize] {
            match (module, entry) {
                #(#prerequisite_arms,)*
                _ => &[],
            }
        }

        fn descriptor_compute_pass(
            module: &str,
            pipeline_index: usize,
        ) -> Option<crate::graph::ComputePass> {
            match (module, pipeline_index) {
                #(#compute_factory_arms,)*
                _ => None,
            }
        }

        pub fn insert_descriptor_prerequisites(graph: &mut crate::graph::Graph) {
            graph.insert_compute_prerequisites(
                descriptor_pipeline_index,
                descriptor_prerequisite_pipelines,
                descriptor_compute_pass,
            );
        }

        pub fn descriptor_compute_entry(
            module: &'static str,
            entry: &'static str,
            window_pixels: u64,
            occ_pixels: u64,
        ) -> Option<crate::graph::ComputePass> {
            match (module, entry) {
                #(#compute_entry_arms,)*
                _ => None,
            }
        }

        pub fn descriptor_resource(
            module: &str,
            binding_name: &str,
        ) -> Option<crate::graph::DescriptorResourceInfo> {
            match (module, binding_name) {
                #(#resource_arms,)*
                _ => None,
            }
        }
    }
}

type BufferInterfaces = std::collections::HashMap<(String, u32, u32), bool>;

/// Match the same SPIR-V declarations that wgpu validates. Descriptor stage
/// reads/writes describe operations, while pipeline-wide access unions can be
/// too broad for a stage-specific readonly global. Neither is a layout contract.
fn buffer_interfaces(bytes: &[u8]) -> BufferInterfaces {
    let module = naga::front::spv::parse_u8_slice(bytes, &Default::default())
        .expect("parse compiled SPIR-V");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("validate compiled SPIR-V");
    let mut interfaces = BufferInterfaces::new();
    for (index, entry) in module.entry_points.iter().enumerate() {
        let uses = info.get_entry_point(index);
        for (handle, global) in module.global_variables.iter() {
            if uses[handle].is_empty() {
                continue;
            }
            if let (Some(binding), naga::AddressSpace::Storage { access }) =
                (&global.binding, global.space)
            {
                let key = (entry.name.clone(), binding.group, binding.binding);
                let writable = access.contains(naga::StorageAccess::STORE);
                if let Some(previous) = interfaces.insert(key, writable) {
                    assert_eq!(previous, writable, "conflicting SPIR-V buffer declarations");
                }
            }
        }
    }
    interfaces
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest
        .parent()
        .expect("driver crate has a parent")
        .to_path_buf();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", repo.join("wyn.toml").display());
    rerun_if_wyn_changed(&repo.join("wyn"));
    for rel in WYN_PACKAGE_PATHS {
        let package = repo.join(rel);
        println!(
            "cargo:rerun-if-changed={}",
            package.join("wyn.toml").display()
        );
        rerun_if_wyn_changed(&package.join("src"));
    }

    // Recompile when the compiler itself changes (reinstalled from a new HEAD), not
    // only when a `.wyn` source does — otherwise a fresh `wyn` links stale SPIR-V.
    // Track the resolved binary's mtime, and PATH so swapping which `wyn` is found
    // also counts. Fall back to the bare name if resolution fails.
    let wyn = which_wyn().unwrap_or_else(|| PathBuf::from("wyn"));
    println!("cargo:rerun-if-changed={}", wyn.display());
    println!("cargo:rerun-if-env-changed=PATH");

    // One codegen path (quote). Each root contributes its embedded-SPIR-V row and
    // its descriptor translation; everything is emitted into a single file.
    let mut shader_rows: Vec<TokenStream> = Vec::new();
    let mut codegen =
        quote! { use crate::graph::{BindingKind, BindingUsage, ImgAccess, TexFormat}; };

    for (key, rel) in ROOTS {
        let src = repo.join(rel);
        let spv = out_dir.join(format!("{key}.spv"));
        let status = Command::new(&wyn)
            .args(["build", "--graphics"])
            .arg(&src)
            .arg("-o")
            .arg(&spv)
            .status()
            .unwrap_or_else(|e| panic!("failed to run `wyn build` ({e}); is `wyn` on PATH?"));
        assert!(
            status.success(),
            "`wyn build --graphics {}` failed",
            src.display()
        );

        let spv_rel = format!("/{key}.spv");
        shader_rows.push(quote! { (#key, include_bytes!(concat!(env!("OUT_DIR"), #spv_rel))) });

        // Translate the descriptor `wyn build` wrote next to the .spv.
        let json_path = out_dir.join(format!("{key}.json"));
        let json = std::fs::read_to_string(&json_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", json_path.display()));
        let desc: Descriptor = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("parse {}: {e}", json_path.display()));
        let interfaces = buffer_interfaces(&std::fs::read(&spv).expect("read compiled SPIR-V"));
        for (pipeline_index, p) in desc.pipelines.iter().enumerate() {
            codegen.extend(codegen_pipeline(p, &interfaces));
            codegen.extend(codegen_bindings(p, pipeline_index));
            codegen.extend(codegen_graphics_item(key, p, pipeline_index));
        }
        codegen.extend(codegen_uniform_blocks(&desc.pipelines));
        codegen.extend(codegen_frame_graph(key, &desc));
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
