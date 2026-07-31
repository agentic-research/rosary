//! Rosary's handoff-attestation POLICY layer over `leyline_envelope`'s
//! mechanism — the consumption half of the LLO DSSE hoist (rosary-30ae8c,
//! ley-line-open-319a08) that followed rosary-33670d's pre-hoist review.
//!
//! What stays here, per that review: the `.rsry-handoff-N.*` on-disk file
//! naming, the `rosary.dev` predicate type, and key CONFIGURATION (where the
//! signing key file lives). What left: every byte of signing IMPLEMENTATION
//! — `ed25519-dalek` is no longer a rosary dependency; `leyline_envelope`
//! composes over `leyline_sign::Ed25519RootSigner` instead, and this module
//! never touches key material beyond reading a 32-byte seed file into it.

use anyhow::{Context, Result, bail};
use leyline_envelope::{Ed25519RootSigner, Envelope, Statement, Subject, UnsignedStatement};
use std::path::Path;

/// in-toto `predicateType` for a rosary handoff. Rosary-specific policy —
/// not part of the hoisted mechanism, which takes this as a caller-supplied
/// `Statement::new` argument.
const PREDICATE_TYPE: &str = "https://rosary.dev/Handoff/v1";

/// Load an Ed25519 signing key from a raw 32-byte key file. Key
/// CONFIGURATION — `leyline_envelope` never takes raw key bytes in its own
/// API by design (its module docs), so loading the seed file into the
/// substrate signer type is the one place rosary still touches key bytes.
pub fn load_signing_key(path: &Path) -> Result<Ed25519RootSigner> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read signing key: {}", path.display()))?;
    if bytes.len() != 32 {
        bail!(
            "signing key must be 32 bytes (raw Ed25519 seed), got {}",
            bytes.len()
        );
    }
    let arr: [u8; 32] = bytes.try_into().unwrap();
    Ok(Ed25519RootSigner::from_seed(&arr))
}

/// Build the in-toto Statement for a handoff.
///
/// `handoff_disk_bytes` MUST be the exact bytes written to disk for the
/// handoff file — the subject digest hashes exactly what an external
/// observer would get by hashing the file directly. `Handoff::write_to`
/// pretty-prints; signing over a freshly re-serialized value would produce a
/// digest that doesn't match the file (rosary-33670d Q4's compact-payload-
/// vs-pretty-disk-digest gotcha — the two byte streams are deliberately
/// distinct inputs, never derived from one another).
pub fn handoff_statement(
    handoff_predicate: &serde_json::Value,
    handoff_filename: &str,
    handoff_disk_bytes: &[u8],
) -> Statement {
    Statement::new(
        vec![Subject::sha256_of(handoff_filename, handoff_disk_bytes)],
        PREDICATE_TYPE,
        handoff_predicate.clone(),
    )
}

fn handoff_statement_from_file(
    handoff_path: &Path,
    handoff_predicate: &serde_json::Value,
) -> Result<Statement> {
    let bytes = std::fs::read(handoff_path)
        .with_context(|| format!("read handoff: {}", handoff_path.display()))?;
    let filename = handoff_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("handoff.json")
        .to_string();
    Ok(handoff_statement(handoff_predicate, &filename, &bytes))
}

/// Read a handoff file from disk and produce a signed envelope for it.
/// Use this when the handoff has just been written by `Handoff::write_to`.
pub fn wrap_handoff_from_file(
    handoff_path: &Path,
    handoff_predicate: &serde_json::Value,
    signer: &Ed25519RootSigner,
) -> Result<Envelope> {
    let statement = handoff_statement_from_file(handoff_path, handoff_predicate)?;
    Ok(Envelope::sign(&statement, signer))
}

/// Read a handoff file from disk and produce its unsigned in-toto Statement.
pub fn unsigned_handoff_statement_from_file(
    handoff_path: &Path,
    handoff_predicate: &serde_json::Value,
) -> Result<UnsignedStatement> {
    Ok(handoff_statement_from_file(handoff_path, handoff_predicate)?.into())
}

/// Write a DSSE envelope alongside a handoff file.
/// Envelope path: `{work_dir}/.rsry-handoff-{phase}.dsse.json`
///
/// Unlike the pre-hoist dsse.rs, there is no "refuse an unsigned envelope"
/// check here — `leyline_envelope::Envelope` cannot represent an empty
/// signature set at all (parse, don't validate); the type itself makes that
/// state unrepresentable.
pub fn write_envelope(work_dir: &Path, phase: u32, envelope: &Envelope) -> Result<()> {
    let path = work_dir.join(format!(".rsry-handoff-{phase}.dsse.json"));
    std::fs::write(&path, envelope.to_json_vec())
        .with_context(|| format!("write envelope: {}", path.display()))
}

