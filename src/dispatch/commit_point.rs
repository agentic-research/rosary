//! Fail-closed commit point for externally visible mutations.

use anyhow::{Context, Result};

/// Proof object issued only after Rosary observes a successful verifier.
///
/// The private field prevents providers or agent output from fabricating the
/// receipt. External writes receive it only through
/// [`commit_external_mutation`].
pub struct VerificationReceipt {
    _sealed: (),
}

/// Run one mutation only after its verifier succeeds.
///
/// Both closures execute in the trusted Rosary host harness, never as sibling
/// commands in an agent-authored shell sequence.
pub fn commit_external_mutation(
    verifier: &mut dyn FnMut() -> Result<()>,
    mutation: &mut dyn FnMut(&VerificationReceipt) -> Result<()>,
) -> Result<()> {
    verifier().context("external mutation verification failed")?;
    let receipt = VerificationReceipt { _sealed: () };
    mutation(&receipt).context("verified external mutation failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_verification_never_invokes_mutation() {
        let mut mutations = 0;
        let result =
            commit_external_mutation(&mut || anyhow::bail!("checksum mismatch"), &mut |_| {
                mutations += 1;
                Ok(())
            });

        assert!(result.is_err());
        assert_eq!(mutations, 0);
    }

    #[test]
    fn successful_verification_invokes_mutation_once() {
        let mut mutations = 0;
        commit_external_mutation(&mut || Ok(()), &mut |_| {
            mutations += 1;
            Ok(())
        })
        .unwrap();

        assert_eq!(mutations, 1);
    }
}
