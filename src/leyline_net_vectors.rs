//! leyline-net/v1 conformance-vector drift gate (bead `rosary-086973`).
//!
//! `schemas/cloister.capnp` is vendored from ley-line-open's canonical
//! `net.capnp` (LLO commit 78638f3, bead `ley-line-open-083344`). This
//! test proves rosary's vendored copy stays byte-identical to LLO's
//! pinned vectors — it can no longer silently drift — by exercising both
//! directions with rosary's own generated `cloister_capnp` bindings
//! (capnp 0.21):
//!
//! 1. **Encode → byte-equality.** Each of the 12 values is rebuilt with
//!    the generated builders and asserted byte-equal to BOTH committed
//!    byte-forms under `schemas/leyline-net/v1/test-vectors/`
//!    (`reference/` = plain `write_message`; `canonical/` =
//!    `set_root_canonical`). Any field/ordinal/type/layout change in the
//!    vendored schema changes the produced bytes and fails here. That the
//!    bytes match at all also proves capnp 0.21 (rosary) and 0.25.0 (the
//!    version LLO pinned the vectors under) agree on the wire — the split
//!    moved zero bytes across the version boundary too.
//! 2. **Decode → field-equality.** Both byte-forms of the fully populated
//!    frames decode via the generated readers into the expected field
//!    values (canonical trailing-zero truncation must be invisible to
//!    readers), across every `Content` union variant and the `isError`
//!    flag.
//!
//! The committed vectors are themselves SHA-256-pinned by
//! `schemas/leyline-net/v1/test-vectors/VECTORS.sha256`; re-vendoring
//! (copy schema + vectors afresh from LLO) is the only sanctioned way to
//! change them. See that dir's `PROVENANCE.md`.

use std::path::{Path, PathBuf};

use capnp::message::{Builder, HeapAllocator};

use crate::cloister_capnp::{content, manifest, tool_call, tool_result};

/// Build a single-segment message via `f`, then return its two committed
/// byte-forms: `reference` (plain `write_message`) and `canonical`
/// (`set_root_canonical` into a fresh builder). Mirrors LLO's vector
/// generator so rosary produces the identical bytes.
fn both<T, F>(f: F) -> (Vec<u8>, Vec<u8>)
where
    T: capnp::traits::Owned,
    F: FnOnce(&mut Builder<HeapAllocator>),
{
    let mut src = Builder::new_default();
    f(&mut src);
    let reference = capnp::serialize::write_message_to_words(&src);

    let mut canon = Builder::new_default();
    canon
        .set_root_canonical(
            src.get_root_as_reader::<<T as capnp::traits::Owned>::Reader<'_>>()
                .unwrap(),
        )
        .unwrap();
    let canonical = capnp::serialize::write_message_to_words(&canon);

    (reference, canonical)
}