/// Read a DSSE envelope from disk.
#[allow(dead_code)] // Consumed by Phase 2 observation cert verification path.
pub fn read_envelope(work_dir: &Path, phase: u32) -> Result<Envelope> {
    let path = work_dir.join(format!(".rsry-handoff-{phase}.dsse.json"));
    let bytes =
        std::fs::read(&path).with_context(|| format!("read envelope: {}", path.display()))?;
    Envelope::from_json_slice(&bytes).context("parse envelope")
}

/// Write an explicitly unsigned forensic in-toto Statement alongside a handoff.
/// Statement path: `{work_dir}/.rsry-handoff-{phase}.intoto.json`
pub fn write_unsigned_statement(
    work_dir: &Path,
    phase: u32,
    statement: &UnsignedStatement,
) -> Result<()> {
    let path = work_dir.join(format!(".rsry-handoff-{phase}.intoto.json"));
    std::fs::write(&path, statement.to_json_vec())
        .with_context(|| format!("write statement: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest as _;

    fn gen_key() -> Ed25519RootSigner {
        Ed25519RootSigner::from_seed(&[7u8; 32])
    }

    fn sample_handoff_json() -> serde_json::Value {
        serde_json::json!({
            "phase": 0,
            "from_agent": "dev-agent",
            "bead_id": "rosary-test",
            "summary": "Fixed the thing."
        })
    }

    fn disk_bytes(json: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec_pretty(json).unwrap()
    }

    #[test]
    fn handoff_statement_is_valid_in_toto() {
        let json = sample_handoff_json();
        let stmt = handoff_statement(&json, ".rsry-handoff-0.json", &disk_bytes(&json));
        assert_eq!(stmt.predicate_type(), PREDICATE_TYPE);
        assert_eq!(stmt.subject()[0].name(), ".rsry-handoff-0.json");
        assert!(stmt.subject()[0].sha256_hex().is_some());
    }

    #[test]
    fn subject_digest_matches_handoff_disk_bytes() {
        // The in-toto subject digest MUST hash the exact bytes that live on
        // disk so external observers can verify by hashing the file directly.
        let json = sample_handoff_json();
        let disk = disk_bytes(&json);
        let stmt = handoff_statement(&json, ".rsry-handoff-0.json", &disk);

        let expected = hex::encode(sha2::Sha256::digest(&disk));
        assert_eq!(stmt.subject()[0].sha256_hex(), Some(expected.as_str()));
    }

    #[test]
    fn load_signing_key_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let seed = [3u8; 32];
        let path = tmp.path().join("signing.key");
        std::fs::write(&path, seed).unwrap();
        let loaded = load_signing_key(&path).unwrap();
        assert_eq!(
            loaded.verifying_key(),
            Ed25519RootSigner::from_seed(&seed).verifying_key()
        );
    }

    #[test]
    fn load_signing_key_wrong_length_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("bad.key");
        std::fs::write(&path, b"tooshort").unwrap();
        assert!(load_signing_key(&path).is_err());
    }

    #[test]
    fn wrap_handoff_from_file_hashes_actual_disk_bytes_and_verifies() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key = gen_key();
        let json = sample_handoff_json();

        let handoff_path = tmp.path().join(".rsry-handoff-0.json");
        std::fs::write(&handoff_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let envelope = wrap_handoff_from_file(&handoff_path, &json, &key).unwrap();
        let stmt = envelope.verify(&key.verifying_key()).unwrap();
        assert_eq!(stmt.predicate()["bead_id"], "rosary-test");

        let file_bytes = std::fs::read(&handoff_path).unwrap();
        let expected = hex::encode(sha2::Sha256::digest(&file_bytes));
        assert_eq!(stmt.subject()[0].sha256_hex(), Some(expected.as_str()));
        assert_eq!(stmt.subject()[0].name(), ".rsry-handoff-0.json");
    }

    #[test]
    fn write_read_envelope_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key = gen_key();
        let json = sample_handoff_json();
        let handoff_path = tmp.path().join(".rsry-handoff-0.json");
        std::fs::write(&handoff_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let env = wrap_handoff_from_file(&handoff_path, &json, &key).unwrap();
        write_envelope(tmp.path(), 0, &env).unwrap();
        let loaded = read_envelope(tmp.path(), 0).unwrap();
        assert_eq!(loaded, env);

        let stmt = loaded.verify(&key.verifying_key()).unwrap();
        assert_eq!(stmt.predicate()["bead_id"], "rosary-test");
    }

    #[test]
    fn write_unsigned_statement_uses_distinct_in_toto_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let json = sample_handoff_json();
        let handoff_path = tmp.path().join(".rsry-handoff-0.json");
        std::fs::write(&handoff_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let statement = unsigned_handoff_statement_from_file(&handoff_path, &json).unwrap();
        write_unsigned_statement(tmp.path(), 0, &statement).unwrap();

        let statement_path = tmp.path().join(".rsry-handoff-0.intoto.json");
        let written = Statement::from_json_slice(&std::fs::read(&statement_path).unwrap()).unwrap();
        assert_eq!(written.predicate_type(), PREDICATE_TYPE);
        assert!(!tmp.path().join(".rsry-handoff-0.dsse.json").exists());
    }
}
