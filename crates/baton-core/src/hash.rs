//! SHA-256 en hexadecimal minúsculas. Lo usa el lock de plugins para saber, en cada arranque, si un
//! manifiesto instalado sigue siendo el que se revisó. Es el `sha2` de RustCrypto, no una
//! implementación propia.

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn one_changed_byte_changes_everything() {
        assert_ne!(
            sha256_hex(b"comando = \"a\""),
            sha256_hex(b"comando = \"b\"")
        );
    }
}