/// The 12 leyline-net/v1 vectors, in the committed listing order. Values
/// mirror `schemas/leyline-net/v1/test-vectors/fixtures.capnp`.
fn all_vectors() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut v: Vec<(&'static str, Vec<u8>, Vec<u8>)> = Vec::new();

    let (r, c) = both::<manifest::Owned, _>(|b| {
        let mut m: manifest::Builder = b.init_root();
        m.set_sequence(42);
        m.set_public_key(&[0x11u8; 32]);
        m.set_signature(&[0x22u8; 64]);
        m.set_content_hash(&[0x33u8; 32]);
    });
    v.push(("manifest-canonical", r, c));

    let (r, c) = both::<manifest::Owned, _>(|b| {
        // contentHash = SHA-256 of the empty string.
        const EMPTY_SHA256: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        let mut m: manifest::Builder = b.init_root();
        m.set_sequence(0);
        m.set_public_key(&[0u8; 32]);
        m.set_signature(&[0u8; 64]);
        m.set_content_hash(&EMPTY_SHA256);
    });
    v.push(("manifest-zero-sequence", r, c));

    let (r, c) = both::<tool_call::Owned, _>(|b| {
        let mut t: tool_call::Builder = b.init_root();
        t.set_upstream_id("rosary");
        t.set_tool_name("rsry_status");
        t.set_arguments_json(b"{}");
    });
    v.push(("tool-call-basic", r, c));

    let (r, c) = both::<tool_call::Owned, _>(|b| {
        let mut t: tool_call::Builder = b.init_root();
        t.set_upstream_id("");
        t.set_tool_name("");
        // argumentsJson intentionally unset (defaulted empty Data).
    });
    v.push(("tool-call-empty", r, c));

    let (r, c) = both::<tool_call::Owned, _>(|b| {
        let mut t: tool_call::Builder = b.init_root();
        t.set_upstream_id("leyline");
        t.set_tool_name("lsp_hover");
        t.set_arguments_json(br#"{"col":5,"file":"/x/foo.rs","line":10}"#);
    });
    v.push(("tool-call-with-args", r, c));

    let (r, c) = both::<tool_result::Owned, _>(|b| {
        let mut t: tool_result::Builder = b.init_root();
        t.reborrow().init_content(0);
        t.set_is_error(false);
    });
    v.push(("tool-result-empty", r, c));

    let (r, c) = both::<tool_result::Owned, _>(|b| {
        let mut t: tool_result::Builder = b.init_root();
        t.reborrow().init_content(0);
        t.set_is_error(true);
    });
    v.push(("tool-result-error-empty", r, c));

    let (r, c) = both::<tool_result::Owned, _>(|b| {
        let t: tool_result::Builder = b.init_root();
        let c = t.init_content(1);
        c.get(0).init_body().set_text("hello world");
    });
    v.push(("tool-result-text", r, c));

    let (r, c) = both::<tool_result::Owned, _>(|b| {
        let t: tool_result::Builder = b.init_root();
        let c = t.init_content(1);
        c.get(0).init_body().set_resource(b"opaque");
    });
    v.push(("tool-result-resource", r, c));

    let (r, c) = both::<tool_result::Owned, _>(|b| {
        let t: tool_result::Builder = b.init_root();
        let c = t.init_content(1);
        let mut bin = c.get(0).init_body().init_binary();
        bin.set_data(&[0x89, 0x50, 0x4e, 0x47]);
        bin.set_mime_type("image/png");
    });
    v.push(("tool-result-binary", r, c));

    let (r, c) = both::<tool_result::Owned, _>(|b| {
        let t: tool_result::Builder = b.init_root();
        let mut c = t.init_content(4);
        c.reborrow().get(0).init_body().set_text("first");
        {
            let mut bin = c.reborrow().get(1).init_body().init_binary();
            bin.set_data(&[1, 2, 3]);
            bin.set_mime_type("application/octet-stream");
        }
        c.reborrow().get(2).init_body().set_resource(b"opaque2");
        c.reborrow().get(3).init_body().set_text("last");
    });
    v.push(("tool-result-mixed", r, c));

    let (r, c) = both::<tool_result::Owned, _>(|b| {
        let mut t: tool_result::Builder = b.init_root();
        {
            let c = t.reborrow().init_content(1);
            c.get(0)
                .init_body()
                .set_text("tool failed: missing 'file' argument");
        }
        t.set_is_error(true);
    });
    v.push(("tool-result-error-with-text", r, c));

    v
}

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/leyline-net/v1/test-vectors")
}

