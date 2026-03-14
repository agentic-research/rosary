use rosary_crypto::cipher::{decrypt_field, encrypt_field};
use rosary_crypto::classify::{FieldVisibility, classify};
use rosary_crypto::key::{derive_key, generate_key};
use rosary_crypto::projection::project_bead;
use rosary_crypto::wasteland::bead_to_wanted;

#[test]
fn encrypt_decrypt_round_trip() {
    let key = generate_key();
    let bead_id = "loom-abc";
    let field = "description";
    let plaintext = b"This is a secret description of the bead.";

    let ciphertext = encrypt_field(field, bead_id, plaintext, &key).unwrap();
    assert_ne!(ciphertext, plaintext);

    let decrypted = decrypt_field(field, bead_id, &ciphertext, &key).unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn wrong_key_fails() {
    let key1 = generate_key();
    let key2 = generate_key();

    let ct = encrypt_field("description", "loom-abc", b"secret", &key1).unwrap();
    let result = decrypt_field("description", "loom-abc", &ct, &key2);
    assert!(result.is_err());
}

#[test]
fn wrong_field_name_fails() {
    let key = generate_key();
    let ct = encrypt_field("description", "loom-abc", b"secret", &key).unwrap();
    let result = decrypt_field("notes", "loom-abc", &ct, &key);
    assert!(result.is_err());
}

#[test]
fn wrong_bead_id_fails() {
    let key = generate_key();
    let ct = encrypt_field("description", "loom-abc", b"secret", &key).unwrap();
    let result = decrypt_field("description", "loom-xyz", &ct, &key);
    assert!(result.is_err());
}

#[test]
fn deterministic_nonce() {
    let key = generate_key();
    let ct1 = encrypt_field("description", "loom-abc", b"same data", &key).unwrap();
    let ct2 = encrypt_field("description", "loom-abc", b"same data", &key).unwrap();
    assert_eq!(ct1, ct2, "same input should produce same ciphertext");
}

#[test]
fn classify_public_fields() {
    assert_eq!(classify("id"), FieldVisibility::Public);
    assert_eq!(classify("title"), FieldVisibility::Public);
    assert_eq!(classify("status"), FieldVisibility::Public);
    assert_eq!(classify("priority"), FieldVisibility::Public);
    assert_eq!(classify("created_at"), FieldVisibility::Public);
}

#[test]
fn classify_private_fields() {
    assert_eq!(classify("description"), FieldVisibility::Private);
    assert_eq!(classify("owner"), FieldVisibility::Private);
    assert_eq!(classify("branch"), FieldVisibility::Private);
    assert_eq!(classify("notes"), FieldVisibility::Private);
    assert_eq!(classify("design"), FieldVisibility::Private);
}

#[test]
fn classify_unknown_defaults_private() {
    assert_eq!(classify("some_random_field"), FieldVisibility::Private);
}

#[test]
fn key_derivation_deterministic() {
    let master = generate_key();
    let k1 = derive_key(&master, "wasteland-v1");
    let k2 = derive_key(&master, "wasteland-v1");
    assert_eq!(k1, k2);
}

#[test]
fn key_derivation_different_contexts() {
    let master = generate_key();
    let k1 = derive_key(&master, "wasteland-v1");
    let k2 = derive_key(&master, "wasteland-v2");
    assert_ne!(k1, k2);
}

#[test]
fn project_bead_encrypts_private_leaves_public() {
    let key = generate_key();
    let bead = serde_json::json!({
        "id": "loom-test1",
        "title": "Fix the widget",
        "status": "open",
        "priority": "2",
        "issue_type": "bug",
        "description": "The widget is broken because of X",
        "owner": "jamestexas",
        "notes": "Internal implementation detail"
    });

    let projection = project_bead(&bead, &key).unwrap();

    // Public fields are cleartext
    assert_eq!(projection.id, "loom-test1");
    assert_eq!(projection.title, "Fix the widget");
    assert_eq!(projection.status.as_deref(), Some("open"));
    assert_eq!(projection.priority.as_deref(), Some("2"));

    // Private fields are encrypted (base64, not plaintext)
    let desc = projection.description.as_ref().unwrap();
    assert_ne!(desc, "The widget is broken because of X");
    assert!(desc.len() > 0);

    let owner = projection.owner.as_ref().unwrap();
    assert_ne!(owner, "jamestexas");

    // Decrypt and verify
    let desc_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, desc).unwrap();
    let decrypted = decrypt_field("description", "loom-test1", &desc_bytes, &key).unwrap();
    assert_eq!(
        String::from_utf8(decrypted).unwrap(),
        "The widget is broken because of X"
    );
}

#[test]
fn bead_to_wanted_mapping() {
    let key = generate_key();
    let bead = serde_json::json!({
        "id": "loom-test2",
        "title": "Add search feature",
        "status": "open",
        "priority": "1",
        "issue_type": "feature",
        "description": "Secret implementation plan"
    });

    let wanted = bead_to_wanted(&bead, &key, "jamestexas", Some("rosary")).unwrap();

    assert_eq!(wanted.id, "w-rr-loom-test2");
    assert_eq!(wanted.title, "Add search feature");
    assert_eq!(wanted.posted_by, "jamestexas");
    assert_eq!(wanted.status, "open");
    assert_eq!(wanted.project.as_deref(), Some("rosary"));
    assert_eq!(wanted.priority, Some(1));

    // Description should be None (encrypted, in metadata)
    assert!(wanted.description.is_none());

    // Metadata should contain encrypted_fields
    let meta = wanted.metadata.as_ref().unwrap();
    assert_eq!(meta["source"], "rosary");
    assert!(meta["encrypted_fields"]["description"].is_string());
}

#[test]
fn status_mapping() {
    let key = generate_key();

    let cases = vec![
        ("open", "open"),
        ("in_progress", "claimed"),
        ("closed", "completed"),
        ("done", "completed"),
        ("deferred", "withdrawn"),
    ];

    for (bead_status, expected_wl) in cases {
        let bead = serde_json::json!({
            "id": format!("loom-s-{}", bead_status),
            "title": "test",
            "status": bead_status,
        });
        let wanted = bead_to_wanted(&bead, &key, "test", None).unwrap();
        assert_eq!(
            wanted.status, expected_wl,
            "bead status '{}' should map to '{}'",
            bead_status, expected_wl
        );
    }
}
