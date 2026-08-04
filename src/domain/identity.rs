use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("identity HMAC secret must not be empty")]
    EmptySecret,
    #[error("email must contain exactly one non-empty @ separator")]
    InvalidEmail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashedEmail {
    pub email_masked: String,
    pub identity_hmac: String,
}

#[derive(Clone, Debug)]
pub struct IdentityHasher {
    secret: Vec<u8>,
}

impl IdentityHasher {
    pub fn new(secret: &[u8]) -> Result<Self, IdentityError> {
        if secret.is_empty() {
            return Err(IdentityError::EmptySecret);
        }
        Ok(Self {
            secret: secret.to_vec(),
        })
    }

    pub fn from_email(&self, email: &str) -> Result<HashedEmail, IdentityError> {
        let normalized = normalize_email(email)?;
        Ok(HashedEmail {
            email_masked: mask_email(&normalized),
            identity_hmac: self.fingerprint(&normalized),
        })
    }

    pub fn fingerprint(&self, stable_identity: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .expect("HMAC accepts keys of any non-zero length");
        mac.update(stable_identity.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

fn normalize_email(email: &str) -> Result<String, IdentityError> {
    let normalized = email.trim().to_lowercase();
    let mut parts = normalized.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if local.is_empty() || domain.is_empty() || parts.next().is_some() {
        return Err(IdentityError::InvalidEmail);
    }
    Ok(normalized)
}

fn mask_email(email: &str) -> String {
    let (local, domain) = email
        .split_once('@')
        .expect("mask_email is called only after email validation");
    let local_prefix = local.chars().next().unwrap_or('*');
    let domain_labels: Vec<&str> = domain.split('.').collect();
    let domain_prefix = domain_labels
        .first()
        .and_then(|label| label.chars().next())
        .unwrap_or('*');
    let suffix = if domain_labels.len() >= 2 {
        format!("{}.{}", domain_prefix, domain_labels.last().unwrap_or(&""))
    } else {
        format!("{}***", domain_prefix)
    };
    format!("{}***@{}***", local_prefix, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_masked_and_hmac_is_stable() {
        let hasher = IdentityHasher::new(b"phase1-test-secret").unwrap();
        let first = hasher.from_email(" Alice.Example@example.com ").unwrap();
        let second = hasher.from_email("alice.example@EXAMPLE.com").unwrap();

        assert_eq!(first, second);
        assert_eq!(first.email_masked, "a***@e.com***");
        assert_eq!(first.identity_hmac.len(), 64);
        assert!(!first.email_masked.contains("alice.example"));
        assert!(!first.email_masked.contains("@example.com"));
    }

    #[test]
    fn invalid_email_and_empty_secret_are_rejected() {
        assert!(matches!(
            IdentityHasher::new(b""),
            Err(IdentityError::EmptySecret)
        ));
        let hasher = IdentityHasher::new(b"secret").unwrap();
        assert_eq!(
            hasher.from_email("not-an-email"),
            Err(IdentityError::InvalidEmail)
        );
    }
}