fn read_committed(form: &str, name: &str) -> Vec<u8> {
    let path = vectors_dir().join(form).join(format!("{name}.bin"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Encode direction: rosary's generated builders reproduce LLO's pinned
/// vectors byte-for-byte, both forms. This is the drift gate — a change
/// to any frame struct in `schemas/cloister.capnp` fails here.
#[test]
fn built_frames_byte_equal_pinned_vectors() {
    let built = all_vectors();
    assert_eq!(built.len(), 12, "expected 12 leyline-net/v1 vectors");
    for (name, reference, canonical) in built {
        for (form, bytes) in [("reference", &reference), ("canonical", &canonical)] {
            let committed = read_committed(form, name);
            assert_eq!(
                bytes.as_slice(),
                committed.as_slice(),
                "leyline-net WIRE DRIFT: rosary's schemas/cloister.capnp encodes {name} \
                 ({form} form) to {} bytes, but LLO's pinned vector is {} bytes. The frame \
                 structs are vendored from LLO's net.capnp and MUST stay byte-identical. \
                 If LLO deliberately bumped the spec, re-vendor schemas/cloister.capnp AND \
                 schemas/leyline-net/v1/test-vectors/ from LLO together (bead rosary-086973).",
                bytes.len(),
                committed.len(),
            );
        }
    }
}

/// Decode direction: both byte-forms of the fully populated Manifest
/// decode into the expected fields. Canonical truncation is invisible.
#[test]
fn manifest_vector_decodes_field_equal() {
    for form in ["reference", "canonical"] {
        let bytes = read_committed(form, "manifest-canonical");
        let mut slice: &[u8] = &bytes;
        let msg = capnp::serialize::read_message(&mut slice, capnp::message::ReaderOptions::new())
            .unwrap_or_else(|e| panic!("decode manifest-canonical ({form}): {e}"));
        let m: manifest::Reader = msg.get_root().expect("get_root Manifest");
        assert_eq!(m.get_sequence(), 42);
        assert_eq!(m.get_public_key().unwrap(), &[0x11u8; 32][..]);
        assert_eq!(m.get_signature().unwrap(), &[0x22u8; 64][..]);
        assert_eq!(m.get_content_hash().unwrap(), &[0x33u8; 32][..]);
    }
}

/// Decode direction: ToolCall, including the defaulted-Data case
/// (tool-call-empty's omitted argumentsJson reads as empty Data).
#[test]
fn tool_call_vectors_decode_field_equal() {
    for form in ["reference", "canonical"] {
        let bytes = read_committed(form, "tool-call-basic");
        let mut slice: &[u8] = &bytes;
        let msg = capnp::serialize::read_message(&mut slice, capnp::message::ReaderOptions::new())
            .unwrap();
        let t: tool_call::Reader = msg.get_root().expect("get_root ToolCall");
        assert_eq!(t.get_upstream_id().unwrap().to_str().unwrap(), "rosary");
        assert_eq!(t.get_tool_name().unwrap().to_str().unwrap(), "rsry_status");
        assert_eq!(t.get_arguments_json().unwrap(), b"{}");

        let bytes = read_committed(form, "tool-call-empty");
        let mut slice: &[u8] = &bytes;
        let msg = capnp::serialize::read_message(&mut slice, capnp::message::ReaderOptions::new())
            .unwrap();
        let t: tool_call::Reader = msg.get_root().expect("get_root ToolCall");
        assert_eq!(t.get_upstream_id().unwrap().to_str().unwrap(), "");
        assert_eq!(t.get_tool_name().unwrap().to_str().unwrap(), "");
        assert_eq!(
            t.get_arguments_json().unwrap(),
            b"",
            "omitted argumentsJson must read as empty Data in both byte forms",
        );
    }
}

/// Decode direction: ToolResult across every `Content` union variant
/// (text / binary / resource) via the mixed vector, plus the `isError`
/// flag via the error vector.
#[test]
fn tool_result_vectors_decode_field_equal() {
    for form in ["reference", "canonical"] {
        let bytes = read_committed(form, "tool-result-mixed");
        let mut slice: &[u8] = &bytes;
        let msg = capnp::serialize::read_message(&mut slice, capnp::message::ReaderOptions::new())
            .unwrap();
        let t: tool_result::Reader = msg.get_root().expect("get_root ToolResult");
        assert!(!t.get_is_error());
        let c = t.get_content().unwrap();
        assert_eq!(c.len(), 4);
        match c.get(0).get_body().which().unwrap() {
            content::body::Which::Text(txt) => {
                assert_eq!(txt.unwrap().to_str().unwrap(), "first")
            }
            _ => panic!("content[0] wrong variant"),
        }
        match c.get(1).get_body().which().unwrap() {
            content::body::Which::Binary(bin) => {
                let bin = bin.unwrap();
                assert_eq!(bin.get_data().unwrap(), &[1u8, 2, 3][..]);
                assert_eq!(
                    bin.get_mime_type().unwrap().to_str().unwrap(),
                    "application/octet-stream"
                );
            }
            _ => panic!("content[1] wrong variant"),
        }
        match c.get(2).get_body().which().unwrap() {
            content::body::Which::Resource(r) => assert_eq!(r.unwrap(), b"opaque2"),
            _ => panic!("content[2] wrong variant"),
        }
        match c.get(3).get_body().which().unwrap() {
            content::body::Which::Text(txt) => {
                assert_eq!(txt.unwrap().to_str().unwrap(), "last")
            }
            _ => panic!("content[3] wrong variant"),
        }

        let bytes = read_committed(form, "tool-result-error-empty");
        let mut slice: &[u8] = &bytes;
        let msg = capnp::serialize::read_message(&mut slice, capnp::message::ReaderOptions::new())
            .unwrap();
        let t: tool_result::Reader = msg.get_root().expect("get_root ToolResult");
        assert!(t.get_is_error());
        assert_eq!(t.get_content().unwrap().len(), 0);
    }
}
