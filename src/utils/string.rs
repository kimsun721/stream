use rand::{Rng, distr::Alphanumeric};
use sha2::{Digest, Sha256};

pub fn random_string(len: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

pub fn hash_string(string: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(string.as_bytes());
    hasher.finalize().into()
}
